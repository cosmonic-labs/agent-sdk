//! The host the runner plays: the bindings, the per-store state, and the
//! shared `Host` every store of a test points at (the scripted model, the
//! tools, the session and the record).
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use wasmtime::Engine;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::gate::Gate;
use crate::record::{Record, Refusal, Refused};
use crate::spec;

pub mod bindings {
    wasmtime::component::bindgen!({
        path: "wit-host",
        world: "agent-under-test",
        imports: { default: trappable },
        exports: { default: async },
        with: {
            "cosmonic:agent/inference-types@0.4.2.model": super::ModelHandle,
            "wasi:clocks": wasmtime_wasi::p3::bindings::clocks,
        },
        require_store_data_send: true,
    });
}

pub mod tool_bindings {
    wasmtime::component::bindgen!({
        path: "wit-host",
        world: "tool-under-test",
        exports: { default: async },
        with: {
            "cosmonic:agent/inference-types": super::bindings::cosmonic::agent::inference_types,
            "cosmonic:agent/tool-types": super::bindings::cosmonic::agent::tool_types,
        },
        require_store_data_send: true,
    });
}

pub use bindings::cosmonic::agent::{
    chat, inference_types as types, models, run_types, session, tool_types, tools,
};

pub struct ModelHandle;

/// A tool the host knows: offered to the agent if granted.
pub struct Offered {
    pub def: tool_types::ToolDef,
    pub source: Source,
}

pub enum Source {
    Stub(Vec<spec::StubResult>),
    Component(Arc<ToolComponent>),
}

pub struct ToolComponent {
    pub name: String,
    pub component: Component,
    pub hosts: Vec<String>,
    pub folders: Vec<(PathBuf, spec::Folder)>,
}

/// One scripted reply, with the ids its tool calls get.
pub struct Scripted {
    pub reply: spec::Reply,
    pub ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: u64,
    pub kind: String,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct Session {
    pub entries: Vec<Entry>,
    pub state: Option<(Vec<u8>, String, Option<u64>)>,
    pub seq: u64,
    pub versions: u64,
    pub commits: HashMap<String, session::CommitResult>,
}

/// Everything a test's stores share.
pub struct Host {
    pub engine: Engine,
    pub linker: Linker<State>,
    /// Tool components get WASI and the shared types, never the model, the
    /// session or other tools.
    pub tool_linker: Linker<State>,
    pub alias: String,
    /// The test's time limit in seconds, for every store.
    pub timeout: u64,
    pub replies: Mutex<VecDeque<Scripted>>,
    pub script_len: usize,
    /// Every tool the test declares, set once tool components are listed.
    pub tools: OnceLock<Vec<Offered>>,
    /// Granted tool names; `None` grants every declared tool.
    pub granted: Option<Vec<String>>,
    pub stub_calls: Mutex<HashMap<String, usize>>,
    pub http: Vec<spec::HttpStub>,
    pub rec: Mutex<Record>,
    pub session: Mutex<Session>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Host {
    pub fn record(&self) -> MutexGuard<'_, Record> {
        lock(&self.rec)
    }

    pub fn session(&self) -> MutexGuard<'_, Session> {
        lock(&self.session)
    }

    pub fn replies(&self) -> MutexGuard<'_, VecDeque<Scripted>> {
        lock(&self.replies)
    }

    pub fn stub_calls(&self) -> MutexGuard<'_, HashMap<String, usize>> {
        lock(&self.stub_calls)
    }

    pub fn tools(&self) -> &[Offered] {
        self.tools.get().map(Vec::as_slice).unwrap_or_default()
    }

    pub fn is_granted(&self, name: &str) -> bool {
        self.granted
            .as_ref()
            .is_none_or(|g| g.iter().any(|n| n == name))
    }

    /// The state for a store of component `by`, with its own grants.
    pub fn state(
        self: &Arc<Self>,
        by: &str,
        hosts: &[String],
        folders: &[(PathBuf, spec::Folder)],
    ) -> anyhow::Result<State> {
        let mut wasi = WasiCtxBuilder::new();
        wasi.inherit_stderr();
        for (path, f) in folders {
            let perms = if f.writable {
                FsPerms::ReadWrite
            } else {
                FsPerms::ReadOnly
            };
            wasi.preopened_dir(path, &f.guest, perms)
                .map_err(|e| anyhow::anyhow!("opening folder {}: {e}", path.display()))?;
        }
        // Sockets are never granted. Turning them on with a check that always
        // refuses is what lets the runner record an attempt.
        let host = Arc::clone(self);
        let who = by.to_string();
        wasi.allow_tcp(true)
            .allow_udp(true)
            .socket_addr_check(move |addr, _use| {
                host.record().refused.push(Refused {
                    by: who.clone(),
                    what: Refusal::Socket {
                        addr: addr.to_string(),
                    },
                });
                Box::pin(async { false })
            });
        Ok(State {
            table: ResourceTable::new(),
            wasi: wasi.build(),
            http: WasiHttpCtx::new(),
            gate: Gate {
                by: by.into(),
                hosts: hosts.to_vec(),
                host: Arc::clone(self),
            },
            host: Arc::clone(self),
        })
    }
}

pub struct State {
    pub table: ResourceTable,
    pub wasi: WasiCtx,
    pub http: WasiHttpCtx,
    pub gate: Gate,
    pub host: Arc<Host>,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for State {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.gate,
        }
    }
}

/// WASI (filesystem and sockets behind the grants), HTTP behind the gate,
/// and the `cosmonic:agent` types: what a tool component is linked with.
fn base(engine: &Engine) -> wasmtime::Result<Linker<State>> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
    types::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    tool_types::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    Ok(linker)
}

pub fn tool_linker(engine: &Engine) -> wasmtime::Result<Linker<State>> {
    base(engine)
}

/// The agent's linker: the base plus the model, the session and its tools.
pub fn linker(engine: &Engine) -> wasmtime::Result<Linker<State>> {
    let mut linker = base(engine)?;
    models::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    chat::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    session::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    tools::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    run_types::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    Ok(linker)
}
