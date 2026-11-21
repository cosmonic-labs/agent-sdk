/*
 * Preserve host result types through shared cancellation and stream helpers.
 * Runtime type tokens and stream inputs remain opaque componentizer values.
 */

// A WIT variant as text, following nested variants.
export function describeVariant(payload: { tag: string; val?: unknown }): string;

// Resource disposal is idempotent at the helper boundary.
export function dispose(handle?: { [Symbol.dispose]?: () => void } | null): void;

// Discover a generated componentizer token by its interface-local name.
export function typeConst(kind: object, ...names: string[]): unknown;

// Cancellation preserves the resolved type of the original host import.
export function abortable<T>(pending: PromiseLike<T> & { cancel?: () => void }, signal?: AbortSignal): Promise<T>;

// Pair request-message delivery with the host's typed response handles.
export function startChat<Model, Message, Options, Reply>(
    chat: (model: Model, input: unknown, done: unknown, options: Options) => Promise<Reply> & { cancel?: () => void },
    model: Model,
    messages: Message[],
    options: Options,
    signal?: AbortSignal,
): Promise<Reply>;
