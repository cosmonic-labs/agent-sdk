//! Run an agent component against scripted model replies and inspect what the host recorded.
mod gate;
mod host;
mod model;
pub mod record;
mod runner;
mod session;
pub mod spec;
mod tools;

pub use host::{run_types, tool_types, types};
pub use runner::{Finished, Ran, run};
