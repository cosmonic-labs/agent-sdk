/*
 * The harness-neutral half of the cosmonic:agent/run and
 * cosmonic:agent/compact exports, shared by the JavaScript harnesses:
 * the checks made before anything is written, the counts the events and
 * outcomes carry, and the ordered writer for a run's event stream. No host
 * imports, so they run under Node's tests.
 */

// Each application owns one slot for runs and compactions from all its adapters.
export function createRunLock() {
    let held = false;

    // A busy caller leaves ownership with the active run.
    return {
        acquire() {
            if (held) {
                return false;
            }
            held = true;
            return true;
        },
        release() {
            held = false;
        },
    };
}

// The commit ids `run:<id>` and `compact:<id>` must fit the session's 128 bytes.
export const MAX_RUN_ID_BYTES = 124;
export const MAX_COMPACT_ID_BYTES = 120;

// The longest focus taken, in characters: the interface's recommended cap.
export const MAX_FOCUS_CHARS = 2000;

const enc = new TextEncoder();

// A start-error, thrown as the variant itself: componentize-qjs lowers a
// thrown plain object as the export's `err`.
export const startError = (tag, val) => (val === undefined ? { tag } : { tag, val });

// Refuse an id (a run's or a compaction's, as `what` says) that does not fit
// its commit id of `max` bytes or has control characters.
export function checkId(id, what, max) {
    if (!id || enc.encode(id).length > max) {
        throw startError('invalid-input', `a ${what} id is 1 to ${max} bytes`);
    }
    // oxlint-disable-next-line no-control-regex -- refusing control characters is the point
    if (/[\u0000-\u001f\u007f]/.test(id)) {
        throw startError('invalid-input', `a ${what} id has no control characters`);
    }
}

// Refuse a max-steps of zero.
export function checkMaxSteps(options) {
    if (options.maxSteps === 0) {
        throw startError('invalid-input', 'max-steps is at least 1');
    }
}

// Refuse a schema that is not JSON.
export function checkSchema(schema) {
    if (schema != null) {
        try {
            JSON.parse(schema);
        } catch {
            throw startError('invalid-input', 'the schema is not JSON');
        }
    }
}

// Check what the driver sent for a compaction, before anything is written.
// The schema and the run's instructions do not apply and are ignored. A
// summary is one model call, so max-steps defaults to 1.
export function compactRequest(focus, options) {
    const id = String(options?.runId ?? '');
    checkId(id, 'compaction', MAX_COMPACT_ID_BYTES);
    const kept = typeof focus === 'string' && focus.trim() ? focus : null;
    if (kept && [...kept].length > MAX_FOCUS_CHARS) {
        throw startError('invalid-input', `a focus is at most ${MAX_FOCUS_CHARS} characters`);
    }
    checkMaxSteps(options);
    return {
        id,
        focus: kept,
        maxSteps: options.maxSteps ?? 1,
        maxTotalTokens: options.maxTotalTokens ?? null,
        maxDuration: options.maxDuration ?? null,
        traceParent: options.traceParent ?? null,
    };
}

// The start-error for a run or compaction the session did not accept: the
// harness's own Duplicate and Busy errors, or a store failure, which
// `describe` puts into words.
export function refusal(error, { Busy, Duplicate, describe }) {
    if (error instanceof Duplicate) {
        return startError('duplicate');
    }
    if (error instanceof Busy) {
        return startError('busy');
    }
    return startError('unavailable', `the session store failed: ${describe(error)}`);
}

// Run-types usage for `counts` ({input, output}): the host reports no cache
// or reasoning split.
export const usageOf = (counts) => ({
    promptTokens: counts.input,
    cachedPromptTokens: undefined,
    cacheWriteTokens: undefined,
    completionTokens: counts.output,
    reasoningTokens: undefined,
});

// Run-types counts for `counts` ({steps, toolCalls, input, output}).
export const countsOf = (counts) => ({ steps: counts.steps, toolCalls: counts.toolCalls, usage: usageOf(counts) });

// A run's events, written one at a time in order. Each `put` resolves false
// once the driver has dropped the stream; `onGone` is called when that is
// first seen. The runtime cannot report a dropped reader without a write, so
// with `heartbeatMs` a `heartbeat()` writes an `other` event (which drivers
// ignore) once the stream has been quiet that long; without it, `heartbeat()`
// does nothing.
export function eventWriter(writable, { onGone, heartbeatMs } = {}) {
    let alive = true;
    let chain = Promise.resolve(true);
    let wrote = Date.now();
    const put = (event) => {
        wrote = Date.now();
        chain = chain.then(async () => {
            if (!alive) {
                return false;
            }
            try {
                alive = (await writable.write([event])) === 1;
            } catch {
                alive = false;
            }
            if (!alive) {
                onGone?.();
            }
            return alive;
        });
        return chain;
    };
    return {
        put,
        // Whether the driver still reads the stream, as far as is known.
        get alive() {
            return alive;
        },
        heartbeat() {
            if (heartbeatMs != null && alive && Date.now() - wrote >= heartbeatMs) {
                void put({ tag: 'other', val: 'heartbeat' });
            }
        },
        close() {
            try {
                writable.drop();
            } catch {
                /* already gone */
            }
        },
    };
}
