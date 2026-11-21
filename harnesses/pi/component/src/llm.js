/*
 * Pi's model boundary, served by the host.
 *
 * pi-agent-core asks a StreamFn for each assistant turn. Pi's own StreamFns
 * open sockets to providers; this one calls `cosmonic:agent/chat`, so the agent
 * holds no key, names no endpoint and opens no connection. The host picks the
 * provider and model from the workload's spec, meters the call, and streams
 * the reply back as a stream<chunk>, then the assembled completion as a
 * future. Dropping the chunk stream cancels generation.
 *
 * Translation, both ways, between Pi's transcript and the WIT types:
 *   system      -> message::system
 *   user        -> message::user (text, and images as inline bytes)
 *   assistant   -> message::assistant (text, tool-use, signed thinking as an
 *                  opaque block so it survives a tool loop)
 *   toolResult  -> message::tool
 */

import { AssistantMessageEventStream, getCurrentTools } from '@earendil-works/pi-ai';
import { chat } from 'cosmonic:agent/chat@0.4.2';
import { describe as describeModel, open } from 'cosmonic:agent/models@0.4.2';

import { agentArguments, offeredInput, toolArguments } from '../../../shared/js/tool-input.js';

export const MODEL = {
    id: 'default',
    name: 'Host-resolved model',
    api: 'cosmonic-llm',
    provider: 'cosmonic',
    baseUrl: '',
    input: ['text', 'image'],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    reasoning: false,
    // Unknown (0, as pi-agent-core has it) until the host reports the window:
    // see contextWindow(), which each run asks.
    contextWindow: 0,
    maxTokens: 8192,
};

// The model's context window as the host reports it for the alias, or null
// when the host does not know it. Asked at the start of each run, since the
// alias can be repointed between runs; also kept on MODEL.
export async function contextWindow() {
    try {
        const window = (await describeModel(MODEL.id)).contextLength ?? null;
        MODEL.contextWindow = window ?? 0;
        return window;
    } catch {
        return null;
    }
}

// Whether the last model call was refused or failed because the conversation
// is longer than the model's context window.
export function overflowed() {
    return hostFailure?.payload?.tag === 'context-length-exceeded';
}

// One model call that is not a turn of the conversation (a compaction's
// summary): the same alias and host call as every turn, no tools, and the
// collected reply. `user` is one user message's text, or several (each its
// own message). Resolves to { text, usage, model, declined }, `declined`
// being 'refused' or 'filtered' when the model or a provider filter declined
// the reply; throws with the host's reason, and that call's own host failure
// as `host`. Aborting `signal` stops the call.
export async function completeOnce(system, user, signal) {
    // The run's own last failure is what reports how the run went, so this
    // call does not replace it.
    const before = hostFailure;
    let m;
    let own = null;
    try {
        const stream = hostStreamFn(
            MODEL,
            {
                messages: [
                    { role: 'system', content: system, timestamp: Date.now() },
                    ...[user].flat().map((content) => ({ role: 'user', content, timestamp: Date.now() })),
                ],
            },
            { signal },
        );
        m = await stream.result();
        own = hostFailure;
    } finally {
        hostFailure = before;
    }
    if (m.stopReason === 'error' || m.stopReason === 'aborted') {
        // What the failed call spent goes with the error, so it is charged.
        throw Object.assign(new Error(m.errorMessage || 'the model call failed'), { usage: m.usage, host: own });
    }
    const text = m.content
        .filter((c) => c.type === 'text')
        .map((c) => c.text)
        .join('\n');
    return { text, usage: m.usage, model: m.responseModel || resolvedModel || MODEL.id, declined: m.declined ?? null };
}

let resolvedModel = null;
export const resolved = () => resolvedModel;

// The W3C traceparent the current run's driver sent, passed on in each model
// call's request-meta. One run at a time, so one value.
let traceParent;
export function setTraceParent(value) {
    traceParent = value || undefined;
}

// Why the last model call failed, as the host said it, for a typed run error:
// `open` refused the alias (no-model), `chat` refused the request
// (model-refused, a request-error) or the generation failed after it was
// accepted (model-failed, a generation-error). Pi's loop only sees an error
// message, so the case is kept here, and cleared when the next call starts.
let hostFailure = null;
export const lastHostFailure = () => hostFailure;

function b64ToBytes(b64) {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) {
        out[i] = bin.charCodeAt(i);
    }
    return out;
}

function textOf(content) {
    return typeof content === 'string'
        ? content
        : content
              .filter((c) => c.type === 'text')
              .map((c) => c.text)
              .join('\n');
}

export function toWitMessage(m) {
    switch (m.role) {
        case 'system':
            return { tag: 'system', val: textOf(m.content) };
        case 'user': {
            const parts = typeof m.content === 'string' ? [{ type: 'text', text: m.content }] : m.content;
            return {
                tag: 'user',
                val: {
                    content: parts.map((p) =>
                        p.type === 'image'
                            ? {
                                  tag: 'attachment',
                                  val: {
                                      mediaType: p.mimeType,
                                      source: { tag: 'bytes', val: b64ToBytes(p.data) },
                                      displayName: undefined,
                                  },
                              }
                            : { tag: 'text', val: p.text },
                    ),
                    name: undefined,
                },
            };
        }
        case 'assistant':
            // A turn that failed or was stopped before it produced anything is not
            // something the model said; an empty assistant message is refused.
            if (
                (m.stopReason === 'error' || m.stopReason === 'aborted') &&
                !m.content?.some((c) => c.text || c.type !== 'text')
            ) {
                return null;
            }
            return {
                tag: 'assistant',
                val: {
                    content: m.content.flatMap((c) => {
                        if (c.type === 'text') {
                            return c.text
                                ? [{ content: { tag: 'text', val: c.text }, opaque: undefined, citations: [] }]
                                : [];
                        }
                        if (c.type === 'toolCall') {
                            return [
                                {
                                    content: {
                                        tag: 'tool-use',
                                        val: { id: c.id, name: c.name, arguments: toolArguments(c.arguments) },
                                    },
                                    opaque: undefined,
                                    citations: [],
                                },
                            ];
                        }
                        if (c.type === 'thinking' && c.thinkingSignature) {
                            const block = c.redacted
                                ? { type: 'redacted_thinking', data: c.thinkingSignature }
                                : { type: 'thinking', thinking: c.thinking, signature: c.thinkingSignature };
                            return [
                                {
                                    content: { tag: 'other', val: { kind: block.type, json: JSON.stringify(block) } },
                                    opaque: undefined,
                                    citations: [],
                                },
                            ];
                        }
                        return [];
                    }),
                },
            };
        case 'toolResult':
            return {
                tag: 'tool',
                val: [
                    {
                        callId: m.toolCallId,
                        isError: Boolean(m.isError),
                        output: m.content.map((c) =>
                            c.type === 'image'
                                ? {
                                      tag: 'attachment',
                                      val: {
                                          mediaType: c.mimeType,
                                          source: { tag: 'bytes', val: b64ToBytes(c.data) },
                                          displayName: undefined,
                                      },
                                  }
                                : { tag: 'text', val: c.text },
                        ),
                    },
                ],
            };
        default:
            return null;
    }
}

function chatOptions(tools, options) {
    return {
        maxTokens: options?.maxTokens,
        temperature: options?.temperature,
        topK: undefined,
        topP: undefined,
        minP: undefined,
        frequencyPenalty: undefined,
        presencePenalty: undefined,
        repeatPenalty: undefined,
        penaltyLastN: undefined,
        logitBias: [],
        seed: undefined,
        stop: [],
        responseFormat: { tag: 'text' },
        parallelToolCalls: undefined,
        meta: { traceParent, deadlineMs: undefined, cacheKey: undefined, labels: [], cachePoints: [] },
        tools: tools.map((t) => ({
            name: t.name,
            title: undefined,
            description: t.description,
            // As the host listed it: a freeform tool stays freeform.
            input: offeredInput(t),
            outputSchema: undefined,
        })),
        toolChoice: tools.length ? { tag: 'auto' } : undefined,
        thinking: undefined,
        effort: undefined,
        onUnrepresentable: 'adapt',
    };
}

const STOP = { 'tool-calls': 'toolUse', 'max-tokens': 'length', 'context-length': 'length' };

function emptyMessage() {
    return {
        role: 'assistant',
        content: [],
        api: MODEL.api,
        provider: MODEL.provider,
        model: MODEL.id,
        usage: {
            input: 0,
            output: 0,
            cacheRead: 0,
            cacheWrite: 0,
            totalTokens: 0,
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
        },
        stopReason: 'stop',
        timestamp: Date.now(),
    };
}

// An error event ending a model call's stream, for a failure outside the
// host call itself (the agent's own code around it).
export function errorEvent(message) {
    return { type: 'error', reason: 'error', error: { ...emptyMessage(), stopReason: 'error', errorMessage: message } };
}

/* A host error, as text a person can act on. A top-level WIT error is thrown
   as an Error whose payload is the WIT variant. */
function describe(e) {
    const p = e?.payload;
    if (p && typeof p === 'object' && 'tag' in p) {
        const v = p.val;
        const detail = typeof v === 'string' ? v : v?.detail || JSON.stringify(v);
        return `${p.tag}: ${detail}`;
    }
    return String(e?.message || e);
}

// The generated type constant for a stream or future payload, by the name the
// shim gives it. Looked up rather than spelled out because the shim qualifies
// a name that would collide with another package's.
function typeConst(kind, ...names) {
    const types = kind.types ?? {};
    for (const n of names) {
        if (n in types) {
            return types[n];
        }
        if (n in kind) {
            return kind[n];
        }
    }
    const k = Object.keys(types).find((key) => names.some((n) => key.endsWith(`_${n}`)));
    if (k) {
        return types[k];
    }
    throw new Error(`no ${names[0]} type in this build; have ${Object.keys(types).join(', ')}`);
}

// Drop a host handle now rather than whenever the GC gets to it.
function dispose(handle) {
    try {
        handle?.[Symbol.dispose]?.();
    } catch {
        /* already gone, or an operation is still settling */
    }
}

class Aborted extends Error {}

// Start a reply. The conversation goes in as a stream<message> written while
// `chat` is pending (streams do not buffer), then `messages-done` says it is
// complete.
async function startChat(model, messages, options) {
    const { readable: msgRx, writable: msgTx } = wit.Stream(typeConst(wit.Stream, 'MESSAGE'));
    const { readable: doneRx, writable: doneTx } = wit.Future(
        typeConst(wit.Future, 'RESULT_UNIT_STRING', 'RESULT_VOID_STRING'),
    );
    const write = (async () => {
        let written = 0;
        try {
            written = await msgTx.writeAll(messages);
        } catch {
            /* reader gone; reported below */
        }
        dispose(msgTx);
        const verdict =
            written === messages.length
                ? { tag: 'ok' }
                : { tag: 'err', val: `${messages.length - written} messages were not delivered` };
        try {
            await doneTx.write(verdict);
        } catch {
            /* reader gone */
        }
    })();
    const reply = chat(model, msgRx, doneRx, options);
    const [started] = await Promise.allSettled([reply, write]);
    if (started.status === 'rejected') {
        throw started.reason;
    }
    return started.value; // [stream<chunk>, future<result<completion, generation-error>>]
}

export function hostStreamFn(_model, context, options) {
    const out = new AssistantMessageEventStream();
    (async () => {
        const partial = emptyMessage();
        const signal = options?.signal;
        let model = null;
        let chunks = null;
        let outcome = null;
        // Which host call a failure came from (see lastHostFailure).
        let stage = 'open';
        hostFailure = null;
        // Stopping the turn cancels the pending read, of a chunk or the
        // outcome; the loop then sees the signal, and dropping the stream
        // cancels generation at the host.
        const onAbort = () => {
            for (const pending of [chunks, outcome]) {
                try {
                    pending?.cancelRead();
                } catch {
                    /* nothing pending */
                }
            }
        };
        signal?.addEventListener('abort', onAbort);
        try {
            if (signal?.aborted) {
                throw new Aborted();
            }
            // Host-resolved alias: the agent never lists models or picks one.
            model = await open(MODEL.id);
            stage = 'chat';
            const tools = getCurrentTools(context.messages);
            const messages = context.messages.map(toWitMessage).filter(Boolean);
            [chunks, outcome] = await startChat(model, messages, chatOptions(tools, options));
            stage = 'generation';
            out.push({ type: 'start', partial });
            let text = null;
            for (;;) {
                if (signal?.aborted) {
                    throw new Aborted();
                }
                const { value: chunk, done } = await chunks.next();
                if (signal?.aborted) {
                    throw new Aborted();
                }
                if (done || chunk == null) {
                    break;
                }
                // Only text is shown as it arrives; tool calls, reasoning and whole
                // parts come assembled in the completion.
                if (chunk.delta?.tag !== 'text' || !chunk.delta.val) {
                    continue;
                }
                if (!text) {
                    text = { type: 'text', text: '' };
                    partial.content.push(text);
                    out.push({ type: 'text_start', contentIndex: partial.content.length - 1, partial });
                }
                text.text += chunk.delta.val;
                out.push({
                    type: 'text_delta',
                    contentIndex: partial.content.length - 1,
                    delta: chunk.delta.val,
                    partial,
                });
            }
            dispose(chunks);
            chunks = null;
            // Read after the last delta, as the interface asks.
            const result = await outcome.read();
            if (signal?.aborted) {
                throw new Aborted();
            }
            outcome = null;
            if (result.tag === 'err') {
                throw Object.assign(new Error(describe({ payload: result.val })), { payload: result.val });
            }
            const done = result.val;
            resolvedModel = done.model || resolvedModel;
            if (text) {
                out.push({
                    type: 'text_end',
                    contentIndex: partial.content.indexOf(text),
                    content: text.text,
                    partial,
                });
            }
            for (const part of done.message.content) {
                const c = part.content;
                if (c.tag === 'tool-use') {
                    // Bad JSON becomes {}, which Pi's validation reports; a
                    // freeform call's text rides as one property.
                    const args = agentArguments(c.val.arguments);
                    const call = { type: 'toolCall', id: c.val.id, name: c.val.name, arguments: args };
                    partial.content.push(call);
                    const i = partial.content.length - 1;
                    out.push({ type: 'toolcall_start', contentIndex: i, partial });
                    out.push({ type: 'toolcall_end', contentIndex: i, toolCall: call, partial });
                } else if (c.tag === 'other' && (c.val.kind === 'thinking' || c.val.kind === 'redacted_thinking')) {
                    const b = JSON.parse(c.val.json);
                    partial.content.unshift(
                        b.type === 'thinking'
                            ? { type: 'thinking', thinking: b.thinking, thinkingSignature: b.signature }
                            : { type: 'thinking', thinking: '', thinkingSignature: b.data, redacted: true },
                    );
                } else if ((c.tag === 'text' || c.tag === 'refusal') && !text) {
                    // A backend that streamed no text still has its answer here.
                    partial.content.push({ type: 'text', text: c.val });
                }
            }
            const u = done.usage;
            partial.usage.input = u.promptTokens ?? 0;
            partial.usage.output = u.completionTokens ?? 0;
            partial.usage.totalTokens = partial.usage.input + partial.usage.output;
            partial.responseModel = done.model;
            partial.stopReason = STOP[done.finishReason.tag] || 'stop';
            // Refused (the stop reason, or a refusal part) or filtered: Pi
            // has no stop reason for either, and a summary that ended so is
            // never committed.
            const refusedPart = done.message.content.some((p) => p.content.tag === 'refusal');
            if (done.finishReason.tag === 'refusal' || refusedPart) {
                partial.declined = 'refused';
            } else if (done.finishReason.tag === 'content-filter') {
                partial.declined = 'filtered';
            }
            out.push({ type: 'done', reason: partial.stopReason, message: partial });
            out.end(partial);
        } catch (e) {
            if (e instanceof Aborted || signal?.aborted) {
                partial.stopReason = 'aborted';
                partial.errorMessage = 'stopped';
                out.push({ type: 'error', reason: 'aborted', error: partial });
            } else {
                const payload = e?.payload;
                hostFailure = { stage, payload: payload && typeof payload === 'object' ? payload : undefined };
                partial.stopReason = 'error';
                partial.errorMessage = describe(e);
                out.push({ type: 'error', reason: 'error', error: partial });
            }
            out.end(partial);
        } finally {
            signal?.removeEventListener('abort', onAbort);
            dispose(chunks);
            dispose(outcome);
            dispose(model);
        }
    })();
    return out;
}
