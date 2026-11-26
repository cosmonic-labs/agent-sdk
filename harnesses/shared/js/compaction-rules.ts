/*
 * The harness-neutral rules of context compaction, shared by the
 * JavaScript harnesses: journal entry kinds, the threshold, the summarizer's
 * instructions, and the data of the compaction journal entries and run
 * events. No host imports, so they run under Node's tests. How a harness
 * splits, renders and replaces its own messages stays with that harness.
 */

import { STEP_KIND, parse } from './journal-rules.ts';

// The journal entry a compaction is recorded under, and a requested
// compaction's acceptance ({compact, focus?}) and ending ({compact, outcome,
// finish | error, compacted}) (cosmonic:agent/session, cosmonic:agent/compact).
export type Trigger = 'threshold' | 'overflow' | 'requested';

// The data of an agent.compaction.v1 entry.
export interface CompactionData {
    trigger: Trigger;
    summary: string;
    model: string;
    kept: number;
    tokensBefore?: number;
    tokensAfter?: number;
    through?: { start: number; end: number };
    focus?: string;
}

// A journal entry as the rules read it: JSON bytes or text.
interface Entry {
    kind: string;
    data: Uint8Array | string;
}

export const COMPACTION_KIND = 'agent.compaction.v1';
export const COMPACT_KIND = 'agent.compact.v1';
export const COMPACT_RESULT_KIND = 'agent.compact-result.v1';

// Summarize at this fraction of the window: 90 percent, as the Codex harness
// does (upstream Codex's auto-compact limit).
export const THRESHOLD = 0.9;

// Roughly four bytes of text a token; errs high for code and JSON, so
// compaction comes a little early rather than late.
export const BYTES_PER_TOKEN = 4;

// Tokens held back for what the estimate cannot see: tool schemas and the reply.
export const RESERVE_TOKENS = 1024;

// The longest a single tool result may be in the text given to the summarizer.
export const TOOL_RESULT_CHARS = 2000;

// The summarizer's instructions. Plain prose rather than a JSON schema, so a
// small local model can follow it.
export const SUMMARY_SYSTEM = [
    'You are summarizing the earlier part of a working session between a person and an assistant (you),',
    "because the conversation no longer fits the model's context window. The summary replaces those messages,",
    'and you will read it to carry on the session.',
    '',
    'Write a summary that keeps: every request the person made, in their words where short; what was done and',
    'decided, and why; facts, names, numbers, paths and results that may matter later, quoted exactly; errors',
    'and how they were resolved; and what was still open. Leave out pleasantries and repetition.',
    'Write only the summary, as plain text or short bullet lists. Do not address the person.',
].join('\n');

// Added to the transcript the summarizer reads when the person said what the
// summary should keep (the compact export's `focus`), which follows as their
// own message, never as instructions.
export const FOCUS_NOTE =
    'The person asked for this summary. Their message, next, says what it should keep; keep that.';

// Whether `estimate` tokens has reached `threshold` of a `window`-token
// context, counting the reserve. An unknown window never triggers.
export function overThreshold(estimate: number, window: number | null | undefined, threshold = THRESHOLD) {
    return !!window && window > 0 && estimate + RESERVE_TOKENS >= window * threshold;
}

// The data of an agent.compaction.v1 entry. `trigger` is why the agent
// compacted: 'threshold' (the context passed the agent's threshold),
// 'overflow' (the model refused a call for length) or 'requested' (the driver
// called compact, with the person's `focus` when given). `throughEnd` is the
// last journal sequence number the summary covers; the range starts at 0
// because a summary replaces everything before it, including any earlier
// summary.
export function entryData({
    trigger,
    summary,
    model,
    throughEnd,
    kept,
    tokensBefore,
    tokensAfter,
    focus,
}: Omit<CompactionData, 'through' | 'focus'> & { throughEnd: number | null; focus?: string | null }): CompactionData {
    return {
        trigger,
        summary,
        model,
        kept,
        tokensBefore,
        tokensAfter,
        ...(throughEnd != null ? { through: { start: 0, end: throughEnd } } : {}),
        ...(focus ? { focus } : {}),
    };
}

// The data of agent.compact-result.v1: how requested compaction `compact`
// ended (outcome done, failed or cancelled, with `finish` or the run-error
// case), and whether it wrote a summary.
export function compactResultData(
    compact: string,
    outcome: string,
    { finish, error }: { finish?: string; error?: string } = {},
    compacted = false,
) {
    return { compact, outcome, ...(finish ? { finish } : {}), ...(error ? { error } : {}), compacted };
}

// The `compacted` run event (run-types compaction-info) for the data of an
// agent.compaction.v1 entry committed at `seq`: the same fields, without the
// summary, model and focus; u64 fields are plain numbers, as componentize-qjs
// lowers them. Null when the entry records no range, which the event cannot
// do without.
export function compactionInfo(data: CompactionData, seq: number | null) {
    if (!data.through) {
        return null;
    }
    return {
        through: { start: data.through.start, end: data.through.end },
        trigger: data.trigger,
        kept: data.kept,
        tokensBefore: data.tokensBefore ?? undefined,
        tokensAfter: data.tokensAfter ?? undefined,
        seq: seq ?? undefined,
    };
}

// Whether journal `entries` ({ kind, data } with JSON bytes or text) hold an
// agent.compact.v1 for compaction `id`.
export function compactionAccepted(entries: ReadonlyArray<Entry>, id: string) {
    return entries.some((e) => e.kind === COMPACT_KIND && parse(e.data)?.compact === id);
}

// The seq of the last entry before the person's latest message among the
// journal `entries` a state covers (seq <= `covered`): the end of what a
// summary that keeps that message, and what follows it, covers. Null when
// there is no such message or nothing before it.
export function beforeLastPerson(entries: ReadonlyArray<Entry & { seq: number | bigint }>, covered: number) {
    const upto = entries.filter((e) => Number(e.seq) <= covered);
    const person = upto.findLast((e) => e.kind === STEP_KIND && parse(e.data)?.role === 'user');
    if (!person) {
        return null;
    }
    const at = Number(person.seq);
    const earlier = upto.map((e) => Number(e.seq)).filter((seq) => seq < at);
    return earlier.length ? earlier[earlier.length - 1] : null;
}

// Where the current turn starts in the journal, from real sequence numbers:
// they may have gaps (session.wit), so neither is inferred from the commit's
// end. `landed` is every entry read from just after `priorEnd` (the journal
// end before the turn's opening commit, null for an empty journal), oldest
// first, as { seq, kind }; that commit ended at `commitEnd` with the person's
// step (then agent.run.v1), and anything later is another writer's. Returns
// { person, before }: the step's seq, and the last seq before it, which is
// the end of what a summary made in this turn can cover (an entry the same
// commit wrote ahead of the step, else `priorEnd`; null when nothing is).
export function turnStart(
    landed: ReadonlyArray<{ seq: number | bigint; kind: string }>,
    commitEnd: number,
    priorEnd: number | null,
) {
    const ours = landed.filter((e) => Number(e.seq) <= commitEnd);
    const steps = ours.filter((e) => e.kind === STEP_KIND);
    if (!steps.length) {
        return { person: null, before: null };
    }
    const person = Number(steps[steps.length - 1].seq);
    const earlier = ours.map((e) => Number(e.seq)).filter((seq) => seq < person);
    return { person, before: earlier.length ? earlier[earlier.length - 1] : (priorEnd ?? null) };
}

// Which of the run's limits `counts` has reached at monotonic time `at`, as
// the run's finish reason, or null. `counts.steps` is every model call the run
// made: its turns, its summaries and the attempts a summary retried. A count
// that is missing counts as none.
export function limitReached(
    counts: { steps?: number; input?: number; output?: number },
    {
        maxSteps = null,
        maxTotalTokens = null,
        deadline = null,
    }: { maxSteps?: number | null; maxTotalTokens?: number | null; deadline?: number | null },
    at: number,
): 'max-total-tokens' | 'max-duration' | 'max-steps' | null {
    if (maxTotalTokens != null && (counts.input || 0) + (counts.output || 0) >= maxTotalTokens) {
        return 'max-total-tokens';
    }
    if (deadline != null && at >= deadline) {
        return 'max-duration';
    }
    return maxSteps != null && (counts.steps || 0) >= maxSteps ? 'max-steps' : null;
}
