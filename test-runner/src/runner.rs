//! Runs one test: hosts the agent with the test's model, tools and grants,
//! drives one run through its `run` export, and returns what happened.
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, oneshot};
use wasmtime::component::{Component, FutureConsumer, Source, StreamConsumer, StreamResult};
use wasmtime::{Config, Engine, OptLevel, Store, StoreContextMut};

use crate::host::{self, Host, Offered, Scripted, Session, ToolComponent, bindings};
use crate::record::Record;
use crate::spec::{Folder, Test};
use crate::tools::{list_component, stub_def};
use host::run_types::{RunError, RunEvent, RunOptions, RunOutcome, StartError};
use host::types::{UserMessage, UserPart};

pub type Outcome = std::result::Result<RunOutcome, RunError>;

pub enum Ran {
    Started { outcome: Outcome },
    Refused(StartError),
}

/// Everything a finished test is checked against.
pub struct Finished {
    pub ran: Ran,
    pub record: Record,
    pub journal: Vec<String>,
}

struct EventSink(mpsc::UnboundedSender<RunEvent>);

impl<D> StreamConsumer<D> for EventSink {
    type Item = RunEvent;
    fn poll_consume(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        mut store: StoreContextMut<D>,
        mut source: Source<'_, RunEvent>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        while source.remaining(&mut store) > 0 {
            let mut one = None;
            source.read(&mut store, &mut one)?;
            if let Some(ev) = one {
                let _ = self.0.send(ev);
            }
        }
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

struct OutcomeSink(Arc<Mutex<Option<oneshot::Sender<Outcome>>>>);

impl<D> FutureConsumer<D> for OutcomeSink {
    type Item = Outcome;
    fn poll_consume(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        store: StoreContextMut<D>,
        mut source: Source<'_, Outcome>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<()>> {
        let mut one = None;
        source.read(store, &mut one)?;
        let tx = self.0.lock().ok().and_then(|mut tx| tx.take());
        if let (Some(v), Some(tx)) = (one, tx) {
            let _ = tx.send(v);
        }
        Poll::Ready(Ok(()))
    }
}

/// Compiled components, shared by every test in a process: compiling is the
/// slow part.
fn compile(engine: &Engine, path: &Path) -> Result<Component> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Component>>> = OnceLock::new();
    let key = path
        .canonicalize()
        .with_context(|| format!("no component at {}", path.display()))?;
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.get(&key) {
        return Ok(c.clone());
    }
    let c = Component::from_file(engine, &key)
        .map_err(|e| anyhow::anyhow!("loading {}: {e}", key.display()))?;
    cache.insert(key, c.clone());
    Ok(c)
}

pub fn engine() -> Result<Engine> {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    if let Some(e) = ENGINE.get() {
        return Ok(e.clone());
    }
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    // Each component runs briefly: compile time is what counts.
    config.cranelift_opt_level(OptLevel::None);
    // Lets a deadline stop a guest stuck in a loop, which a timeout on the
    // future alone cannot.
    config.epoch_interruption(true);
    let engine = Engine::new(&config)?;
    let engine = ENGINE.get_or_init(|| engine).clone();
    static TICKER: OnceLock<()> = OnceLock::new();
    TICKER.get_or_init(|| {
        let e = engine.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(TICK_MS));
                e.increment_epoch();
            }
        });
    });
    Ok(engine)
}

const TICK_MS: u64 = 100;

/// Trap any guest code in `store` still running `seconds` (plus a margin)
/// from now.
pub fn set_deadline<T>(store: &mut Store<T>, seconds: u64) {
    store.set_epoch_deadline(seconds.saturating_add(5).saturating_mul(1000 / TICK_MS));
}

fn folders(test: &Test, list: &[Folder]) -> Vec<(PathBuf, Folder)> {
    list.iter()
        .map(|f| (test.resolve(&f.path), f.clone()))
        .collect()
}

/// Run `test`, with `agent` in place of the file's component when given.
pub async fn run(test: &Test, agent: Option<&Path>) -> Result<Finished> {
    let engine = engine()?;
    let linker = host::linker(&engine)?;
    let agent_path = agent
        .map(Path::to_path_buf)
        .unwrap_or_else(|| test.resolve(&test.agent.component));
    let agent_component = compile(&engine, &agent_path)?;

    let mut next_id = 0;
    let replies: VecDeque<Scripted> = test
        .model
        .replies
        .iter()
        .map(|r| Scripted {
            reply: r.clone(),
            ids: r
                .tool_calls
                .iter()
                .map(|c| {
                    next_id += 1;
                    c.id.clone().unwrap_or_else(|| format!("call-{next_id}"))
                })
                .collect(),
        })
        .collect();
    let host = Arc::new(Host {
        engine: engine.clone(),
        linker,
        tool_linker: host::tool_linker(&engine)?,
        alias: test.model.alias.clone(),
        timeout: test.run.timeout,
        script_len: replies.len(),
        replies: Mutex::new(replies),
        tools: OnceLock::new(),
        granted: test.grants.tools.clone(),
        stub_calls: Mutex::default(),
        http: test.http.clone(),
        rec: Mutex::new(Record::default()),
        session: Mutex::new(Session::default()),
    });

    let mut offered = Vec::new();
    for t in &test.tools {
        if let Some(path) = &t.component {
            let path = test.resolve(path);
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "tool".into());
            let tool = Arc::new(ToolComponent {
                name,
                component: compile(&engine, &path)?,
                hosts: t.hosts.clone(),
                folders: folders(test, &t.folders),
            });
            for def in list_component(&host, &tool).await? {
                offered.push(Offered {
                    def,
                    source: host::Source::Component(Arc::clone(&tool)),
                });
            }
        } else {
            offered.push(Offered {
                def: stub_def(t),
                source: host::Source::Stub(t.results.clone()),
            });
        }
    }
    let mut seen = std::collections::HashSet::new();
    for o in &offered {
        if !seen.insert(o.def.name.clone()) {
            bail!("two tools are named {}", o.def.name);
        }
    }
    // A granted name nothing offers is a typo that would grant nothing.
    for name in test.grants.tools.iter().flatten() {
        if !seen.contains(name) {
            bail!("grants.tools names {name}, which no tool offers");
        }
    }
    let _ = host.tools.set(offered);

    let state = host.state(
        "agent",
        &test.grants.hosts,
        &folders(test, &test.grants.folders),
    )?;
    let mut store = Store::new(&engine, state);
    set_deadline(&mut store, test.run.timeout);
    let agent =
        bindings::AgentUnderTest::instantiate_async(&mut store, &agent_component, &host.linker)
            .await
            .map_err(|e| anyhow::anyhow!("instantiating the agent: {e:?}"))?;
    let message = UserMessage {
        content: vec![UserPart::Text(test.run.message.clone())],
        name: None,
    };
    let options = RunOptions {
        run_id: test.run.id.clone(),
        instructions: test.run.instructions.clone(),
        schema: test.run.schema.as_ref().map(|s| s.to_string()),
        max_steps: Some(test.run.max_steps),
        max_total_tokens: None,
        max_duration: Some(test.run.timeout.saturating_mul(1_000_000_000)),
        effort: None,
        trace_parent: None,
    };
    let ran = tokio::time::timeout(
        // A little past the run's own max-duration, so the agent's limit
        // fires first.
        Duration::from_secs(test.run.timeout.saturating_add(5)),
        store.run_concurrent(async |acc| -> Result<Ran> {
            let (events, outcome) = match agent
                .cosmonic_agent_run()
                .call_run(acc, message, options)
                .await?
            {
                Ok(p) => p,
                Err(e) => return Ok(Ran::Refused(e)),
            };
            let (tx, mut rx) = mpsc::unbounded_channel();
            let (otx, orx) = oneshot::channel();
            acc.with(|mut s| -> Result<()> {
                events.pipe(&mut s, EventSink(tx))?;
                outcome.pipe(&mut s, OutcomeSink(Arc::new(Mutex::new(Some(otx)))))?;
                Ok(())
            })?;
            while rx.recv().await.is_some() {}
            let outcome = orx
                .await
                .context("the outcome future closed without a value")?;
            Ok(Ran::Started { outcome })
        }),
    )
    .await
    .with_context(|| format!("the run did not end within {}s", test.run.timeout))???;
    drop(store);

    let journal = host
        .session()
        .entries
        .iter()
        .map(|e| e.kind.clone())
        .collect();
    let record = std::mem::take(&mut *host.record());
    Ok(Finished {
        ran,
        record,
        journal,
    })
}
