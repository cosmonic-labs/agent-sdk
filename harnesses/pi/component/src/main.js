/*
 * Pi (pi-agent-core) as a sandboxed agent harness component.
 *
 * Two ways in, one loop:
 *   - the typed cosmonic:agent/run@0.4.2 export (`run`, below): a host calls
 *     run(message, options) and reads a stream of run events and a future of
 *     the outcome;
 *   - an HTTP contract, for hosts that do not call `run`:
 *       POST /task {task}       stream NDJSON lines: step, delta, done, failed
 *       GET  /history           the session's stored steps
 *       DELETE /history         forget the session
 *     Mutating requests need the x-cosmonic-agent-session header (a browser
 *     never sends a custom header cross-origin without a preflight, which the
 *     ingress does not answer).
 * Both run the same loop (`work`) through the same journal entries; only
 * where its progress goes differs (a sink of run events, or of NDJSON lines).
 *
 * What it can reach is what it imports: cosmonic:agent/models and
 * cosmonic:agent/chat for the model (the host holds the key and picks the
 * provider), cosmonic:agent/tools@0.4.2 for tools (the host dispatches them
 * to the components that hold their network grants), cosmonic:agent/session
 * for the journal and the transcript. No outgoing HTTP, no sockets. Nothing
 * is kept in memory between runs: the next one reads the session.
 */

import { runAgentLoop } from '@earendil-works/pi-agent-core';
import { AssistantMessageEventStream, normalizeContext } from '@earendil-works/pi-ai';
import { list as listTools, call as callTool } from 'cosmonic:agent/tools@0.4.2';
import { now } from 'wasi:clocks/monotonic-clock@0.3.0';

import {
    MAX_RUN_ID_BYTES,
    checkId,
    checkMaxSteps,
    checkSchema,
    countsOf,
    eventWriter,
    refusal,
    startError,
} from '../../../shared/js/run-rules.js';
import { agentSchema, rememberListed, toolArguments } from '../../../shared/js/tool-input.js';
import { abortable, typeConst } from '../../../shared/js/wit.js';
import {
    SUMMARY_SYSTEM,
    View,
    beforeCall,
    charged,
    compacted,
    compactionInfo,
    compactWithinLimits,
    entryData,
    estimateTokens,
    limitReached,
    pipeModelEvents,
    plan,
    transcriptText,
} from './compaction.js';
import { outcomeForToolError, toolOps } from './journal-rules.js';
import {
    MODEL,
    completeOnce,
    contextWindow,
    errorEvent,
    hostStreamFn,
    lastHostFailure,
    overflowed,
    resolved,
    setTraceParent,
    toWitMessage,
} from './llm.js';
import { acquire, release } from './lock.js';
import { Busy, Duplicate, Superseded, clear, describe, openTurn, randomId, runResult, steps } from './session.js';

const SESSION_HEADER = 'x-cosmonic-agent-session';
const SYSTEM =
    'You are Pi, a helpful assistant running in a sandbox. Use the tools you are given when they help; ' +
    'say so plainly when a request needs something you cannot reach.';

const enc = new TextEncoder();

function json(status, value) {
    return new Response(JSON.stringify(value), {
        status,
        headers: { 'content-type': 'application/json', 'cache-control': 'no-store' },
    });
}

const errorStep = (text, run) => ({ role: 'error', text, blocks: ['error'], tools: [], ...(run ? { run } : {}) });

/* ---- tools (cosmonic:agent/tools@0.4.2) ---- */

async function agentTools(journal) {
    // `list` is fallible since 0.4.0: an error is thrown with its list-error.
    const defs = await listTools();
    // Each tool's input as listed, for the model's request: Pi's own tool
    // holds a JSON Schema only, and a freeform tool's text rides in it.
    rememberListed(defs);
    return defs.map((t) => ({
        name: t.name,
        label: t.title || t.name,
        description: t.description,
        parameters: agentSchema(t.input),
        async execute(id, params, signal) {
            const callId = String(id ?? '');
            // The op for this call is already in the journal: the reply that asked
            // for it was committed, with an op per call, before Pi runs its tools.
            // A call a restart could not account for is not made.
            if (!journal.dispatch(callId)) {
                throw new Error(`${t.name}: the call was not recorded in the session, so it was not made`);
            }
            let r;
            try {
                // The model's own tool call; its id names the operation. Stopping
                // the run drops the pending call, which asks the host to cancel it.
                r = await abortable(callTool({ id: callId, name: t.name, arguments: toolArguments(params) }), signal);
            } catch (e) {
                // not-dispatched: nothing ran. outcome-unknown, or a cancellation:
                // whether it took effect is not known.
                journal.finish(callId, e?.payload ? outcomeForToolError(e.payload) : 'unresolved');
                throw new Error(describe(e));
            }
            journal.finish(callId, r.isError ? 'failed' : 'succeeded');
            const content = r.output.map((o) => {
                if (o.tag === 'text') {
                    return { type: 'text', text: o.val };
                }
                const { mediaType, source } = o.val;
                // A URL is a fetch the host would make for the agent; never followed.
                if (source.tag !== 'bytes') {
                    return { type: 'text', text: `[${mediaType} attachment by URL, not fetched]` };
                }
                // Pi's tool result holds images and text only; other media is named.
                if (mediaType.startsWith('image/')) {
                    return { type: 'image', mimeType: mediaType, data: bytesToB64(source.val) };
                }
                return { type: 'text', text: `[${mediaType} attachment, ${source.val.length} bytes, not shown]` };
            });
            if (r.isError) {
                throw new Error(
                    content
                        .filter((c) => c.type === 'text')
                        .map((c) => c.text)
                        .join('\n') || 'the tool failed',
                );
            }
            return { content, details: r.structured ? JSON.parse(r.structured) : undefined };
        },
    }));
}

function bytesToB64(bytes) {
    let s = '';
    for (let i = 0; i < bytes.length; i += 0x8000) {
        s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    }
    return btoa(s);
}

/* ---- steps, in the journal's shape (agent.step.v1) ---- */

function stepOf(m) {
    if (m.role === 'assistant') {
        if (m.stopReason === 'error') {
            return errorStep(m.errorMessage || 'the model call failed');
        }
        const text = textOf(m);
        const tools = m.content.filter((c) => c.type === 'toolCall').map((c) => c.name);
        return {
            role: 'assistant',
            text,
            blocks: [...(text ? ['text'] : []), ...tools.map(() => 'toolRequest')],
            tools,
        };
    }
    if (m.role === 'toolResult') {
        const images = m.content.filter((c) => c.type === 'image').map((c) => `data:${c.mimeType};base64,${c.data}`);
        return {
            role: 'tool',
            text: m.content
                .filter((c) => c.type === 'text')
                .map((c) => c.text)
                .join('\n'),
            blocks: ['toolResponse', ...(images.length ? ['image'] : [])],
            tools: [],
            images,
        };
    }
    return null;
}

// The text parts of a Pi assistant message, joined.
function textOf(m) {
    return (m?.content ?? [])
        .filter((c) => c.type === 'text')
        .map((c) => c.text)
        .join('\n');
}

/* ---- the loop ---- */

// Why a run that started ended without a reply: the run-error case for the
// typed export, the `error` field of agent.run-result.v1, and text for a
// person.
class Failure extends Error {
    constructor(tag, message, val) {
        super(message);
        this.tag = tag;
        this.val = val;
    }

    get variant() {
        if (this.tag === 'cancelled' || this.tag === 'superseded') {
            return { tag: this.tag };
        }
        return { tag: this.tag, val: this.val ?? this.message };
    }
}

// A failed model call, from what the host said about it (see llm.js).
function modelFailure(m) {
    const text = m.errorMessage || 'the model call failed';
    const host = lastHostFailure();
    if (host?.stage === 'open') {
        return new Failure('no-model', `no model for the alias ${MODEL.id}: ${text}`);
    }
    if (host?.stage === 'chat' && host.payload) {
        return new Failure('model-refused', text, host.payload);
    }
    if (host?.stage === 'generation' && host.payload) {
        return new Failure('model-failed', text, host.payload);
    }
    return new Failure('failed', text);
}

// The driver's instructions, and the schema the reply must satisfy, as one
// system message ahead of Pi's own for this run's model calls. Not stored:
// the driver sends them with every run.
function runInstructions(req) {
    const schema = req.schema
        ? `Your final answer must be only a JSON value that satisfies this JSON Schema, with no other text: ${req.schema}`
        : null;
    const text = [req.instructions, schema].filter(Boolean).join('\n\n');
    return text ? { role: 'system', content: text, timestamp: Date.now() } : null;
}

// The reply as JSON, with a fenced code block around it taken off. Whether it
// satisfies the schema is the driver's to check: Pi carries no JSON Schema
// validator here.
function structured(answer) {
    const trimmed = answer.trim();
    const fenced = trimmed.match(/^```(?:json)?\s*([\s\S]*?)\s*```$/);
    const inner = (fenced ? fenced[1] : trimmed).trim();
    try {
        JSON.parse(inner);
        return inner;
    } catch (e) {
        throw new Failure('output-rejected', `the reply did not match the schema: not JSON (${e.message})`);
    }
}

// Open a run: list the tools, then commit the person's message and
// agent.run.v1 under the commit id `run:<run-id>`. Throws Busy, Duplicate,
// or the session's error, having written nothing.
async function accept(req) {
    // Nothing has happened outside the session until the person's message is
    // committed, so listing tools first is safe.
    let tools = [];
    let toolsError = null;
    let journalRef = null;
    try {
        tools = await agentTools({
            dispatch: (id) => journalRef?.dispatch(id) ?? false,
            finish: (id, o) => journalRef?.finish(id, o),
        });
    } catch (e) {
        toolsError = `listing tools failed, so this run has none: ${describe(e)}`;
    }
    const system = {
        role: 'system',
        content: SYSTEM,
        toolsAdded: tools.map(({ name, description, parameters }) => ({ name, description, parameters })),
        timestamp: Date.now(),
    };
    const userStep = { role: 'user', text: req.text, blocks: req.blocks, tools: [], run: req.run };
    // Committed before the run, so a run that fails, is stopped, or loses its
    // client still shows what was asked.
    const turn = await openTurn(req.user, userStep, (prior) => (prior.length ? prior : [system]), req.run);
    journalRef = turn.journal;
    const deadline = req.maxDuration != null ? now() + req.maxDuration : null;
    return { ...req, turn, tools, toolsError, deadline };
}

// The tool loop: Pi calls the model, makes the tool calls it asks for, hands
// it the results and repeats until it answers. Every step is committed before
// the sink sees it, and the run's last commit comes before this returns, so a
// driver that redraws from the journal on the outcome finds the run recorded.
// Returns { status: 'done' | 'failed' | 'cancelled', ... }.
async function work(acc, sink) {
    const { turn, run } = acc;
    const { journal } = turn;
    setTraceParent(acc.traceParent);

    // The sink returning false means the reader went away: the run stops its
    // model and tool calls and ends cancelled. A failure to record aborts too,
    // and then the reader is still there to be told why, so the two are kept
    // apart.
    const stop = new AbortController();
    let gone = false;
    const tell = async (send) => {
        if (gone) {
            return;
        }
        if (!(await send())) {
            gone = true;
            stop.abort();
        }
    };

    const counts = { steps: 0, toolCalls: 0, input: 0, output: 0 };
    if (acc.toolsError) {
        const step = errorStep(acc.toolsError, run);
        await journal.record({ step }).catch((e) => console.error(`session: ${describe(e)}`));
        await tell(() => sink.step(step, null));
    }

    const context = { messages: turn.context, tools: acc.tools };
    const base = context.messages.length; // where the person's message lands
    const instructions = runInstructions(acc);
    const convertToLlm = (ms) => {
        const llm = ms.filter((m) => ['system', 'user', 'assistant', 'toolResult'].includes(m.role));
        return instructions ? [instructions, ...llm] : llm;
    };
    // The loop's transcript, message for message (the person's message
    // included), so a compaction's view applies to it as to the loop's own.
    const progress = [...context.messages];
    let failure = null;
    // A limit in the options that ended the run, and the transcript as of the
    // reply that reached it (its tool calls, never made, left out), or as of
    // the model call a summary left no room for.
    let limit = null;
    let limited = null;
    let transcript = null;

    // Compaction (compaction.js). pi-agent-core keeps its own full transcript
    // and hands it to transformContext before every model call; `view` stands
    // in a summary for its older part there and in every transcript saved.
    const window = await contextWindow();
    const view = new View();
    let seen = null; // the loop's transcript as the last model call was built from it
    let overflowRetry = true; // an overflow is answered by compacting once per run

    // A limit reached before a model call (after a summary, or before one)
    // ends the run there, with what it has: the call is not made.
    const stopAt = (reason) => {
        limit = reason;
        limited = [...progress];
        stop.abort();
    };

    // Summarize the transcript before the person's latest message. The model
    // call is an op in the journal before it is made; its result lands with
    // agent.compaction.v1 and the smaller transcript in one commit. Returns the
    // transcript the model is given now, or null when nothing was summarized
    // (no plan, a failed summary, or a smaller transcript too large to save).
    // Throws Superseded when another writer changed the session meanwhile:
    // compactOrStop then ends the run before its next model or tool call.
    const compactNow = async (full, why) => {
        const viewed = view.apply(full);
        const p = plan(viewed);
        if (!p || journal.hasOpenOps()) {
            return null;
        }
        const op = `compact-${randomId()}`;
        await journal.beginModelOp(op, 'compaction');
        let summary;
        try {
            // The summarizer gets at most 60 percent of the window as input.
            // The call counts against the run's limits like any other.
            const maxChars = window ? Math.floor(window * 0.6 * 4) : Infinity;
            summary = await charged(counts, () => completeOnce(SUMMARY_SYSTEM, transcriptText(p.older, maxChars)));
            // A refused or filtered summary is never committed.
            if (summary.declined) {
                throw new Error(`the summary was ${summary.declined}`);
            }
            if (!summary.text.trim()) {
                throw new Error('the model returned an empty summary');
            }
        } catch (e) {
            console.error(`compaction (${why}): the summary failed: ${describe(e)}`);
            await journal.compacted(op, null, viewed);
            return null;
        }
        const next = compacted(p, summary.text, summary.model);
        const data = entryData({
            trigger: why,
            summary: summary.text,
            model: summary.model,
            throughEnd: journal.beforeTurn(),
            kept: p.kept.length,
            tokensBefore: estimateTokens(viewed),
            tokensAfter: estimateTokens(next),
        });
        if (!(await journal.compacted(op, data, next))) {
            // The smaller transcript was too large to save, so the summary is
            // not used: the run goes on with the transcript it had.
            return null;
        }
        view.replace(full, viewed, p.at, next.slice(0, next.length - p.kept.length));
        // Committed, so the driver may mark the point now; the summary itself
        // is read from the journal. With no range (the turn's start could not
        // be read back) there is nothing to mark it by.
        const info = compactionInfo(data, journal.lastCompactionSeq());
        if (info) {
            await tell(() => sink.compacted(info));
        } else {
            console.error(`compaction (${why}): no journal range to report`);
        }
        return next;
    };

    // A failure to record a compaction ends the run like any other, and so
    // does losing the session to another writer (Superseded): the stop
    // signal is aborted before the call, so hostStreamFn makes no model call
    // and the run ends superseded.
    const compactOrStop = async (full, why) => {
        try {
            return await compactNow(full, why);
        } catch (e) {
            failure =
                e instanceof Superseded
                    ? new Failure('superseded', e.message)
                    : new Failure('not-recorded', `saving the session: ${describe(e)}`);
            stop.abort();
            return null;
        }
    };

    // Near the window, summarize before the call, within the run's limits.
    // First, a run that lost the session (a commit after a lost race, which
    // lands without failing, marked the journal stale) ends superseded here:
    // the stop signal is aborted before the call, as in compactOrStop. Must
    // not throw.
    const transformContext = async (messages) => {
        seen = messages;
        if (failure || limit) {
            return view.apply(messages);
        }
        let r;
        try {
            r = await beforeCall(messages, {
                view,
                window,
                canCompact: !journal.hasOpenOps(),
                counts,
                limits: acc,
                now,
                compact: (full) => compactOrStop(full, 'threshold'),
                lost: () => journal.superseded,
            });
        } catch (e) {
            failure =
                e instanceof Superseded
                    ? new Failure('superseded', e.message)
                    : new Failure('failed', `preparing the model call: ${describe(e)}`);
            stop.abort();
            return view.apply(messages);
        }
        if (r.limit && !failure) {
            stopAt(r.limit);
        }
        return r.messages;
    };

    // Past the window, the host refuses the call: compact once and call again.
    // Only a refusal before any of the reply's text was streamed is retried,
    // so nothing the reader was shown is taken back; the refusal is not
    // passed on. Whatever
    // throws on the way ends the call with an error, never a hung stream.
    const streamFn = (model, llmContext, options) => {
        const out = new AssistantMessageEventStream();
        pipeModelEvents(out, hostStreamFn(model, llmContext, options), {
            shouldRetry: () => overflowRetry && overflowed() && seen && !limit,
            retry: async (ev) => {
                overflowRetry = false;
                // The refused call was a model call too.
                counts.steps++;
                counts.input += ev.error?.usage?.input || 0;
                counts.output += ev.error?.usage?.output || 0;
                const { limit: reached, next } = await compactWithinLimits({
                    counts,
                    limits: acc,
                    now,
                    compact: () => compactOrStop(seen, 'overflow'),
                });
                if (reached && !failure) {
                    // The refusal is passed on, and passed over: the run
                    // ends with the limit.
                    stopAt(reached);
                    return null;
                }
                if (!next) {
                    return null;
                }
                return hostStreamFn(model, normalizeContext({ messages: convertToLlm(next) }), options);
            },
            fail: (e) => errorEvent(`agent failed: ${describe(e)}`),
        });
        return out;
    };

    try {
        const added = await runAgentLoop(
            [acc.user],
            context,
            {
                model: MODEL,
                convertToLlm,
                transformContext,
            },
            async (ev) => {
                if (ev.type === 'message_update' && ev.assistantMessageEvent?.type === 'text_delta') {
                    await tell(() => sink.delta(ev.assistantMessageEvent.delta));
                    return;
                }
                if (ev.type !== 'message_end') {
                    return;
                }
                let m = ev.message;
                // After a failure or a limit the loop winds down with aborted
                // placeholders, which are not steps anyone took.
                if (failure || limit) {
                    return;
                }
                // The person's message was committed when the run opened; it is
                // only kept in the transcript.
                if (m.role === 'user') {
                    progress.push(m);
                    return;
                }
                if (m.role === 'assistant' && m.stopReason === 'error') {
                    failure = modelFailure(m);
                    progress.push(m);
                    return;
                }
                let ops = [];
                if (m.role === 'assistant') {
                    counts.steps++;
                    counts.input += m.usage?.input || 0;
                    counts.output += m.usage?.output || 0;
                    ops = m.stopReason === 'aborted' ? [] : toolOps(m);
                    // A limit reached with tool calls pending ends the run here: the
                    // calls are not recorded, so the tool wrapper refuses them.
                    if (ops.length) {
                        limit = limitReached(counts, acc, now());
                        if (limit) {
                            m = { ...m, content: m.content.filter((c) => c.type !== 'toolCall'), stopReason: 'stop' };
                            ops = [];
                        }
                    }
                } else if (m.role === 'toolResult') {
                    counts.toolCalls++;
                }
                progress.push(m);
                const step = stepOf(m);
                if (step && run) {
                    step.run = run;
                }
                try {
                    if (m.role === 'assistant') {
                        // Committed with its ops before Pi runs the calls (this
                        // callback is awaited first).
                        await journal.record({ step, ops, messages: view.apply(progress) });
                    } else if (m.role === 'toolResult') {
                        await journal.record({
                            step,
                            closes: [String(m.toolCallId)],
                            messages: view.apply(progress),
                        });
                    }
                } catch (e) {
                    failure =
                        e instanceof Superseded
                            ? new Failure('superseded', e.message)
                            : new Failure('not-recorded', `saving the session: ${describe(e)}`);
                    stop.abort();
                    return;
                }
                if (step) {
                    await tell(() => sink.step(step, m));
                }
                if (m.role === 'assistant') {
                    await tell(() => sink.counts(counts));
                }
                if (limit) {
                    limited = [...progress];
                    stop.abort();
                }
            },
            stop.signal,
            streamFn,
        );
        // runAgentLoop works on a copy of the context and returns only what it
        // added (the prompt, the replies and tool results), so the transcript is
        // the old one plus that, as compactions left it.
        transcript = view.apply(limited ?? [...context.messages, ...added]);
        if (!failure && !gone && acc.schema) {
            try {
                acc.structured = structured(textOf(lastAssistant(transcript, view.index(base))));
            } catch (e) {
                failure = e;
            }
        }
    } catch (e) {
        failure = e instanceof Failure ? e : new Failure('failed', describe(e));
    }
    // The loop threw before returning: the transcript as far as it got.
    transcript ??= view.apply(progress);
    // Where the person's message is in the transcript after any compaction.
    const at = view.index(base);

    if (failure) {
        await journal.close([errorStep(failure.message, run)], transcript, [
            runResult(run, 'failed', { error: failure.tag }),
        ]);
        return { status: 'failed', failure, counts };
    }
    if (gone) {
        // The driver stopped the run: keep what it got to.
        await journal.close([errorStep('the run was cancelled', run)], transcript, [
            runResult(run, 'cancelled', { error: 'cancelled' }),
        ]);
        return { status: 'cancelled', counts };
    }
    // Written before `done` and the outcome, so a client that redraws the
    // moment it sees either finds the run already stored.
    await journal.settle(transcript, at);
    const finish = limit ?? 'done';
    await journal.endRun(runResult(run, 'done', { finish }));
    return {
        status: 'done',
        finish,
        transcript,
        reply: lastAssistant(transcript, at),
        structured: acc.structured,
        counts,
    };
}

// The last assistant message this run added.
function lastAssistant(transcript, base) {
    return transcript
        .slice(base)
        .reverse()
        .find((m) => m.role === 'assistant');
}

/* ---- the typed run export (cosmonic:agent/run@0.4.2) ---- */

// Check what the driver sent. Construction is lenient; this is where a
// message or options the agent cannot take are refused, before anything is
// written.
function validate(message, options) {
    const run = String(options?.runId ?? '');
    checkId(run, 'run', MAX_RUN_ID_BYTES);
    const content = [];
    for (const part of message?.content ?? []) {
        if (part.tag === 'text') {
            content.push({ type: 'text', text: part.val });
            continue;
        }
        const { mediaType, source } = part.val;
        // The model call takes attachments by bytes; a URL would have the host
        // fetch on the agent's behalf, which this agent does not ask for.
        if (source.tag !== 'bytes') {
            throw startError('invalid-input', 'attachments are taken as bytes, not URLs');
        }
        if (!mediaType.startsWith('image/')) {
            throw startError('invalid-input', `Pi takes images and text, not ${mediaType}`);
        }
        content.push({ type: 'image', mimeType: mediaType, data: bytesToB64(source.val) });
    }
    const text = content
        .filter((c) => c.type === 'text')
        .map((c) => c.text)
        .join('\n');
    if (!text.trim()) {
        throw startError('invalid-input', 'the message has no text');
    }
    checkMaxSteps(options);
    checkSchema(options.schema);
    const images = content.filter((c) => c.type === 'image').length;
    return {
        run,
        text,
        blocks: ['text', ...(images ? ['image'] : [])],
        user: { role: 'user', content, timestamp: Date.now() },
        instructions: options.instructions?.trim() ? options.instructions : null,
        schema: options.schema ?? null,
        maxSteps: options.maxSteps ?? null,
        maxTotalTokens: options.maxTotalTokens ?? null,
        maxDuration: options.maxDuration ?? null,
        traceParent: options.traceParent ?? null,
    };
}

// The run's events, written one at a time in order. Resolves false once the
// driver has dropped the stream.
function eventSink(writable) {
    const writer = eventWriter(writable);
    const { put } = writer;
    const stepEvent = (content) => ({ tag: 'step', val: { content, seq: undefined } });
    return {
        delta: (text) => put({ tag: 'text-delta', val: text }),
        step(step, m) {
            if (!m) {
                return put(stepEvent({ tag: 'error', val: step.text }));
            }
            const message = toWitMessage(m);
            // An assistant reply with nothing left in it (stopped before it said
            // anything, or a limit took its tool calls out) is not a step.
            const empty = !message || (message.tag === 'assistant' && message.val.content.length === 0);
            return empty ? Promise.resolve(writer.alive) : put(stepEvent(message));
        },
        counts: (c) => put({ tag: 'counts', val: countsOf(c) }),
        compacted: (info) => put({ tag: 'compacted', val: info }),
        close: () => writer.close(),
    };
}

// The outcome future's value for how `work` ended.
function outcomeOf(ended) {
    if (ended.status === 'cancelled') {
        return { tag: 'err', val: { tag: 'cancelled' } };
    }
    if (ended.status === 'failed') {
        return { tag: 'err', val: ended.failure.variant };
    }
    const text = textOf(ended.reply);
    const model = resolved();
    return {
        tag: 'ok',
        val: {
            reply: text ? [{ tag: 'text', val: text }] : [],
            structured: ended.structured ?? undefined,
            counts: countsOf(ended.counts),
            finish: { tag: ended.finish },
            model: model ? { alias: MODEL.id, model } : undefined,
        },
    };
}

export const runExport = {
    async run(message, options) {
        const req = validate(message, options);
        if (!acquire()) {
            throw startError('busy');
        }
        let acc;
        try {
            acc = await accept(req);
        } catch (e) {
            release();
            throw refusal(e, { Busy, Duplicate, describe });
        }

        const events = wit.Stream(typeConst(wit.Stream, 'RUN_EVENT'));
        const outcome = wit.Future(typeConst(wit.Future, 'RESULT_RUN_OUTCOME_RUN_ERROR'));
        // The run works after `run` returns, while the driver reads the events.
        (async () => {
            const sink = eventSink(events.writable);
            let ended;
            try {
                ended = await work(acc, sink);
            } catch (e) {
                ended = { status: 'failed', failure: new Failure('failed', describe(e)) };
            }
            // The stream closes before the outcome resolves.
            sink.close();
            release();
            try {
                await outcome.writable.write(outcomeOf(ended));
            } catch {
                /* the driver stopped listening */
            }
        })();
        return [events.readable, outcome.readable];
    },
};

/* ---- POST /task: the same run, as NDJSON lines ---- */

const line = (v) => enc.encode(JSON.stringify(v) + '\n');

// Open a run for POST /task under a fresh run id. A lost race is retried with
// a new id, since a run id is never resent with other content.
async function acceptTask(task) {
    for (let i = 0; ; i++) {
        try {
            return await accept({
                run: randomId(),
                text: task,
                blocks: ['text'],
                user: { role: 'user', content: task, timestamp: Date.now() },
            });
        } catch (e) {
            if (!(e instanceof Busy) || i >= 2) {
                throw e;
            }
        }
    }
}

async function runTask(req) {
    if (!req.headers.has(SESSION_HEADER)) {
        return json(403, { error: `missing ${SESSION_HEADER} header` });
    }
    let body;
    try {
        body = await req.json();
    } catch {
        return json(400, { error: 'expected {"task": "..."}' });
    }
    const task = typeof body?.task === 'string' ? body.task : null;
    if (!task) {
        return json(400, { error: 'expected {"task": "..."}' });
    }
    if (!acquire()) {
        return json(409, { error: 'a run is already in progress' });
    }
    let acc;
    try {
        acc = await acceptTask(task);
    } catch (e) {
        release();
        if (e instanceof Busy) {
            return json(409, { error: e.message });
        }
        return json(500, { error: `reading the session: ${describe(e)}` });
    }

    // A host stopping a turn closes this response, which the loop sees as
    // the reader going away: it stops, which drops the chunk stream and
    // cancels generation at the host.
    let gone = false;
    const stream = new ReadableStream({
        cancel() {
            gone = true;
        },
        async start(controller) {
            const send = (v) => {
                if (gone) {
                    return false;
                }
                try {
                    controller.enqueue(line(v));
                    return true;
                } catch {
                    gone = true;
                    return false;
                }
            };
            const sink = {
                delta: async (text) => send({ type: 'delta', text }),
                step: async (step) => send({ type: 'step', ...step }),
                counts: async () => !gone,
                // No compaction line; the journal has agent.compaction.v1.
                compacted: async () => !gone,
            };
            let ended;
            try {
                ended = await work(acc, sink);
            } catch (e) {
                ended = { status: 'failed', failure: new Failure('failed', describe(e)) };
            } finally {
                release();
            }
            if (ended.status === 'failed') {
                send({ type: 'failed', error: ended.failure.message });
            } else if (ended.status === 'done') {
                const { transcript, counts } = ended;
                send({
                    type: 'done',
                    state: transcript,
                    context_messages: transcript.length,
                    answer: textOf(ended.reply),
                    finish: ended.finish,
                    turns: counts.steps,
                    input_tokens: counts.input,
                    output_tokens: counts.output,
                    model: { alias: MODEL.id, provider: 'cosmonic', resolved: resolved() },
                });
            }
            try {
                controller.close();
            } catch {
                /* the reader already went away */
            }
        },
    });
    return new Response(stream, { headers: { 'content-type': 'application/x-ndjson', 'cache-control': 'no-store' } });
}

// Serve the HTTP adapter using the same turn engine as the typed export.
export async function handle(req) {
    const url = new URL(req.url);
    const route = `${req.method} ${url.pathname}`;
    try {
        if (route === 'GET /healthz') {
            return new Response('ok');
        }
        if (route === 'POST /task') {
            return await runTask(req);
        }
        if (route === 'GET /history') {
            return json(200, { steps: await steps() });
        }
        if (route === 'DELETE /history') {
            if (!req.headers.has(SESSION_HEADER)) {
                return json(403, { error: `missing ${SESSION_HEADER} header` });
            }
            await clear();
            return json(200, { cleared: true });
        }
        return new Response('POST /task with {"task": "..."}', { status: 404 });
    } catch (e) {
        return json(500, { error: describe(e) });
    }
}
