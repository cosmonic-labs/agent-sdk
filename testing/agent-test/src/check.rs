//! Assertions: each returns the ways the record differs from `expect`.
use serde_json::Value;

use crate::host::run_types::{Finish, ReplyPart, RunError, StartError};
use crate::record::{Dispatched, Refusal, Refused, Response};
use crate::runner::{Finished, Ran};
use crate::spec::{CallMatch, Calls, Expect, Refusal as RefusalSpec, TextMatch};

fn outcome_name(ran: &Ran) -> String {
    match ran {
        Ran::Refused(e) => format!(
            "not-started ({})",
            match e {
                StartError::Busy => "busy",
                StartError::Duplicate => "duplicate",
                StartError::InvalidInput(_) => "invalid-input",
                StartError::Unavailable(_) => "unavailable",
                StartError::Other(_) => "other",
            }
        ),
        Ran::Started { outcome: Ok(_) } => "done".into(),
        Ran::Started { outcome: Err(e) } => match e {
            RunError::Cancelled => "cancelled",
            RunError::NoModel(_) => "no-model",
            RunError::ModelRefused(_) => "model-refused",
            RunError::ModelFailed(_) => "model-failed",
            RunError::NotRecorded(_) => "not-recorded",
            RunError::Superseded => "superseded",
            RunError::OutputRejected(_) => "output-rejected",
            RunError::Failed(_) => "failed",
            RunError::Other(_) => "other",
        }
        .into(),
    }
}

fn finish_name(f: &Finish) -> String {
    match f {
        Finish::Done => "done".into(),
        Finish::InputRequired => "input-required".into(),
        Finish::MaxSteps => "max-steps".into(),
        Finish::MaxTotalTokens => "max-total-tokens".into(),
        Finish::MaxDuration => "max-duration".into(),
        Finish::Refusal => "refusal".into(),
        Finish::ContentFilter => "content-filter".into(),
        Finish::Other(s) => s.clone(),
    }
}

pub fn text_matches(m: &TextMatch, text: &str) -> Result<(), String> {
    match m {
        TextMatch::Exact(want) if want == text => Ok(()),
        TextMatch::Exact(want) => Err(format!("expected {want:?}, got {text:?}")),
        TextMatch::Rule(r) => {
            if let Some(c) = &r.contains
                && !text.contains(c.as_str())
            {
                return Err(format!("expected text containing {c:?}, got {text:?}"));
            }
            if let Some(p) = &r.starts_with
                && !text.starts_with(p.as_str())
            {
                return Err(format!("expected text starting {p:?}, got {text:?}"));
            }
            if let Some(re) = &r.matches {
                let re = regex::Regex::new(re).map_err(|e| format!("bad regex {re:?}: {e}"))?;
                if !re.is_match(text) {
                    return Err(format!("expected text matching /{re}/, got {text:?}"));
                }
            }
            Ok(())
        }
    }
}

/// `want` is contained in `got`: every field of an object, recursively.
fn includes(got: &Value, want: &Value) -> bool {
    match (got, want) {
        (Value::Object(g), Value::Object(w)) => w
            .iter()
            .all(|(k, wv)| g.get(k).is_some_and(|gv| includes(gv, wv))),
        _ => got == want,
    }
}

fn call_matches(m: &CallMatch, d: &Dispatched) -> bool {
    if m.name != d.name {
        return false;
    }
    if let Some(a) = &m.arguments
        && d.arguments.as_ref() != Some(a)
    {
        return false;
    }
    if let Some(a) = &m.arguments_include
        && !d.arguments.as_ref().is_some_and(|g| includes(g, a))
    {
        return false;
    }
    if let Some(t) = &m.text
        && text_matches(t, d.text.as_deref().unwrap_or_default()).is_err()
    {
        return false;
    }
    let (text, is_error) = match &d.response {
        Response::Returned { text, is_error } => (text.as_str(), Some(*is_error)),
        Response::Failed(e) => (e.as_str(), None),
    };
    if let Some(e) = m.is_error
        && is_error.unwrap_or(true) != e
    {
        return false;
    }
    if let Some(r) = &m.result
        && text_matches(r, text).is_err()
    {
        return false;
    }
    true
}

fn show(d: &Dispatched) -> String {
    let args = match (&d.arguments, &d.text) {
        (Some(v), _) => v.to_string(),
        (None, Some(t)) => format!("{t:?}"),
        (None, None) => String::new(),
    };
    let resp = match &d.response {
        Response::Returned { text, is_error } => {
            format!("{}{text:?}", if *is_error { "error " } else { "" })
        }
        Response::Failed(e) => format!("failed {e}"),
    };
    format!("{}({args}) -> {resp}", d.name)
}

fn show_all(calls: &[Dispatched]) -> String {
    if calls.is_empty() {
        return "none".into();
    }
    calls.iter().map(show).collect::<Vec<_>>().join("; ")
}

fn check_calls(spec: &Calls, got: &[Dispatched], fail: &mut Vec<String>) {
    let (exactly, in_order, includes) = match spec {
        Calls::Exactly(l) => (Some(l), None, None),
        Calls::Rule(r) => (r.exactly.as_ref(), r.in_order.as_ref(), r.includes.as_ref()),
    };
    if let Some(want) = exactly {
        let ok = want.len() == got.len() && want.iter().zip(got).all(|(m, d)| call_matches(m, d));
        if !ok {
            fail.push(format!(
                "tool calls: expected exactly [{}], the host dispatched: {}",
                want.iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                show_all(got)
            ));
        }
    }
    if let Some(want) = in_order {
        let mut rest = got.iter();
        for m in want {
            if !rest.any(|d| call_matches(m, d)) {
                fail.push(format!(
                    "tool calls: no {} in order; the host dispatched: {}",
                    m.name,
                    show_all(got)
                ));
                break;
            }
        }
    }
    if let Some(want) = includes
        && !distinct_matches(want, got)
    {
        fail.push(format!(
            "tool calls: expected [{}] each matched by its own call; the host dispatched: {}",
            want.iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            show_all(got)
        ));
    }
}

/// Whether every expected call can be given its own recorded call: a
/// bipartite matching, by augmenting paths, so overlapping matchers are
/// assigned correctly.
fn distinct_matches(want: &[CallMatch], got: &[Dispatched]) -> bool {
    fn assign(
        w: usize,
        want: &[CallMatch],
        got: &[Dispatched],
        owner: &mut [Option<usize>],
        seen: &mut [bool],
    ) -> bool {
        for (g, d) in got.iter().enumerate() {
            if seen[g] || !call_matches(&want[w], d) {
                continue;
            }
            seen[g] = true;
            let free = match owner[g] {
                None => true,
                Some(other) => assign(other, want, got, owner, seen),
            };
            if free {
                owner[g] = Some(w);
                return true;
            }
        }
        false
    }
    let mut owner = vec![None; got.len()];
    (0..want.len()).all(|w| assign(w, want, got, &mut owner, &mut vec![false; got.len()]))
}

fn refusal_matches(want: &RefusalSpec, got: &Refused) -> bool {
    if want.by.as_ref().is_some_and(|b| b != &got.by) {
        return false;
    }
    match (&got.what, &want.tool, &want.host) {
        (Refusal::Tool { name, .. }, Some(t), None) => name == t,
        (Refusal::Host { host, .. }, None, Some(h)) => host.eq_ignore_ascii_case(h),
        (Refusal::Socket { addr }, None, Some(h)) => addr == h,
        _ => false,
    }
}

fn show_refusal(r: &Refused) -> String {
    match &r.what {
        Refusal::Tool { name, why } => format!("tool {name} ({why}) by {}", r.by),
        Refusal::Host { url, .. } => format!("request to {url} by {}", r.by),
        Refusal::Socket { addr } => format!("socket to {addr} by {}", r.by),
    }
}

/// Every way `finished` falls short of `expect`; empty when the test passes.
pub fn check(expect: &Expect, finished: &Finished) -> Vec<String> {
    let rec = &finished.record;
    let mut fail: Vec<String> = rec.problems.clone();

    let outcome = outcome_name(&finished.ran);
    let want_outcome = expect.outcome.as_deref().unwrap_or("done");
    if !(outcome == want_outcome || outcome.starts_with(&format!("{want_outcome} "))) {
        let detail = match &finished.ran {
            Ran::Started { outcome: Err(e) } => format!(": {e:?}"),
            Ran::Refused(e) => format!(": {e:?}"),
            _ => String::new(),
        };
        fail.push(format!(
            "outcome: expected {want_outcome}, got {outcome}{detail}"
        ));
    }
    if let Ran::Started { outcome: Ok(o) } = &finished.ran {
        if let Some(f) = &expect.finish
            && &finish_name(&o.finish) != f
        {
            fail.push(format!(
                "finish: expected {f}, got {}",
                finish_name(&o.finish)
            ));
        }
        if let Some(m) = &expect.reply {
            let text: String = o
                .reply
                .iter()
                .filter_map(|p| match p {
                    ReplyPart::Text(t) | ReplyPart::Refusal(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            if let Err(e) = text_matches(m, &text) {
                fail.push(format!("reply: {e}"));
            }
        }
    } else if expect.finish.is_some() || expect.reply.is_some() {
        fail.push(format!(
            "finish and reply: the run ended {outcome}, with no reply"
        ));
    }

    if let Some(spec) = &expect.tool_calls {
        check_calls(spec, &rec.dispatched, &mut fail);
    }
    for (name, want) in &expect.call_counts {
        let got = rec.dispatched.iter().filter(|d| &d.name == name).count();
        if got != *want {
            fail.push(format!(
                "{name} was dispatched {got} times, expected {want}"
            ));
        }
    }
    if let Some(want) = &expect.refused {
        let ok = want.len() == rec.refused.len()
            && want
                .iter()
                .zip(&rec.refused)
                .all(|(w, g)| refusal_matches(w, g));
        if !ok {
            let got: Vec<String> = rec.refused.iter().map(show_refusal).collect();
            fail.push(format!(
                "refusals: expected {} in order, the host recorded: {}",
                want.len(),
                if got.is_empty() {
                    "none".into()
                } else {
                    got.join("; ")
                }
            ));
        }
    }
    for name in &expect.never.calls {
        let n = rec.dispatched.iter().filter(|d| &d.name == name).count();
        if n > 0 {
            fail.push(format!("never: {name} was dispatched {n} times"));
        }
    }
    for h in &expect.never.reaches {
        for e in rec.egress.iter().filter(|e| {
            e.allowed && crate::gate::granted(std::slice::from_ref(h), &e.host, &e.authority)
        }) {
            fail.push(format!(
                "never: {} reached {h} ({} {})",
                e.by, e.method, e.url
            ));
        }
    }
    if let Some(n) = expect.model_calls
        && rec.model_calls.len() != n
    {
        fail.push(format!(
            "model calls: expected {n}, the host served {}",
            rec.model_calls.len()
        ));
    }
    if let Some(kinds) = &expect.journal
        && kinds != &finished.journal
    {
        fail.push(format!(
            "journal: expected [{}], got [{}]",
            kinds.join(", "),
            finished.journal.join(", ")
        ));
    }
    fail
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn includes_is_recursive() {
        let got = json!({"a": 1, "b": {"c": 2, "d": 3}});
        assert!(includes(&got, &json!({"b": {"c": 2}})));
        assert!(!includes(&got, &json!({"b": {"c": 3}})));
        assert!(!includes(&got, &json!({"x": 1})));
    }

    fn call(name: &str, args: Value) -> Dispatched {
        Dispatched {
            id: String::new(),
            name: name.into(),
            arguments: Some(args),
            text: None,
            response: Response::Returned {
                text: String::new(),
                is_error: false,
            },
        }
    }

    #[test]
    fn includes_needs_distinct_calls() {
        let any: CallMatch = serde_yaml_ng::from_str("{name: echo}").unwrap();
        let hi: CallMatch = serde_yaml_ng::from_str("{name: echo, arguments: {text: hi}}").unwrap();
        let one = [call("echo", json!({"text": "hi"}))];
        assert!(!distinct_matches(&[any.clone(), any.clone()], &one));
        // The loose matcher must take the second call for both to match.
        let two = [
            call("echo", json!({"text": "hi"})),
            call("echo", json!({"text": "yo"})),
        ];
        assert!(distinct_matches(&[any, hi], &two));
    }

    #[test]
    fn text_rules() {
        let m: TextMatch = serde_yaml_ng::from_str("{contains: lo, starts-with: he}").unwrap();
        assert!(text_matches(&m, "hello").is_ok());
        assert!(text_matches(&m, "yellow").is_err());
        let m: TextMatch = serde_yaml_ng::from_str("{matches: '^h.*o$'}").unwrap();
        assert!(text_matches(&m, "hello").is_ok());
        assert!(text_matches(&TextMatch::Exact("a".into()), "b").is_err());
    }
}
