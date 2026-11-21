/*
 * Types for the shared run export rules (run-rules.js), for the TypeScript
 * harnesses that import them.
 */

// Nonblocking admission for one application instance.
export function createRunLock(): { acquire(): boolean; release(): void };

export const MAX_RUN_ID_BYTES: number;
export const MAX_COMPACT_ID_BYTES: number;
export const MAX_FOCUS_CHARS: number;

export function startError(tag: string, val?: string): { tag: string; val?: string };

export function checkId(id: string, what: string, max: number): void;

export function checkMaxSteps(options: { maxSteps?: number | null }): void;

export function checkSchema(schema: string | null | undefined): void;

export function compactRequest(
    focus: string | null | undefined,
    options: { runId?: string; maxSteps?: number; maxTotalTokens?: number; maxDuration?: number; traceParent?: string },
): {
    id: string;
    focus: string | null;
    maxSteps: number;
    maxTotalTokens: number | null;
    maxDuration: number | null;
    traceParent: string | null;
};

export function refusal(
    error: unknown,
    errors: {
        Busy: abstract new (...args: never[]) => unknown;
        Duplicate: abstract new (...args: never[]) => unknown;
        describe: (error: unknown) => string;
    },
): { tag: string; val?: string };

export function usageOf(counts: { input: number; output: number }): {
    promptTokens: number;
    cachedPromptTokens: undefined;
    cacheWriteTokens: undefined;
    completionTokens: number;
    reasoningTokens: undefined;
};

export function countsOf(counts: { steps: number; toolCalls: number; input: number; output: number }): {
    steps: number;
    toolCalls: number;
    usage: ReturnType<typeof usageOf>;
};

export interface EventWriter {
    put(event: unknown): Promise<boolean>;
    readonly alive: boolean;
    heartbeat(): void;
    close(): void;
}

export function eventWriter(
    writable: { write(values: unknown[]): Promise<number>; drop(): void },
    options?: { onGone?: () => void; heartbeatMs?: number },
): EventWriter;
