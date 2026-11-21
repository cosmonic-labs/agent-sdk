//! The Codex agent harness: Codex's agent loop (turn handling,
//! history normalization, tool-output truncation and local compaction) as a
//! component that exports `cosmonic:agent/run@0.4.2` and
//! `cosmonic:agent/compact@0.4.2`.
//!
//! The host provides everything else: model calls (`models`, `chat`), tools
//! (`tools`) and the session (`session`). The component has no HTTP server,
//! no model client or credentials, no shell or file tools, and no outgoing
//! HTTP, sockets or filesystem imports.
//!
//! Layout:
//! - `codex/`: the vendored Codex modules (models, history, truncation,
//!   compaction, prompts), each naming its upstream source;
//! - `turn`: Codex's `run_turn` loop over the host;
//! - `host`: history to and from `chat` messages, tool calls;
//! - `plan`: Codex's `update_plan`, answered here and streamed as `plan`;
//! - `session`: the session journal conventions;
//! - `run`: the typed export;
//! - `requested`: the `compact` export, a compaction the driver asks for.

pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "codex-agent",
        // `run` and its types are unstable in cosmonic:agent@0.4.2.
        features: ["agent-run"],
        generate_all,
    });
}

pub mod codex;
mod host;
mod plan;
mod requested;
mod run;
mod session;
mod turn;

use futures::lock::{Mutex, MutexGuard};

use session::TurnError;
use turn::{Accepted, TurnRequest};

/// Model calls a run makes when the driver sets no max-steps. Codex itself
/// has no step limit; a run needs one so a looping model ends.
pub(crate) const DEFAULT_MAX_STEPS: u32 = 32;
/// The most model calls a driver may ask for.
pub(crate) const MAX_STEPS_CAP: u32 = 128;

/// One run or compaction at a time in this instance: the backstop against two
/// drivers on one session. The session's conditional commits stop a second
/// instance.
static RUNNING: Mutex<()> = Mutex::new(());
type Running = MutexGuard<'static, ()>;

/// Open the run: take the lock, read the session, close what a run cut off by
/// a restart left open, and commit the person's message with `agent.run.v1`
/// under the commit id `run:<run-id>`. Nothing is written on an error.
pub(crate) async fn accept(req: &TurnRequest) -> Result<(Accepted, Running), TurnError> {
    let running = RUNNING.try_lock().ok_or(TurnError::Busy)?;
    let user_step = serde_json::json!({
        "role": "user", "text": req.text, "blocks": ["text"], "tools": [], "run": req.run,
    });
    let user = codex::models::ResponseItem::user_text(req.text.clone());
    let (journal, conversation) = session::open_turn(user, &user_step, &req.run).await?;
    let base = conversation.len().saturating_sub(1);
    let deadline = req.max_duration.map(|d| wasip3::clocks::monotonic_clock::now().saturating_add(d));
    Ok((Accepted { journal, conversation, base, deadline }, running))
}

use run::Runner;
bindings::export!(Runner with_types_in bindings);
