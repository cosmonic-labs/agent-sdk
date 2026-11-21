/*
 * Types for the shared compaction rules (compaction-rules.js), for the
 * TypeScript harnesses that import them.
 */

export const COMPACTION_KIND: 'agent.compaction.v1';
export const COMPACT_KIND: 'agent.compact.v1';
export const COMPACT_RESULT_KIND: 'agent.compact-result.v1';
export const THRESHOLD: number;
export const BYTES_PER_TOKEN: number;
export const RESERVE_TOKENS: number;
export const TOOL_RESULT_CHARS: number;
export const SUMMARY_SYSTEM: string;
export const FOCUS_NOTE: string;

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

export function overThreshold(estimate: number, window: number | null | undefined, threshold?: number): boolean;

export function entryData(fields: {
    trigger: Trigger;
    summary: string;
    model: string;
    throughEnd: number | null;
    kept: number;
    tokensBefore?: number;
    tokensAfter?: number;
    focus?: string | null;
}): CompactionData;

export function compactResultData(
    compact: string,
    outcome: string,
    detail?: { finish?: string; error?: string },
    compacted?: boolean,
): { compact: string; outcome: string; finish?: string; error?: string; compacted: boolean };

export function compactionInfo(
    data: CompactionData,
    seq: number | null,
): {
    through: { start: number; end: number };
    trigger: Trigger;
    kept: number;
    tokensBefore: number | undefined;
    tokensAfter: number | undefined;
    seq: number | undefined;
} | null;

export function compactionAccepted(entries: ReadonlyArray<Entry>, id: string): boolean;

export function beforeLastPerson(entries: ReadonlyArray<Entry & { seq: number }>, covered: number): number | null;

export function turnStart(
    landed: ReadonlyArray<{ seq: number; kind: string }>,
    commitEnd: number,
    priorEnd: number | null,
): { person: number | null; before: number | null };

export function limitReached(
    counts: { steps?: number; input?: number; output?: number },
    limits: { maxSteps?: number | null; maxTotalTokens?: number | null; deadline?: number | null },
    at: number,
): 'max-total-tokens' | 'max-duration' | 'max-steps' | null;
