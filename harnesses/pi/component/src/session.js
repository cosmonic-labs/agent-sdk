/*
 * The session journal over cosmonic:agent/session@0.4.2, a host store scoped
 * to this workload: no id, nothing to guess or enumerate, deleted with the
 * workload.
 *
 * Every write is one commit of journal entries and, optionally, the state
 * (Pi's transcript). A turn commits at every point where something happened:
 *
 *   openTurn:  [unresolved ops closed, step: the person's message] + state
 *   record:    [step: the reply, op: each tool call]          (no state)
 *              ... the tool calls ...
 *   record:    [op-result: the call, step: its result] + state once no op is open
 *   endRun:    [run-result: how the run ended]
 *
 * A compaction (compaction.js) summarizes the older transcript with a model
 * call, which is an op like any other, and lands its result with
 * agent.compaction.v1 and the smaller transcript as state in one commit:
 *
 *   beginModelOp: [op: target model]                          (no state)
 *   compacted:    [op-result, compaction: the summary] + state
 *
 * It only happens with no tool call open, so the state never moves past an
 * unresolved op. The journal keeps every original message.
 *
 * openTurn's commit also carries agent.run.v1 under the commit id
 * `run:<run-id>`, so the same run id again is answered with the original
 * commit instead of a second run (see cosmonic:agent/run).
 *
 * State is never written between an op and its result, so every unresolved op
 * is after the state's `through`, which is what makes a restart safe.
 */

import * as store from 'cosmonic:agent/session@0.4.2';
import { getRandomBytes } from 'wasi:random/random@0.3.0';

import { describeVariant } from '../../../shared/js/wit.js';
import {
    COMPACTION_KIND,
    COMPACT_KIND,
    COMPACT_RESULT_KIND,
    Superseded,
    beforeLastPerson,
    closeCompaction,
    compactResultData,
    compactionAccepted,
    turnStart,
} from './compaction.js';
import {
    CONTEXT_RESET_KIND,
    OP_KIND,
    OP_RESULT_KIND,
    RUN_KIND,
    RUN_RESULT_KIND,
    STEP_KIND,
    closingEntries,
    condition,
    entry,
    later,
    readEntries,
    readableTranscript,
    resolutionEntries,
    stepWithoutImages,
    startAfter,
    stepsOf,
    unresolvedOps,
} from './journal-rules.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

const STALE_RETRIES = 3;
const UNAVAILABLE_RETRIES = 2;
const PAGE = 1000;

export class Busy extends Error {
    constructor() {
        super('the session kept changing under this turn; try again');
    }
}
export class Duplicate extends Error {
    constructor() {
        super('a run with this id already started in this session');
    }
}
export { Superseded };

export function describe(e) {
    const p = e?.payload;
    if (p && typeof p === 'object' && 'tag' in p) {
        return describeVariant(p);
    }
    return String(e?.message || e);
}

// Random, never a counter: counters restart with the agent, and the host keeps
// the most recent ids (at least 1024); `clear` forgets them. Not Math.random,
// whose state is in the snapshotted heap.
function commitId() {
    return Array.from(getRandomBytes(16), (b) => b.toString(16).padStart(2, '0')).join('');
}

// A fresh random id, for a run that needs one (POST /task names none).
export const randomId = commitId;

// One commit, resent under the same commit-id while the host is unavailable
// (so a retry after a lost reply cannot write twice), and with a step's
// images dropped if they put it over the host's entry limit. `fixedId` is a
// commit id with a meaning (a run's `run:<run-id>`), which a retry keeps.
async function commit(entries, state, requireState, requireEnd, fixedId) {
    let id = fixedId ?? commitId();
    let unavailable = 0;
    for (;;) {
        try {
            return await store.commit(entries, state, { requireState, requireEnd, commitId: id });
        } catch (e) {
            const p = e?.payload;
            if (p?.tag === 'unavailable' && unavailable < UNAVAILABLE_RETRIES) {
                unavailable++;
                continue;
            }
            const i = p?.tag === 'too-large' ? p.val?.entry : undefined;
            const withoutImages = i != null && entries[i]?.kind === STEP_KIND ? stepWithoutImages(entries[i].data) : null;
            if (withoutImages) {
                entries = entries.slice();
                entries[i] = { kind: STEP_KIND, data: withoutImages };
                id = fixedId ?? commitId(); // different content, different commit
                continue;
            }
            throw e;
        }
    }
}

const readFrom = (start) => readEntries(store, start, PAGE);

// The saved transcript, or why it is unusable.
function saved(snapshot) {
    if (!snapshot.state) {
        return { messages: [] };
    }
    let v = null;
    try {
        v = JSON.parse(dec.decode(snapshot.state.value));
    } catch {
        /* unreadable */
    }
    return readableTranscript(v) ? { messages: v } : { messages: [], unreadable: true };
}

class Journal {
    constructor(version, end, written, beforeTurn = null) {
        this.before = beforeTurn; // the last seq before this turn's person's step
        this.version = version; // the state's version; null is absent
        this.end = end; // where the journal ended after this writer's last commit
        this.ops = new Map(); // op id -> "committed" | "dispatched" | { done: outcome }
        this.names = new Map(); // op id -> tool name, for an unresolved call's error step
        this.statePending = false;
        this.superseded = false;
        this.lastStale = null;
        this.written = written; // how many messages the saved state holds
        this.compactionSeq = null; // the seq of the last agent.compaction.v1 committed
    }

    // The seq of the agent.compaction.v1 this writer last committed.
    lastCompactionSeq() {
        return this.compactionSeq;
    }

    // Mark a tool call as being made. Refuses a call whose op is not in the
    // journal, so no call is made that a restart could not account for.
    dispatch(id) {
        if (this.ops.get(id) !== 'committed') {
            return false;
        }
        this.ops.set(id, 'dispatched');
        return true;
    }

    // Whether a tool call is in the journal without its result. The state
    // cannot be replaced then, so a compaction waits.
    hasOpenOps() {
        return this.ops.size > 0;
    }

    // The last sequence number before this turn's person's message: the end
    // of what a summary made now can cover. Read from the journal when the
    // turn opened, never inferred, since sequence numbers may have gaps.
    beforeTurn() {
        return this.before;
    }

    // Commit the op for a model call this agent is about to make, before
    // making it. Conditional, like a tool call's: a replaced writer stops here
    // (Superseded). If the call never returns, the next turn closes it as
    // cancelled, since a model call may repeat.
    async beginModelOp(op, purpose) {
        const data = { op, target: 'model', name: 'default', purpose };
        let r;
        try {
            r = await commit([entry(OP_KIND, data)], undefined, condition(this.version), this.end);
        } catch (e) {
            throw new Error(`recording the model call: ${describe(e)}`);
        }
        if (r.tag === 'stale') {
            this.superseded = true;
            this.lastStale = r.val;
            throw new Superseded();
        }
        this.end = r.val.end;
    }

    // Close a compaction's model op. With `compaction` (the entry's data), the
    // result, the entry and the smaller transcript as state land in one
    // commit, and this resolves to true. When the state is over the host's
    // limit the summary is not used: only the op's result lands, as failed,
    // with no compaction entry, the larger state stays, and this resolves to
    // false so the run goes on uncompacted. When another writer changed the
    // session meanwhile, the failed result lands the same way and then this
    // throws Superseded: the run stops before its next model or tool call.
    // Without `compaction` (the summary failed) the result lands as failed
    // with `messages`, unchanged, and a lost race there stops the run too.
    //
    // `extra` goes in the installing commit after the entry: a requested
    // compaction's agent.compact-result.v1.
    async compacted(op, compaction, messages, extra = []) {
        const failed = () => [entry(OP_RESULT_KIND, { op, outcome: 'failed' })];
        if (!compaction) {
            await this.land(failed(), messages);
            if (this.superseded) {
                throw new Superseded();
            }
            return false;
        }
        const outcome = await closeCompaction({
            owned: !this.superseded,
            install: async () => {
                const entries = [
                    entry(OP_RESULT_KIND, { op, outcome: 'succeeded' }),
                    entry(COMPACTION_KIND, compaction),
                    ...extra,
                ];
                const before = this.end;
                let r;
                try {
                    r = await commit(entries, enc.encode(JSON.stringify(messages)), condition(this.version), this.end);
                } catch (e) {
                    if (e?.payload?.tag === 'too-large') {
                        return 'too-large';
                    }
                    throw new Error(`saving the compaction: ${describe(e)}`);
                }
                if (r.tag === 'committed') {
                    this.end = r.val.end;
                    this.version = r.val.version ?? this.version;
                    this.statePending = false;
                    this.written = messages.length;
                    // The entry's seq: the journal end the commit reports when
                    // the entry is its last (a commit is atomic), else read
                    // back, since seqs may have gaps.
                    this.compactionSeq = r.val.end.tag === 'at' ? Number(r.val.end.val) : null;
                    if (extra.length && this.compactionSeq != null) {
                        const end = this.compactionSeq;
                        this.compactionSeq = null;
                        try {
                            const landed = await readFrom(startAfter(before));
                            const mine = landed.filter((e) => e.kind === COMPACTION_KIND && Number(e.seq) <= end);
                            this.compactionSeq = mine.length ? Number(mine[mine.length - 1].seq) : null;
                        } catch (e) {
                            console.error(`session: reading the compaction back: ${describe(e)}`);
                        }
                    }
                    return 'committed';
                }
                this.superseded = true;
                this.lastStale = r.val;
                return 'stale';
            },
            // The call happened, so its result must land; without conditions,
            // as land's fallback does, but never claiming the compaction.
            closeFailed: async () => {
                try {
                    const r = await commit(failed(), undefined, undefined, undefined);
                    if (r.tag === 'committed') {
                        this.end = r.val.end;
                    }
                } catch (e) {
                    throw new Error(`saving the compaction: ${describe(e)}`);
                }
            },
        });
        if (outcome === 'superseded') {
            throw new Superseded();
        }
        return outcome === 'compacted';
    }

    finish(id, outcome) {
        if (this.ops.has(id)) {
            this.ops.set(id, { done: outcome });
        }
    }

    takeResults(ids) {
        const out = [];
        for (const id of ids ?? [...this.ops.keys()]) {
            if (!this.ops.has(id)) {
                continue;
            }
            out.push(
                ...closingEntries(id, this.names.get(id), this.ops.get(id)).map(([kind, data]) => entry(kind, data)),
            );
            this.ops.delete(id);
            this.names.delete(id);
        }
        return out;
    }

    // Commit one finished message. A reply that asks for tools goes with an op
    // per call and no state, conditional on the state and the journal end, so a
    // replaced writer stops here, before the calls (Superseded). Anything else
    // closes the ops it answers and goes with the transcript as state once no
    // op is left open.
    async record({ step, ops = [], closes, messages }) {
        const entries = [...this.takeResults(closes), ...(step ? [entry(STEP_KIND, step)] : [])];
        if (ops.length) {
            entries.push(...ops.map((o) => entry(OP_KIND, o.data)));
            let r;
            try {
                r = await commit(entries, undefined, condition(this.version), this.end);
            } catch (e) {
                throw new Error(`recording the tool calls: ${describe(e)}`);
            }
            if (r.tag === 'stale') {
                this.superseded = true;
                this.lastStale = r.val;
                throw new Superseded();
            }
            this.end = r.val.end;
            for (const o of ops) {
                this.ops.set(o.id, 'committed');
                this.names.set(o.id, o.data.name);
            }
            return;
        }
        if (!entries.length) {
            return;
        }
        await this.land(entries, this.ops.size ? undefined : messages);
    }

    // Commit entries that record something that already happened. After a call
    // the commit must land: on a lost race the entries go again with no
    // conditions, and the state waits for the end of the turn.
    async land(entries, messages) {
        if (messages && !this.superseded) {
            try {
                const r = await commit(
                    entries,
                    enc.encode(JSON.stringify(messages)),
                    condition(this.version),
                    this.end,
                );
                if (r.tag === 'committed') {
                    this.end = r.val.end;
                    this.version = r.val.version ?? this.version;
                    this.statePending = false;
                    this.written = messages.length;
                    return;
                }
                this.superseded = true;
                this.lastStale = r.val;
            } catch (e) {
                // A transcript over the host's limit keeps the record of what
                // happened; anything else is the caller's.
                if (!(e?.payload?.tag === 'too-large' && e.payload.val?.entry == null)) {
                    throw new Error(`saving the turn: ${describe(e)}`);
                }
            }
        }
        if (messages) {
            this.statePending = true;
        }
        if (!entries.length) {
            return;
        }
        try {
            const r = await commit(entries, undefined, undefined, undefined);
            if (r.tag === 'committed') {
                this.end = r.val.end;
            }
        } catch (e) {
            throw new Error(`saving the turn: ${describe(e)}`);
        }
    }

    // Record how a requested compaction ended, when no summary was committed
    // with it: the summary call's op closed as failed first, when there is
    // one, in the same commit. Already true, so it lands without conditions
    // and leaves the state as it was.
    async endCompaction(op, resultEntry) {
        const entries = [...(op ? [entry(OP_RESULT_KIND, { op, outcome: 'failed' })] : []), resultEntry];
        try {
            const r = await commit(entries, undefined, undefined, undefined);
            if (r.tag === 'committed') {
                this.end = r.val.end;
            }
        } catch (e) {
            console.error(`session: could not record the end of the compaction: ${describe(e)}`);
        }
    }

    // Record that a run ended. Already true, so it lands without conditions.
    async endRun(resultEntry) {
        try {
            await this.land([resultEntry], undefined);
        } catch (e) {
            console.error(`session: ${describe(e)}`);
        }
    }

    // End a turn that did not finish: closes the open ops (a call never made is
    // cancelled; one dropped mid-flight is unresolved, with an error step) and
    // stores `steps` with the transcript as far as it got.
    async close(steps, messages, extra = []) {
        const entries = [...this.takeResults(), ...steps.map((s) => entry(STEP_KIND, s)), ...extra];
        // Nothing new to record and the state already holds this transcript.
        if (!entries.length && !this.statePending && this.written === messages.length) {
            return;
        }
        try {
            await this.land(entries, messages);
        } catch (e) {
            console.error(`session: ${describe(e)}`);
        }
    }

    // The turn's last write, for a turn that finished. When the state was not
    // written after the last message (a commit after a call lost a race), write
    // it now, taking in what the other writer added: its transcript, then this
    // turn's messages (from `base`, the person's message, on), under the
    // conditions of the snapshot it returned. A transcript in a format this
    // build cannot read is not overwritten; an error step says so instead.
    async settle(messages, base) {
        if (!this.statePending && !this.superseded && this.written !== messages.length) {
            await this.land([], messages).catch((e) => console.error(`session: ${describe(e)}`));
        }
        if (!this.statePending) {
            return;
        }
        const ours = messages.slice(base);
        let snapshot = this.lastStale;
        this.lastStale = null;
        let failure = 'another writer kept changing the session';
        try {
            snapshot ??= await store.current();
            for (let i = 0; i < STALE_RETRIES; i++) {
                const theirs = saved(snapshot);
                if (theirs.unreadable) {
                    failure = 'another writer saved a transcript this version cannot read';
                    break;
                }
                const merged = [...theirs.messages, ...ours];
                const version = snapshot.state ? snapshot.state.version : null;
                const r = await commit([], enc.encode(JSON.stringify(merged)), condition(version), snapshot.end);
                if (r.tag === 'committed') {
                    this.end = r.val.end;
                    this.version = r.val.version ?? this.version;
                    this.statePending = false;
                    return;
                }
                snapshot = r.val;
            }
        } catch (e) {
            failure = describe(e);
        }
        const step = {
            role: 'error',
            text: `This turn is in the history, but the conversation the model sees next could not be saved: ${failure}.`,
            blocks: ['error'],
            tools: [],
        };
        try {
            await commit([entry(STEP_KIND, step)], undefined, undefined, undefined);
        } catch (e) {
            console.error(`session: ${describe(e)}`);
        }
    }
}

// The last commit of a run: agent.run-result.v1, written without conditions
// because it records what already happened.
export function runResult(run, outcome, { finish, error } = {}) {
    return entry(RUN_RESULT_KIND, { run, outcome, ...(finish ? { finish } : {}), ...(error ? { error } : {}) });
}

// The last sequence number before the person's step that openTurn committed,
// ending the journal at `end`, read back from the journal. The turn is open
// either way, so a failed read only means a summary records no range.
async function beforeTurn(prior, end) {
    if (end.tag !== 'at') {
        return null;
    }
    try {
        const landed = await readFrom(startAfter(prior));
        return turnStart(landed, Number(end.val), prior.tag === 'at' ? Number(prior.val) : null).before;
    } catch (e) {
        console.error(`session: reading the turn back: ${describe(e)}`);
        return null;
    }
}

// Start a turn: load the transcript, close the ops a previous run left
// unresolved, and commit the person's message with the transcript that now
// includes it. `initial(prior)` is the transcript to continue from (Pi starts
// an empty one with its system message). An unreadable saved transcript
// starts a fresh context, marked with agent.context-reset.v1 in the same
// commit, rather than failing every turn. Nothing has happened outside the
// session yet, so a lost race is retried from the snapshot `stale` returns.
//
// `run` is the run id: the commit also records agent.run.v1, under the commit
// id `run:<run-id>`. The host answers a commit id it has seen with the
// original result rather than writing, and that result's journal end is not
// past the end this turn read, which is how a repeated run id (Duplicate) is
// told from a fresh commit.
export async function openTurn(user, userStep, initial, run) {
    let snapshot = await store.current();
    for (let i = 0; i < STALE_RETRIES; i++) {
        const { messages: prior, unreadable } = saved(snapshot);
        const through = snapshot.state ? snapshot.state.through : { tag: 'empty' };
        const after = await readFrom(startAfter(through));
        const entries = resolutionEntries(unresolvedOps(after)).map(([kind, data]) => entry(kind, data));
        if (unreadable) {
            entries.push(entry(CONTEXT_RESET_KIND, {}));
        }
        entries.push(entry(STEP_KIND, userStep));
        if (run) {
            entries.push(entry(RUN_KIND, { run }));
        }
        const context = initial(prior);
        const state = [...context, user];
        const version = snapshot.state ? snapshot.state.version : null;
        const id = run ? `run:${run}` : undefined;
        const r = await commit(entries, enc.encode(JSON.stringify(state)), condition(version), snapshot.end, id);
        if (r.tag === 'committed') {
            if (run && !later(r.val.end, snapshot.end)) {
                throw new Duplicate();
            }
            const before = await beforeTurn(snapshot.end, r.val.end);
            return { journal: new Journal(r.val.version ?? null, r.val.end, state.length, before), context };
        }
        // A run's commit id is not resent with other content: the host may
        // retain the id with this stale answer. Another writer is in the
        // session, so the run is refused as busy.
        if (run) {
            throw new Busy();
        }
        snapshot = r.val;
    }
    throw new Busy();
}

// The agent.compact-result.v1 entry for requested compaction `compact`.
export function compactResult(compact, outcome, detail = {}, compacted = false) {
    return entry(COMPACT_RESULT_KIND, compactResultData(compact, outcome, detail, compacted));
}

// Accept a requested compaction: refuse an id the journal already has an
// agent.compact.v1 for (Duplicate), close the ops a run cut off left
// unresolved, and commit agent.compact.v1 under the commit id
// `compact:<id>`. The host answers a commit id it has seen with the original
// result rather than writing, and that result's journal end is not past the
// end read here, which is a retained duplicate too. A lost race is Busy.
// Nothing is written on an error. Resolves to { journal, messages, before }:
// the writer, the transcript to summarize (null when there is none, or none
// this build can read), and the last seq before the person's latest message
// in it.
export async function openCompaction(id, focus) {
    const snapshot = await store.current();
    const all = await readFrom(0);
    if (compactionAccepted(all, id)) {
        throw new Duplicate();
    }
    const through = snapshot.state ? snapshot.state.through : { tag: 'empty' };
    const after = startAfter(through);
    const entries = resolutionEntries(unresolvedOps(all.filter((e) => Number(e.seq) >= after))).map(([kind, data]) =>
        entry(kind, data),
    );
    entries.push(entry(COMPACT_KIND, { compact: id, ...(focus ? { focus } : {}) }));
    const version = snapshot.state ? snapshot.state.version : null;
    const r = await commit(entries, undefined, condition(version), snapshot.end, `compact:${id}`);
    if (r.tag !== 'committed') {
        throw new Busy();
    }
    if (!later(r.val.end, snapshot.end)) {
        throw new Duplicate();
    }
    const { messages, unreadable } = saved(snapshot);
    const covered = through.tag === 'at' ? Number(through.val) : null;
    return {
        journal: new Journal(version, r.val.end, messages.length),
        messages: snapshot.state && !unreadable ? messages : null,
        before: covered != null ? beforeLastPerson(all, covered) : null,
    };
}

export async function steps() {
    return stepsOf(await readFrom(0));
}

export async function clear() {
    await store.clear();
}
