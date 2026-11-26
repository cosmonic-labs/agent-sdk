/*
 * Context compaction for Pi: when the transcript nears the model's context
 * window, or overflows it, the older part is summarized by the model and
 * replaced, in what the model is given next, by that summary.
 *
 * Inputs are Pi transcripts (pi-agent-core AgentMessages) and the window the
 * host reports; outputs are plans, compacted transcripts and the data of the
 * agent.compaction.v1 journal entry. The model call itself is injected, so
 * this module imports nothing from the host and runs under Node's tests.
 *
 * What a compaction may do:
 *   - Only messages before the current turn's kickoff (the person's latest
 *     message) are summarized. The kickoff and everything after it are kept
 *     verbatim, and so are Pi's system messages, which declare the tools.
 *   - The summary is model output, so it goes back in as an ASSISTANT message,
 *     never as system text and never in the person's voice. A short fixed user
 *     message the agent writes (not the model) introduces it.
 *   - The originals are not deleted: they stay in the session journal.
 *
 * The harness-neutral rules (entry kinds, threshold, the summarizer's
 * instructions, entry data) live in shared/js/compaction-rules.ts and are
 * re-exported here.
 */

import type { ImageContent } from '@earendil-works/pi-ai';
import type { Reply, TranscriptMessage } from './types.ts';

import {
    BYTES_PER_TOKEN,
    TOOL_RESULT_CHARS,
    limitReached,
    overThreshold,
} from '../../../shared/js/compaction-rules.ts';
import { recordedArguments } from '../../../shared/js/tool-input.ts';

export * from '../../../shared/js/compaction-rules.ts';

// What the agent, not the model, says ahead of the summary.
export const SUMMARY_PREAMBLE =
    "The earlier part of this conversation was summarized to fit the model's context window. " +
    'The next message is that summary, written by the model; the original messages are kept in the session.';

// The text of one message part, for estimating and for the summarizer.
function partText(c: Reply['content'][number] | ImageContent) {
    if (c.type === 'text') {
        return c.text ?? '';
    }
    if (c.type === 'toolCall') {
        return `[called ${c.name} with ${JSON.stringify(recordedArguments(c.arguments))}]`;
    }
    if (c.type === 'image') {
        return '[image]';
    }
    if (c.type === 'thinking') {
        return c.thinking ?? '';
    }
    return '';
}

function messageText(m: TranscriptMessage) {
    if (typeof m.content === 'string') {
        return m.content;
    }
    return (m.content ?? []).map(partText).join('\n');
}

// Whether `m` is a message a compaction wrote (the preamble or the summary).
const isCompactionMessage = (m: TranscriptMessage) => m?.compaction === true;

// A rough token count of what the model would be given from `messages`.
export function estimateTokens(messages: TranscriptMessage[]) {
    let bytes = 0;
    for (const m of messages) {
        bytes += messageText(m).length;
        if (m.role === 'system' && m.toolsAdded) {
            bytes += JSON.stringify(m.toolsAdded).length;
        }
    }
    return Math.floor(bytes / BYTES_PER_TOKEN);
}

// The index of the current turn's kickoff: the person's latest message.
export function kickoff(messages: TranscriptMessage[]) {
    for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === 'user' && !isCompactionMessage(messages[i])) {
            return i;
        }
    }
    return -1;
}

// Split `messages` at the kickoff: the system messages before it (kept), the
// conversation before it (summarized) and the rest (kept). Null when there is
// nothing worth summarizing: no earlier conversation, or only a previous
// summary and its preamble, which summarizing again would not shrink. Those
// are known by the `compaction` flag the agent sets on them, never by their
// text, so a single real exchange is still summarized and a message that only
// reads like a summary is not mistaken for one.
export function plan(messages: TranscriptMessage[]) {
    const at = kickoff(messages);
    if (at <= 0) {
        return null;
    }
    const before = messages.slice(0, at);
    const older = before.filter((m) => m.role !== 'system');
    if (older.every(isCompactionMessage)) {
        return null;
    }
    return { at, system: before.filter((m) => m.role === 'system'), older, kept: messages.slice(at) };
}

// The older conversation as text for the summarizer, each tool result cut
// short, and the oldest lines dropped first when the whole exceeds `maxChars`.
export function transcriptText(older: TranscriptMessage[], maxChars = Infinity) {
    const lines = older.map((m) => {
        let text = messageText(m);
        if (m.role === 'toolResult' && text.length > TOOL_RESULT_CHARS) {
            text = `${text.slice(0, TOOL_RESULT_CHARS)}\n[... ${text.length - TOOL_RESULT_CHARS} more characters]`;
        }
        const who = m.role === 'toolResult' ? `tool ${m.toolName ?? ''}`.trim() : m.role;
        return `[${who}]\n${text}`;
    });
    let out = lines.join('\n\n');
    while (out.length > maxChars && lines.length > 1) {
        lines.shift();
        out = `[earlier messages omitted]\n\n${lines.join('\n\n')}`;
    }
    return out;
}

// The transcript after compaction: the kept system messages, the agent's
// preamble, the model's summary as an assistant message, then the kept
// messages unchanged. The two new messages carry `compaction: true`, so a
// later compaction knows them, and the summary is marked as the model's.
export function compacted(
    p: NonNullable<ReturnType<typeof plan>>,
    summary: string,
    model: string,
): TranscriptMessage[] {
    const now = Date.now();
    return [
        ...p.system,
        { role: 'user', content: SUMMARY_PREAMBLE, timestamp: now, compaction: true },
        {
            role: 'assistant',
            content: [{ type: 'text', text: summary }],
            api: 'cosmonic-llm',
            provider: 'cosmonic',
            model: model || 'default',
            usage: {
                input: 0,
                output: 0,
                cacheRead: 0,
                cacheWrite: 0,
                totalTokens: 0,
                cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
            },
            stopReason: 'stop',
            timestamp: now,
            compaction: true,
        },
        ...p.kept,
    ];
}

// Another writer took the session over. The run stops before its next model
// or tool call (session.ts and main.ts map it to a run that ends superseded).
export class Superseded extends Error {
    constructor() {
        super('another writer took this session over, so the turn stopped before its next model or tool call');
    }
}

// Close a compaction's model op, whose call already happened. `install` tries
// the one conditional commit of its result, the compaction entry and the
// smaller transcript, resolving to 'committed', 'stale' (another writer
// committed since this writer's last commit) or 'too-large' (the host refused
// the size; anything else it throws). `closeFailed` lands the op's result as
// failed without conditions, so the journal accounts for the call either way.
// `owned` is false when this writer already lost the session, and the
// conditional commit is not tried. Resolves to:
//   'compacted'  the smaller transcript is the state; go on with it.
//   'kept'       the host refused the size: harmless, go on uncompacted.
//   'superseded' another writer owns the session: stop the run, with no
//                further model or tool call.
export async function closeCompaction({
    owned,
    install,
    closeFailed,
}: {
    owned: boolean;
    install(): Promise<'committed' | 'stale' | 'too-large'>;
    closeFailed(): Promise<void>;
}) {
    const outcome = owned ? await install() : 'stale';
    if (outcome === 'committed') {
        return 'compacted';
    }
    await closeFailed();
    return outcome === 'too-large' ? 'kept' : 'superseded';
}

// Make one model call that is not a turn (a summary) and charge it to the run:
// a step whether or not it succeeds, and the tokens it reports, which a failed
// call carries on the error it throws (`usage`).
export async function charged<T extends { usage?: { input?: number; output?: number } }>(
    counts: { steps: number; input: number; output: number },
    call: () => Promise<T>,
) {
    counts.steps++;
    const add = (usage: T['usage']) => {
        counts.input += usage?.input || 0;
        counts.output += usage?.output || 0;
    };
    try {
        const out = await call();
        add(out?.usage);
        return out;
    } catch (e) {
        add(e?.usage);
        throw e;
    }
}

// Compact within the run's limits: no summary is started once a limit is
// reached, and the limits are checked again after it, before the normal model
// call. Resolves to { limit, next }: the limit that ends the run (null to go
// on) and what `compact` returned (null when it did not run or did nothing).
interface Budget {
    counts: Parameters<typeof limitReached>[0];
    limits: Parameters<typeof limitReached>[1];
    now(): number;
}

export async function compactWithinLimits<T>({ counts, limits, now, compact }: Budget & { compact(): Promise<T> }) {
    const before = limitReached(counts, limits, now());
    if (before) {
        return { limit: before, next: null };
    }
    const next = await compact();
    return { limit: limitReached(counts, limits, now()), next };
}

// What the model is given for its next call: the loop's `messages` through
// the view, compacted first when they near the window and there is earlier
// conversation to summarize. Resolves to { messages, limit }; a limit means the
// run ends there, without the call. `lost()` says whether this writer already
// lost the session: then it throws Superseded before anything else.
export async function beforeCall(
    messages: TranscriptMessage[],
    {
        view,
        window,
        canCompact,
        counts,
        limits,
        now,
        compact,
        lost = () => false,
    }: Budget & {
        view: View;
        window: number | null;
        canCompact: boolean;
        compact(full: TranscriptMessage[]): Promise<TranscriptMessage[] | null>;
        lost?(): boolean;
    },
) {
    // A writer that lost the session (a commit after a lost race lands
    // without failing, but marks the journal stale) makes no further call.
    if (lost()) {
        throw new Superseded();
    }
    const viewed = view.apply(messages);
    if (!canCompact || !overThreshold(estimateTokens(viewed), window) || !plan(viewed)) {
        return { messages: viewed, limit: null };
    }
    const { limit, next } = await compactWithinLimits({ counts, limits, now, compact: () => compact(messages) });
    return { messages: next ?? viewed, limit };
}

// Pass one model call's events from `first` (an async iterable) to `out` (a
// stream with push and end), answering a refusal for length with one retry
// when none of the reply's text was streamed yet, the rule both agents keep,
// so a reader never sees a partial reply replaced. The call's `start` event
// is held until something follows it: a refusal right after it is then still
// retried, and the loop never sees two starts for one reply. `shouldRetry(ev)`
// says whether an error event may be retried; `retry(ev)` resolves to the
// retried call's events, or null to pass the refusal on. Never rejects and
// always ends `out`: anything thrown (building the retry, or either call's
// events) is pushed as `fail(e)`, an error event, so the loop reading `out`
// is never left waiting.
export async function pipeModelEvents<E extends { type: string }>(
    out: { push(event: E): void; end(): void },
    first: AsyncIterable<E>,
    {
        shouldRetry,
        retry,
        fail,
    }: {
        shouldRetry(event: E): boolean;
        retry(event: E): Promise<AsyncIterable<E> | null>;
        fail(error: unknown): E;
    },
) {
    let held = null;
    let streamed = false;
    try {
        for await (const ev of first) {
            if (!streamed && ev.type === 'start') {
                held = ev;
                continue;
            }
            if (!streamed && ev.type === 'error' && shouldRetry(ev)) {
                const again = await retry(ev);
                if (again) {
                    for await (const e of again) {
                        out.push(e);
                    }
                    return;
                }
            }
            if (held) {
                out.push(held);
                held = null;
            }
            streamed = true;
            out.push(ev);
        }
        if (held) {
            out.push(held);
        }
    } catch (e) {
        out.push(fail(e));
    } finally {
        out.end();
    }
}

// A view of the loop's transcript after compactions: `prefix` stands in for
// the first `covered` messages. pi-agent-core keeps its own full copy of the
// transcript and hands it to transformContext on every call, so the view is
// applied each time, and to every transcript this agent saves.
export class View {
    private prefix: TranscriptMessage[] | null = null;
    private covered = 0;

    apply(messages: TranscriptMessage[]) {
        return this.prefix ? [...this.prefix, ...messages.slice(this.covered)] : messages;
    }

    // Where index `i` of the full transcript is in the viewed one.
    index(i: number) {
        return this.prefix ? this.prefix.length + (i - this.covered) : i;
    }

    // Record a compaction of `viewed` (this view applied to `full`) at its
    // kickoff `at`, replacing everything before it with `prefix`.
    replace(full: TranscriptMessage[], viewed: TranscriptMessage[], at: number, prefix: TranscriptMessage[]) {
        const keptFromFull = viewed.length - at;
        this.covered = full.length - keptFromFull;
        this.prefix = prefix;
    }
}
