/*
 * The cosmonic:agent/compact@0.4.2 export: summarize the transcript now, at
 * the driver's request, between runs. Inputs are the person's optional focus
 * and the run options; the output is a stream of run events and a future of
 * the outcome, and the side effects are the journal entries and the smaller
 * transcript the interface describes:
 *
 *   accept:  [unresolved ops closed, compact: the id and focus]   (commit id compact:<id>)
 *   begin:   [op: target model]                                  (no state)
 *            ... the summary call ...
 *   install: [op-result, compaction, compact-result] + state      (one commit)
 *
 * It shares the run lock (busy while a run or another compaction is in
 * progress), makes the summary call the automatic compaction makes, with the
 * person's focus as their own message after the transcript, and keeps what
 * the automatic compaction keeps: the person's latest message and what
 * followed it, verbatim. Every other ending writes agent.compact-result.v1
 * alone, with compacted false: done when there was nothing to summarize or a
 * limit stopped it, failed otherwise (in the same commit as the summary
 * call's op result, failed, when the call was made). The state is left as it
 * was, and a refused or filtered summary is never committed.
 */

import { now } from 'wasi:clocks/monotonic-clock@0.3.0';

import { compactRequest, countsOf, eventWriter, refusal, startError } from '../../../shared/js/run-rules.js';
import { typeConst } from '../../../shared/js/wit.js';
import {
    FOCUS_NOTE,
    SUMMARY_SYSTEM,
    charged,
    compacted,
    compactionInfo,
    entryData,
    estimateTokens,
    limitReached,
    plan,
    transcriptText,
} from './compaction.js';
import { MODEL, completeOnce, contextWindow, setTraceParent } from './llm.js';
import { acquire, release } from './lock.js';
import { Busy, Duplicate, Superseded, compactResult, describe, openCompaction, randomId } from './session.js';

// How long the stream may stay quiet before a heartbeat is written.
// A dropped reader shows only on a write, and a summary call streams nothing,
// so the heartbeat stops it within a second of the driver going away.
const HEARTBEAT_MS = 1000;

// A compaction that ended done: no reply, no structured answer.
function done(counts, finish, model) {
    return {
        tag: 'ok',
        val: {
            reply: [],
            structured: undefined,
            counts: countsOf(counts),
            finish: { tag: finish },
            model: model ? { alias: MODEL.id, model } : undefined,
        },
    };
}

// A compaction that failed: the run-error case and its detail.
const failed = (tag, val) => ({ tag: 'err', val: val === undefined ? { tag } : { tag, val } });

// End without a summary: agent.compact-result.v1 (after the failed result of
// the summary call's `op`, when it was made), then the outcome.
async function ended(journal, id, op, outcome) {
    if (outcome.tag === 'ok') {
        const finish = outcome.val.finish.tag;
        await journal.endCompaction(op, compactResult(id, 'done', { finish }, false));
    } else {
        const error = outcome.val.tag;
        await journal.endCompaction(
            op,
            compactResult(id, error === 'cancelled' ? 'cancelled' : 'failed', { error }, false),
        );
    }
    return outcome;
}

// A failed summary call, from what the host said about it.
function summaryFailure(e) {
    const host = e?.host;
    if (host?.stage === 'open') {
        return failed('no-model', `no model for the alias ${MODEL.id}: ${describe(e)}`);
    }
    if (host?.stage === 'chat' && host.payload) {
        return failed('model-refused', host.payload);
    }
    if (host?.stage === 'generation' && host.payload) {
        return failed('model-failed', host.payload);
    }
    return failed('failed', describe(e));
}

// A failure to record, as the compaction's ending.
const storeFailure = (e) =>
    e instanceof Superseded ? failed('superseded') : failed('not-recorded', `saving the session: ${describe(e)}`);

async function work(req, opened, sink) {
    const { journal, messages, before } = opened;
    const { id } = req;
    const counts = { steps: 0, toolCalls: 0, input: 0, output: 0 };
    const deadline = req.maxDuration != null ? now() + req.maxDuration : null;
    const limits = { maxSteps: req.maxSteps, maxTotalTokens: req.maxTotalTokens, deadline };
    // Nothing to summarize (no earlier exchange, or only a previous summary):
    // done, with no event and no model call.
    const p = messages ? plan(messages) : null;
    if (!p) {
        return ended(journal, id, null, done(counts, 'done'));
    }
    const limit = limitReached(counts, limits, now());
    if (limit) {
        return ended(journal, id, null, done(counts, limit));
    }

    setTraceParent(req.traceParent);
    const op = `compact-${randomId()}`;
    try {
        await journal.beginModelOp(op, 'compaction');
    } catch (e) {
        return ended(journal, id, null, storeFailure(e));
    }
    // The summarizer gets at most 60 percent of the window as input, as in a
    // run; the focus is the person's own message after it.
    const window = await contextWindow();
    const maxChars = window ? Math.floor(window * 0.6 * 4) : Infinity;
    const transcript = transcriptText(p.older, maxChars);
    const user = req.focus ? [`${transcript}\n\n${FOCUS_NOTE}`, req.focus] : transcript;
    let summary;
    try {
        summary = await charged(counts, () => completeOnce(SUMMARY_SYSTEM, user, sink.signal));
    } catch (e) {
        // The driver went away: the call stopped, and nothing is committed.
        return ended(journal, id, op, sink.signal.aborted ? failed('cancelled') : summaryFailure(e));
    }
    // A refused or filtered summary is never committed.
    if (summary.declined) {
        return ended(journal, id, op, failed('failed', `the summary was ${summary.declined}`));
    }
    if (!summary.text.trim()) {
        return ended(journal, id, op, failed('failed', 'the model returned an empty summary'));
    }

    const next = compacted(p, summary.text, summary.model);
    const data = entryData({
        trigger: 'requested',
        summary: summary.text,
        model: summary.model,
        throughEnd: before,
        kept: p.kept.length,
        tokensBefore: estimateTokens(messages),
        tokensAfter: estimateTokens(next),
        focus: req.focus,
    });
    let installed;
    try {
        installed = await journal.compacted(op, data, next, [compactResult(id, 'done', { finish: 'done' }, true)]);
    } catch (e) {
        return ended(journal, id, null, storeFailure(e));
    }
    if (!installed) {
        return ended(journal, id, null, failed('not-recorded', 'the smaller transcript is too large to save'));
    }

    // Recorded: the outcome is done whether or not the driver still reads.
    const info = compactionInfo(data, journal.lastCompactionSeq());
    if (info && (await sink.put({ tag: 'compacted', val: info }))) {
        await sink.put({ tag: 'counts', val: countsOf(counts) });
    }
    return done(counts, 'done', summary.model);
}

export const compactExport = {
    async compact(focus, options) {
        // Checked before anything is written; the schema and the run's
        // instructions do not apply to a compaction and are ignored.
        const req = compactRequest(focus, options);
        if (!acquire()) {
            throw startError('busy');
        }
        let opened;
        try {
            opened = await openCompaction(req.id, req.focus);
        } catch (e) {
            release();
            throw refusal(e, { Busy, Duplicate, describe });
        }

        const stream = wit.Stream(typeConst(wit.Stream, 'RUN_EVENT'));
        const outcome = wit.Future(typeConst(wit.Future, 'RESULT_RUN_OUTCOME_RUN_ERROR'));
        // The compaction works after `compact` returns, while the driver reads
        // the events.
        (async () => {
            // The events, written one at a time in order; `signal` aborts once
            // the driver has dropped them.
            const gone = new AbortController();
            const sink = Object.assign(
                eventWriter(stream.writable, { onGone: () => gone.abort(), heartbeatMs: HEARTBEAT_MS }),
                { signal: gone.signal },
            );
            const beat = setInterval(() => sink.heartbeat(), HEARTBEAT_MS);
            let out;
            try {
                out = await work(req, opened, sink);
            } catch (e) {
                out = failed('failed', describe(e));
            }
            // The stream closes before the outcome resolves.
            clearInterval(beat);
            sink.close();
            release();
            try {
                await outcome.writable.write(out);
            } catch {
                /* the driver stopped listening */
            }
        })();
        return [stream.readable, outcome.readable];
    },
};
