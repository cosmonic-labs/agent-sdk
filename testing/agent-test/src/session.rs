//! An in-memory `cosmonic:agent/session` with the conditional commit rules.
use wasmtime::component::{Accessor, HasSelf};

use crate::host::{Entry, Session, State, session};

fn end(s: &Session) -> session::JournalEnd {
    match s.entries.last() {
        Some(e) => session::JournalEnd::At(e.seq),
        None => session::JournalEnd::Empty,
    }
}

fn same(a: &session::JournalEnd, b: &session::JournalEnd) -> bool {
    match (a, b) {
        (session::JournalEnd::Empty, session::JournalEnd::Empty) => true,
        (session::JournalEnd::At(x), session::JournalEnd::At(y)) => x == y,
        _ => false,
    }
}

fn snapshot(s: &Session) -> session::Snapshot {
    session::Snapshot {
        state: s
            .state
            .as_ref()
            .map(|(value, version, through)| session::State {
                value: value.clone(),
                version: version.clone(),
                through: match through {
                    Some(seq) => session::JournalEnd::At(*seq),
                    None => session::JournalEnd::Empty,
                },
            }),
        end: end(s),
    }
}

fn commit(
    s: &mut Session,
    entries: Vec<session::NewEntry>,
    state: Option<Vec<u8>>,
    options: session::CommitOptions,
) -> Result<session::CommitResult, session::Error> {
    if let Some(id) = &options.commit_id
        && let Some(result) = s.commits.get(id)
    {
        return Ok(result.clone());
    }
    if state.is_some() && (options.require_state.is_none() || options.require_end.is_none()) {
        return Err(session::Error::InvalidArgument(
            "a state write requires both conditions".into(),
        ));
    }
    if let Some(want) = &options.require_end
        && !same(want, &end(s))
    {
        return Ok(session::CommitResult::Stale(snapshot(s)));
    }
    if let Some(condition) = &options.require_state {
        let current = s.state.as_ref().map(|(_, v, _)| v.as_str());
        let ok = match condition {
            session::StateCondition::Absent => current.is_none(),
            session::StateCondition::Version(v) => current == Some(v.as_str()),
        };
        if !ok {
            return Ok(session::CommitResult::Stale(snapshot(s)));
        }
    }
    for entry in entries {
        s.seq += 1;
        let seq = s.seq;
        s.entries.push(Entry {
            seq,
            kind: entry.kind,
            data: entry.data,
        });
    }
    let mut version = None;
    if let Some(value) = state {
        s.versions += 1;
        let v = format!("v{}", s.versions);
        let through = s.entries.last().map(|e| e.seq);
        s.state = Some((value, v.clone(), through));
        version = Some(v);
    }
    let result = session::CommitResult::Committed(session::Committed {
        end: end(s),
        version,
    });
    if let Some(id) = options.commit_id {
        s.commits.insert(id, result.clone());
    }
    Ok(result)
}

impl session::Host for State {}

impl<T: Send> session::HostWithStore<T> for HasSelf<State> {
    async fn current(
        store: &Accessor<T, Self>,
    ) -> wasmtime::Result<Result<session::Snapshot, session::Error>> {
        Ok(Ok(store.with(|mut s| snapshot(&s.get().host.session()))))
    }

    async fn commit(
        store: &Accessor<T, Self>,
        entries: Vec<session::NewEntry>,
        state: Option<Vec<u8>>,
        options: session::CommitOptions,
    ) -> wasmtime::Result<Result<session::CommitResult, session::Error>> {
        Ok(store.with(|mut s| commit(&mut s.get().host.session(), entries, state, options)))
    }

    async fn read(
        store: &Accessor<T, Self>,
        start: u64,
        max: u32,
    ) -> wasmtime::Result<Result<Vec<session::Entry>, session::Error>> {
        Ok(Ok(store.with(|mut s| {
            s.get()
                .host
                .session()
                .entries
                .iter()
                .filter(|e| e.seq >= start)
                .take(max as usize)
                .map(|e| session::Entry {
                    seq: e.seq,
                    author: "agent".into(),
                    at_ms: 0,
                    kind: e.kind.clone(),
                    data: e.data.clone(),
                })
                .collect()
        })))
    }

    async fn follow(
        _store: &Accessor<T, Self>,
        _start: u64,
    ) -> wasmtime::Result<Result<wasmtime::component::StreamReader<session::Entry>, session::Error>>
    {
        Ok(Err(session::Error::Unavailable(
            "this host does not follow".into(),
        )))
    }

    async fn clear(store: &Accessor<T, Self>) -> wasmtime::Result<Result<(), session::Error>> {
        store.with(|mut s| {
            let host = std::sync::Arc::clone(&s.get().host);
            let mut session = host.session();
            session.entries.clear();
            session.state = None;
            session.commits.clear();
        });
        Ok(Ok(()))
    }
}
