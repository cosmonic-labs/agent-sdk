/*
 * The session journal's rules, with no host imports so they can be tested
 * under Node. See session.js for the writer and cosmonic:agent/session's
 * session.wit for the contract these follow.
 */

import { recordedArguments } from './tool-input.js';

export const STEP_KIND = 'agent.step.v1';
export const OP_KIND = 'agent.op.v1';
export const OP_RESULT_KIND = 'agent.op-result.v1';
export const CONTEXT_RESET_KIND = 'agent.context-reset.v1';
// A run of the cosmonic:agent/run export opened ({run}) and ended ({run,
// outcome, finish | error}); see run.wit and session.wit.
export const RUN_KIND = 'agent.run.v1';
export const RUN_RESULT_KIND = 'agent.run-result.v1';

const dec = new TextDecoder();
const enc = new TextEncoder();

// The JSON value of an entry's data (bytes or text), or null when it is not JSON.
export function parse(data) {
    try {
        return JSON.parse(typeof data === 'string' ? data : dec.decode(data));
    } catch {
        return null;
    }
}

// A journal entry to commit: `value` as JSON bytes under `kind`.
/** @type {(kind: string, value: unknown) => { kind: string, data: Uint8Array }} */
export const entry = (kind, value) => ({ kind, data: enc.encode(JSON.stringify(value)) });

// The state condition for a commit that expects `version` (null or undefined: none saved).
/** @type {(version?: string | null) => { tag: 'absent' } | { tag: 'version', val: string }} */
export const condition = (version) => (version == null ? { tag: 'absent' } : { tag: 'version', val: version });

// The first seq after journal end `end`.
/** @type {(end: { tag: 'empty' } | { tag: 'at', val: number | bigint }) => number} */
export const startAfter = (end) => (end.tag === 'empty' ? 0 : Number(end.val) + 1);

// Whether journal end `a` is past `b`.
/** @type {(a: { tag: 'empty' } | { tag: 'at', val: number | bigint }, b: { tag: 'empty' } | { tag: 'at', val: number | bigint }) => boolean} */
export const later = (a, b) => a.tag === 'at' && (b.tag === 'empty' || Number(a.val) > Number(b.val));

// Every journal entry of `store` (a cosmonic:agent/session store) from seq
// `start` on, read a page at a time.
/**
 * @template {{ seq: number | bigint }} T
 * @param {{ read(start: number, limit: number): PromiseLike<T[]> }} store
 * @param {number} [start]
 * @param {number} [page]
 * @returns {Promise<T[]>}
 */
export async function readEntries(store, start = 0, page = 1000) {
    const out = [];
    for (;;) {
        const read = await store.read(start, page);
        if (!read.length) {
            return out;
        }
        out.push(...read);
        start = Number(read[read.length - 1].seq) + 1;
    }
}

// The steps among journal `entries`, oldest first, skipping any that are not JSON.
export function stepsOf(entries) {
    const out = [];
    for (const e of entries) {
        if (e.kind !== STEP_KIND) {
            continue;
        }
        try {
            out.push(JSON.parse(typeof e.data === 'string' ? e.data : dec.decode(e.data)));
        } catch {
            /* skip */
        }
    }
    return out;
}

// The ops in `entries` ({kind, data}) that no op-result resolves, oldest first.
export function unresolvedOps(entries) {
    let open = [];
    for (const { kind, data } of entries) {
        if (kind !== OP_KIND && kind !== OP_RESULT_KIND) {
            continue;
        }
        const v = parse(data);
        if (!v || typeof v.op !== 'string') {
            continue;
        }
        if (kind === OP_KIND) {
            open.push({ op: v.op, target: String(v.target ?? ''), name: String(v.name ?? '') });
        } else {
            open = open.filter((u) => u.op !== v.op);
        }
    }
    return open;
}

// What to commit for ops a previous run left unresolved. A model call may be
// repeated (the state predates it, so this turn's model call is the repeat);
// its op is closed as cancelled. A tool call is never repeated on this
// agent's say-so: repeating needs the workload's configuration to say the
// tool recognizes repeats, which this agent does not read, and a tool's
// annotations never authorize it. It is closed as unresolved, with an error
// step naming the call, and left to a person.
export function resolutionEntries(unresolved) {
    const out = [];
    for (const u of unresolved) {
        if (u.target === 'model') {
            out.push([OP_RESULT_KIND, { op: u.op, outcome: 'cancelled' }]);
            continue;
        }
        out.push([OP_RESULT_KIND, { op: u.op, outcome: 'unresolved' }]);
        out.push([STEP_KIND, unresolvedStep(u.op, u.name)]);
    }
    return out;
}

// The error step that goes with an unresolved tool call, naming it.
function unresolvedStep(op, name) {
    return {
        role: 'error',
        text:
            `The call to ${name || 'a tool'} (call ${op}) ended without a result, so it may or may not have happened. ` +
            'It was not repeated; check its effect before asking again.',
        blocks: ['error'],
        tools: name ? [name] : [],
    };
}

// A step without its images, marked so a reader can say they were dropped;
// null when it had none to drop.
export function stepWithoutImages(data) {
    const v = parse(data);
    if (!v || !Array.isArray(v.images) || v.images.length === 0) {
        return null;
    }
    return enc.encode(JSON.stringify({ ...v, images: [], imagesDropped: true }));
}

// An agent.op.v1 for each tool call in a Pi assistant message. The op is the
// tool call's id, which is also the id the tool is called with. A freeform
// call's arguments are recorded as its text.
export function toolOps(message) {
    return (message?.content ?? [])
        .filter((c) => c.type === 'toolCall')
        .map((c) => ({
            id: String(c.id),
            data: { op: String(c.id), target: 'tool', name: c.name, arguments: recordedArguments(c.arguments) },
        }));
}

// The outcome to record when an op is closed. A call never made was
// cancelled. One made and then dropped before it returned is unresolved: it
// may or may not have taken effect. Otherwise, how it ended.
/* The outcome for a tool call that ended in a call-error. One that was
   cancelled or timed out was made and never returned, so whether it took
   effect is not known: unresolved. Any other error came back from the call:
   failed. */
export function outcomeForCallError(tag) {
    return tag === 'cancelled' || tag === 'timed-out' ? 'unresolved' : 'failed';
}

/* The same for cosmonic:agent/tools@0.4.2, whose call-error says whether the
   call started. `not-dispatched` produced no effects, which the session
   conventions record as cancelled. `outcome-unknown` may have taken effect:
   unresolved. */
export function outcomeForToolError(error) {
    if (error?.tag === 'not-dispatched') {
        return 'cancelled';
    }
    return 'unresolved';
}

export function outcomeOf(status) {
    if (status === 'committed') {
        return 'cancelled';
    }
    if (status === 'dispatched') {
        return 'unresolved';
    }
    return status?.done ?? 'unresolved';
}

// What to commit when this turn closes one of its own ops: the result and,
// when the call is unresolved, an error step naming it.
export function closingEntries(op, name, status) {
    const outcome = outcomeOf(status);
    const out = [[OP_RESULT_KIND, { op, outcome }]];
    if (outcome === 'unresolved') {
        out.push([STEP_KIND, unresolvedStep(op, name)]);
    }
    return out;
}

// Pi's transcript in a form this build can use: an array of messages with
// string roles. Anything else is an upgrade's old format.
export function readableTranscript(value) {
    return Array.isArray(value) && value.every((m) => m && typeof m === 'object' && typeof m.role === 'string');
}
