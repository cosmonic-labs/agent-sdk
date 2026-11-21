//! The scripted host's driver side: one instance per call, as a host does,
//! with only the host's session surviving between calls; the events a run or
//! compaction streams, read to the end (or dropped after a limit, which
//! cancels), and its outcome.

use std::pin::Pin;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use tokio::sync::{mpsc, oneshot};
use wasmtime::component::{
    Accessor, Component, FutureConsumer, FutureReader, Linker, Source, StreamConsumer, StreamReader, StreamResult,
};
use wasmtime::{Config, Engine, OptLevel, Store, StoreContextMut};

use crate::host::types::{AssistantContent, ToolOutput, UserMessage, UserPart};
use crate::host::run_types::{RunError, RunEvent, RunOptions, RunOutcome, StartError, StepContent};
use crate::host::{self, Agent, AgentUnderTest, Shared, State};

/// Reads the run's events; with a limit, drops the stream after that many,
/// which is how a driver cancels a run.
pub(crate) struct EventSink {
    tx: mpsc::UnboundedSender<RunEvent>,
    limit: Option<usize>,
    agent: Shared,
}

impl<D> StreamConsumer<D> for EventSink {
    type Item = RunEvent;
    fn poll_consume(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        mut store: StoreContextMut<D>,
        mut source: Source<'_, RunEvent>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let this = self.get_mut();
        while source.remaining(&mut store) > 0 {
            if this.limit == Some(0) {
                return Poll::Ready(Ok(StreamResult::Dropped));
            }
            let mut one = None;
            source.read(&mut store, &mut one)?;
            if let Some(ev) = one {
                // Heartbeats are safe to ignore: drivers skip them, and so do
                // the suites, which never count them against `limit`.
                if matches!(ev, RunEvent::Other(_)) {
                    continue;
                }
                if matches!(ev, RunEvent::Plan(_)) {
                    let mut agent = this.agent.lock().unwrap();
                    let len = agent.entries.len();
                    agent.plan_events_at.push(len);
                }
                let _ = this.tx.send(ev);
                this.limit = this.limit.map(|n| n - 1);
            }
        }
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

pub(crate) type Outcome = std::result::Result<RunOutcome, RunError>;
pub(crate) struct OutcomeSink(Arc<Mutex<Option<oneshot::Sender<Outcome>>>>);

impl<D> FutureConsumer<D> for OutcomeSink {
    type Item = Outcome;
    fn poll_consume(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<'_, Outcome>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<()>> {
        let mut one = None;
        source.read(store, &mut one)?;
        if let (Some(v), Some(tx)) = (one, self.0.lock().unwrap().take()) {
            let _ = tx.send(v);
        }
        Poll::Ready(Ok(()))
    }
}

pub(crate) struct Fixture {
    engine: Engine,
    component: Component,
    linker: Linker<State>,
    agent: Shared,
}

pub(crate) enum Ran {
    Started { events: Vec<RunEvent>, outcome: Outcome },
    Refused(StartError),
}

/// The engine and compiled component for `path`. Compiling a large component
/// takes minutes, so each path is compiled once per process and every
/// fixture of it shares the result; fixtures stay apart in their own stores.
fn compile_cached(path: &str) -> Result<(Engine, Component)> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Engine, Component)>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some(hit) = cache.get(path) {
        return Ok(hit.clone());
    }
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    // The suites run each component briefly: compile time is what counts.
    config.cranelift_opt_level(OptLevel::None);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, path).map_err(|e| anyhow::anyhow!("loading {path}: {e}"))?;
    cache.insert(path.to_owned(), (engine.clone(), component.clone()));
    Ok((engine, component))
}

impl Fixture {
    pub(crate) fn new(path: &str) -> Result<Self> {
        let (engine, component) = compile_cached(path)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi::p3::add_to_linker(&mut linker)?;
        wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
        host::add_to_linker(&mut linker)?;
        Ok(Self { engine, component, linker, agent: Arc::new(Mutex::new(Agent::default())) })
    }

    pub(crate) fn agent(&self) -> std::sync::MutexGuard<'_, Agent> {
        self.agent.lock().unwrap()
    }

    pub(crate) async fn run(&self, id: &str, text: &str, schema: Option<&str>) -> Result<Ran> {
        self.run_reading(id, text, schema, None).await
    }

    pub(crate) async fn run_reading(&self, id: &str, text: &str, schema: Option<&str>, limit: Option<usize>) -> Result<Ran> {
        let mut store = Store::new(&self.engine, State::new(self.agent.clone()));
        let agent = AgentUnderTest::instantiate_async(&mut store, &self.component, &self.linker).await?;
        let message = UserMessage { content: vec![UserPart::Text(text.into())], name: None };
        let options = RunOptions {
            run_id: id.into(),
            instructions: Some("Be brief.".into()),
            schema: schema.map(str::to_string),
            max_steps: Some(8),
            max_total_tokens: None,
            max_duration: Some(60_000_000_000),
            effort: None,
            trace_parent: None,
        };
        let ran = tokio::time::timeout(
            Duration::from_secs(60),
            store.run_concurrent(async |acc| -> Result<Ran> {
                let started = agent.cosmonic_agent_run().call_run(acc, message, options).await?;
                let (events, outcome) = match started {
                    Ok(p) => p,
                    Err(e) => return Ok(Ran::Refused(e)),
                };
                drain(acc, events, outcome, limit, self.agent.clone()).await
            }),
        )
        .await
        .context("the run timed out")???;
        Ok(ran)
    }
}

impl Fixture {
    pub(crate) fn store(&self) -> Store<State> {
        Store::new(&self.engine, State::new(self.agent.clone()))
    }

    /// One call of the `compact` export on a fresh instance, read to the end.
    pub(crate) async fn compact(&self, id: &str, focus: Option<&str>, options: impl FnOnce(&mut RunOptions)) -> Result<Ran> {
        self.compact_reading(id, focus, options, None).await
    }

    /// The same, with a driver that drops the events after `limit` of them
    /// (`Some(0)`: at the first write).
    pub(crate) async fn compact_reading(
        &self,
        id: &str,
        focus: Option<&str>,
        options: impl FnOnce(&mut RunOptions),
        limit: Option<usize>,
    ) -> Result<Ran> {
        let mut store = self.store();
        let agent = AgentUnderTest::instantiate_async(&mut store, &self.component, &self.linker).await?;
        let mut opts = compact_options(id);
        options(&mut opts);
        let focus = focus.map(str::to_string);
        tokio::time::timeout(
            Duration::from_secs(60),
            store.run_concurrent(async |acc| -> Result<Ran> {
                let started = agent.cosmonic_agent_compact().call_compact(acc, focus, opts).await?;
                let (events, outcome) = match started {
                    Ok(p) => p,
                    Err(e) => return Ok(Ran::Refused(e)),
                };
                drain(acc, events, outcome, limit, self.agent.clone()).await
            }),
        )
        .await
        .context("the compaction timed out")??
    }

    /// Start a run on one instance and, while it is in progress (its first
    /// event not yet read), ask the same instance to compact. Then read the
    /// run to its end.
    pub(crate) async fn compact_during_run(&self, run: &str, id: &str) -> Result<(std::result::Result<(), StartError>, Ran)> {
        let mut store = self.store();
        let agent = AgentUnderTest::instantiate_async(&mut store, &self.component, &self.linker).await?;
        let message = UserMessage { content: vec![UserPart::Text("a run in progress".into())], name: None };
        let mut options = compact_options(run);
        options.max_steps = Some(8);
        tokio::time::timeout(
            Duration::from_secs(60),
            store.run_concurrent(async |acc| -> Result<(std::result::Result<(), StartError>, Ran)> {
                let (events, outcome) = match agent.cosmonic_agent_run().call_run(acc, message, options).await? {
                    Ok(p) => p,
                    Err(e) => bail!("the run did not start: {e:?}"),
                };
                // A refused call must leave the original run holding its slot.
                ensure!(matches!(
                    agent.cosmonic_agent_compact().call_compact(acc, None, compact_options(id)).await?,
                    Err(StartError::Busy)
                ), "first overlapping compaction should be busy");
                let compacting = match agent.cosmonic_agent_compact().call_compact(acc, None, compact_options(id)).await? {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e),
                };
                Ok((compacting, drain(acc, events, outcome, None, self.agent.clone()).await?))
            }),
        )
        .await
        .context("the run timed out")??
    }
}

/// Read what a started run or compaction streams, the driver dropping the
/// events after `limit` of them, and then its outcome.
async fn drain(
    acc: &Accessor<State>,
    events: StreamReader<RunEvent>,
    outcome: FutureReader<Outcome>,
    limit: Option<usize>,
    agent: Shared,
) -> Result<Ran> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (otx, orx) = oneshot::channel();
    acc.with(|mut s| -> Result<()> {
        events.pipe(&mut s, EventSink { tx, limit, agent })?;
        outcome.pipe(&mut s, OutcomeSink(Arc::new(Mutex::new(Some(otx)))))?;
        Ok(())
    })?;
    let mut seen = Vec::new();
    while let Some(ev) = rx.recv().await {
        seen.push(ev);
    }
    let outcome = orx.await.context("the outcome future closed without a value")?;
    Ok(Ran::Started { events: seen, outcome })
}

pub(crate) fn compact_options(id: &str) -> RunOptions {
    RunOptions {
        run_id: id.into(),
        instructions: Some("Be brief.".into()),
        schema: None,
        max_steps: None,
        max_total_tokens: None,
        max_duration: Some(60_000_000_000),
        effort: None,
        trace_parent: None,
    }
}

pub(crate) fn started(ran: Ran) -> Result<(Vec<RunEvent>, RunOutcome)> {
    match ran {
        Ran::Started { events, outcome: Ok(outcome) } => Ok((events, outcome)),
        Ran::Started { outcome: Err(e), .. } => bail!("the run failed: {e:?}"),
        Ran::Refused(e) => bail!("the run did not start: {e:?}"),
    }
}

pub(crate) fn reply_text(outcome: &RunOutcome) -> String {
    outcome
        .reply
        .iter()
        .filter_map(|p| match p {
            host::run_types::ReplyPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

pub(crate) fn describe(ev: &RunEvent) -> String {
    match ev {
        RunEvent::TextDelta(t) => format!("text-delta {t:?}"),
        RunEvent::ReasoningDelta(t) => format!("reasoning-delta {t:?}"),
        RunEvent::Step(s) => match &s.content {
            StepContent::Assistant(m) => format!(
                "step assistant [{}]",
                m.content
                    .iter()
                    .map(|p| match &p.content {
                        AssistantContent::Text(t) => format!("text {t:?}"),
                        AssistantContent::Reasoning(r) => format!("reasoning {:?}", r.summary),
                        AssistantContent::ToolUse(c) => format!("tool-use {}({:?})", c.name, c.arguments),
                        AssistantContent::Attachment(a) => format!("attachment {}", a.media_type),
                        AssistantContent::Refusal(t) => format!("refusal {t:?}"),
                        AssistantContent::Other(o) => format!("other {}", o.kind),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            StepContent::Tool(results) => format!(
                "step tool [{}]",
                results
                    .iter()
                    .map(|r| format!(
                        "{} error={} {:?}",
                        r.call_id,
                        r.is_error,
                        r.output.iter().filter_map(|o| match o { ToolOutput::Text(t) => Some(t.as_str()), _ => None }).collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            StepContent::Error(t) => format!("step error {t:?}"),
            StepContent::User(_) | StepContent::Other(_) => "step other".into(),
        },
        RunEvent::Counts(c) => format!("counts steps={} tools={} usage={:?}/{:?}", c.steps, c.tool_calls, c.usage.prompt_tokens, c.usage.completion_tokens),
        RunEvent::Compacted(c) => format!("compacted {:?}", c.trigger),
        RunEvent::Plan(entries) => format!(
            "plan [{}]",
            entries.iter().map(|e| format!("{:?} {:?}", e.status, e.content)).collect::<Vec<_>>().join(", ")
        ),
        RunEvent::Other(_) => "other".into(),
    }
}
