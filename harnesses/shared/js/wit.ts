/*
 * Shared QuickJS helpers for WIT handles and rendezvous message streams.
 * Inputs are host functions and values; closing a failed request releases both
 * sides of its streams rather than leaving an input writer suspended.
 */

export function typeConst(kind: object, ...names: string[]): unknown {
    const tokens = kind as Record<string, unknown> & { types?: Record<string, unknown> };
    const types = tokens.types ?? {};
    for (const name of names) {
        if (name in types) {
            return types[name];
        }
        if (name in kind) {
            return tokens[name];
        }
    }
    const key = Object.keys(types).find((candidate) => names.some((name) => candidate.endsWith(`_${name}`)));
    if (key) {
        return types[key];
    }
    throw new Error(`no ${names[0]} type in this build; have ${Object.keys(types).join(', ')}`);
}
// A WIT variant as text, following nested variants (tools@0.4.2's call-error
// wraps the reason in not-dispatched or outcome-unknown).
export function describeVariant(payload: { tag: string; val?: unknown }): string {
    const value = payload.val;
    if (value == null) {
        return payload.tag;
    }
    if (typeof value === 'object' && 'tag' in value && typeof value.tag === 'string') {
        return `${payload.tag}: ${describeVariant({ tag: value.tag, val: 'val' in value ? value.val : undefined })}`;
    }
    const detail =
        typeof value === 'string'
            ? value
            : ((typeof value === 'object' && 'detail' in value ? value.detail : undefined) ?? JSON.stringify(value));
    return `${payload.tag}: ${detail}`;
}
export function dispose(handle?: { [Symbol.dispose]?: () => void } | null) {
    try {
        handle?.[Symbol.dispose]?.();
    } catch {
        /* already dropped */
    }
}
// The pinned QuickJS patch exposes cancellation on pending async imports.
// Await cancellation settlement before releasing borrowed model/tool handles.
export async function abortable<T>(pending: PromiseLike<T> & { cancel?: () => void }, signal?: AbortSignal) {
    const cancel = () => pending.cancel?.();
    signal?.addEventListener('abort', cancel);
    if (signal?.aborted) {
        cancel();
    }
    try {
        return await pending;
    } finally {
        signal?.removeEventListener('abort', cancel);
    }
}
export async function startChat<Model, Message, Options, Reply>(
    chat: (
        model: Model,
        input: HostStream<Message>,
        done: HostFuture<unknown>,
        options: Options,
    ) => Promise<Reply> & { cancel?: () => void },
    model: Model,
    messages: Message[],
    options: Options,
    signal?: AbortSignal,
) {
    const { readable: input, writable: writer } = wit.Stream<Message>(typeConst(wit.Stream, 'MESSAGE'));
    const { readable: done, writable: doneWriter } = wit.Future(
        typeConst(wit.Future, 'RESULT_UNIT_STRING', 'RESULT_VOID_STRING'),
    );
    const writing = (async () => {
        let written = 0;
        try {
            written = await writer.writeAll(messages);
        } catch {
            /* input closed */
        }
        dispose(writer);
        try {
            await doneWriter.write(
                written === messages.length
                    ? { tag: 'ok' }
                    : { tag: 'err', val: `${messages.length - written} messages were not delivered` },
            );
        } catch {
            /* request closed */
        } finally {
            dispose(doneWriter);
        }
    })();
    let pending: ReturnType<typeof chat> | undefined;
    const cancel = () => pending?.cancel?.();
    signal?.addEventListener('abort', cancel);
    try {
        signal?.throwIfAborted();
        pending = chat(model, input, done, options);
        const reply = await pending;
        await writing;
        return reply;
    } catch (error) {
        dispose(input);
        dispose(done);
        await writing;
        throw error;
    } finally {
        signal?.removeEventListener('abort', cancel);
    }
}
