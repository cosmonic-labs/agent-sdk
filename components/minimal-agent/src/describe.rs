//! Error messages a person can read, for every error the agent reports.

use crate::agent::inference_types::{
    ContextOverflow, GenerationError, Quota, RateLimit, RequestError, SizeLimit,
};
use crate::agent::models::OpenError;
use crate::agent::session;
use crate::agent::tool_types::{CallError, DispatchError, ListError, OutcomeError};

/// Error messages a person can read. The generated types implement `Display`
/// and `Debug` too, but both print Rust syntax, and the formatting code behind
/// them is a good part of the component's size.
pub(crate) trait Describe {
    fn describe(&self) -> String;
}

impl Describe for OpenError {
    fn describe(&self) -> String {
        match self {
            Self::UnknownAlias(d) => format!("the host has no model by that alias ({d})"),
            Self::Denied(d) => format!("this workload may not use it ({d})"),
            Self::Unavailable(d) => format!("the model is unavailable ({d})"),
            Self::TooLarge(limit) => format!("the alias is over the {limit}-byte limit"),
            Self::Other(d) => d.clone(),
        }
    }
}

impl Describe for RequestError {
    fn describe(&self) -> String {
        match self {
            Self::Cancelled => "the request was cancelled".into(),
            Self::TimedOut => "the request timed out".into(),
            Self::ContextLengthExceeded(c) => c.describe(),
            Self::InvalidOptions(d) => format!("invalid options ({d})"),
            Self::MessagesIncomplete(d) => format!("the conversation was incomplete ({d})"),
            Self::Unrepresentable(changes) => {
                let details: Vec<&str> = changes.iter().map(|c| c.detail.as_str()).collect();
                format!(
                    "the provider cannot carry this request ({})",
                    details.join("; ")
                )
            }
            Self::NotAuthenticated(d) => {
                format!("the host's credentials for the provider were rejected ({d})")
            }
            Self::RateLimited(r) => format!("rate limited: {}", r.describe()),
            Self::QuotaExhausted(q) => q.describe(),
            Self::TemporarilyUnavailable(r) => format!("temporarily unavailable: {}", r.describe()),
            Self::TooLarge(s) => s.describe(),
            Self::Other(d) => d.clone(),
        }
    }
}

impl Describe for GenerationError {
    fn describe(&self) -> String {
        match self {
            Self::Cancelled => "generation was cancelled".into(),
            Self::TimedOut => "generation timed out".into(),
            Self::RateLimited(r) => format!("rate limited: {}", r.describe()),
            Self::TemporarilyUnavailable(r) => format!("temporarily unavailable: {}", r.describe()),
            Self::ContextLengthExceeded(c) => c.describe(),
            Self::Other(d) => d.clone(),
        }
    }
}

impl Describe for ContextOverflow {
    fn describe(&self) -> String {
        format!(
            "the conversation is too long for the model ({})",
            self.detail
        )
    }
}

impl Describe for RateLimit {
    fn describe(&self) -> String {
        match self.retry_after_seconds {
            Some(s) => format!("{}; retry in {s}s", self.detail),
            None => self.detail.clone(),
        }
    }
}

impl Describe for Quota {
    fn describe(&self) -> String {
        let out = format!("the provider account is out of quota ({})", self.detail);
        match &self.top_up_url {
            Some(url) => format!("{out}; top up at {url}"),
            None => out,
        }
    }
}

impl Describe for SizeLimit {
    fn describe(&self) -> String {
        format!(
            "the {} is over the host's {}-byte limit",
            self.what, self.limit
        )
    }
}

impl Describe for session::Error {
    fn describe(&self) -> String {
        match self {
            Self::TooLarge(t) => format!("the session is full ({}-byte limit)", t.limit),
            Self::InvalidArgument(d) => format!("invalid request ({d})"),
            Self::Unavailable(d) => format!("unavailable ({d})"),
            Self::Other(d) => d.clone(),
        }
    }
}

/// What the model reads when a tool call reached its time limit, in either
/// case: `dispatch-error::timed-out` (it never started) and
/// `outcome-error::timed-out` (no result came back, and the call may have
/// done some of its work, or may even go on: the host may not stop it). `timed-out` carries no text, so the harness words it; this is the
/// wording every harness should give its model, so it asks for less rather
/// than repeating a call that will run out of time again.
pub(crate) fn tool_timed_out(started: bool) -> String {
    let what = if started {
        "the call reached its time limit and no result came back, so it may have done some or \
         all of its work"
    } else {
        "the call reached its time limit before the tool started it, so it had no effect"
    };
    format!("{what}. {TIMED_OUT_ADVICE}")
}

/// The advice in [`tool_timed_out`].
const TIMED_OUT_ADVICE: &str = "Do not repeat the same call: ask for less in one call (a \
     narrower query, a smaller folder or file, fewer items, or one page at a time)";

impl Describe for CallError {
    fn describe(&self) -> String {
        match self {
            Self::NotDispatched(e) => e.describe(),
            // Its text already says it may have taken effect.
            Self::OutcomeUnknown(e @ OutcomeError::TimedOut) => e.describe(),
            Self::OutcomeUnknown(e) => {
                format!("{}; whether it took effect is unknown", e.describe())
            }
        }
    }
}

impl Describe for DispatchError {
    fn describe(&self) -> String {
        match self {
            Self::UnknownTool(name) => format!("there is no tool named {name}"),
            Self::InvalidArguments(d) => format!("invalid arguments: {d}"),
            Self::Upstream(d) => format!("the tool could not run: {d}"),
            Self::Denied(d) => format!("the call was declined: {d}"),
            Self::Cancelled => "the call was cancelled".into(),
            Self::TimedOut => tool_timed_out(false),
            Self::Other(d) => d.clone(),
        }
    }
}

impl Describe for OutcomeError {
    fn describe(&self) -> String {
        match self {
            Self::Upstream(d) => format!("the tool failed: {d}"),
            Self::Cancelled => "the call was cancelled".into(),
            Self::TimedOut => tool_timed_out(true),
            Self::Other(d) => d.clone(),
        }
    }
}

impl Describe for ListError {
    fn describe(&self) -> String {
        match self {
            Self::Unavailable(d) => format!("the tools are unavailable ({d})"),
            Self::Denied(d) => format!("listing the tools was denied ({d})"),
            Self::Other(d) => d.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both kinds of tool timeout tell the model to ask for less, and say
    /// truthfully whether the call may have done anything.
    #[test]
    fn a_tool_timeout_tells_the_model_what_to_do() {
        let stopped = CallError::OutcomeUnknown(OutcomeError::TimedOut).describe();
        assert!(
            stopped.contains("may have done some or all of its work"),
            "{stopped}"
        );
        assert!(!stopped.contains("whether it took effect"), "{stopped}");
        let never = CallError::NotDispatched(DispatchError::TimedOut).describe();
        assert!(never.contains("had no effect"), "{never}");
        for text in [stopped, never] {
            assert!(text.contains("Do not repeat the same call"), "{text}");
            assert!(text.contains("one page at a time"), "{text}");
        }
        // Other unknown outcomes still say so.
        let failed = CallError::OutcomeUnknown(OutcomeError::Cancelled).describe();
        assert!(
            failed.ends_with("whether it took effect is unknown"),
            "{failed}"
        );
    }
}
