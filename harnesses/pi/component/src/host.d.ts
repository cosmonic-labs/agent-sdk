// QuickJS bindings for the host operations used by this component. WIT u64
// values are numbers here. Streams yield one item at a time (bytes in chunks).
type Variant<Tag extends string, Value> = { tag: Tag; val: Value };
type HostResult<T, E> = Variant<'ok', T> | Variant<'err', E>;
type JournalEnd = { tag: 'empty' } | Variant<'at', number>;
interface Snapshot {
    state?: { version: string; value: Uint8Array; through: JournalEnd };
    end: JournalEnd;
}
interface NewEntry {
    kind: string;
    data: Uint8Array;
}
type HostPart =
    | Variant<'text', string>
    | Variant<
          'attachment',
          {
              mediaType: string;
              source: Variant<'bytes', Uint8Array> | Variant<'url' | 'stored', string>;
              displayName?: string;
          }
      >;
type HostArguments = Variant<'json' | 'text', string>;
interface HostCall {
    id: string;
    name: string;
    arguments: HostArguments;
}
type HostContent =
    | Variant<'text' | 'refusal', string>
    | Variant<'tool-use', HostCall>
    | Variant<'other', { kind: string; json: string }>
    | Variant<'attachment' | 'reasoning', unknown>;
interface AssistantPart {
    content: HostContent;
    opaque?: unknown;
    citations: unknown[];
}
type HostMessage =
    | Variant<'system', string>
    | Variant<'user', { content: HostPart[]; name?: string }>
    | Variant<'assistant', { content: AssistantPart[] }>
    | Variant<'tool', { callId: string; isError: boolean; output: HostPart[] }[]>;
interface RunOptions {
    runId: string;
    instructions?: string;
    schema?: string;
    maxSteps?: number;
    maxTotalTokens?: number;
    maxDuration?: number;
    traceParent?: string;
}
interface HostStream<T> extends AsyncIterableIterator<T>, Disposable {
    cancelRead(): void;
}
interface HostFuture<T> extends Disposable {
    read(): Promise<T>;
    cancelRead(): void;
}
interface StreamWriter<T> extends Disposable {
    write(values: T[]): Promise<number>;
    writeAll(values: T[] | Uint8Array): Promise<number>;
    drop(): void;
}
interface FutureWriter<T> extends Disposable {
    write(value: T): Promise<void>;
}
interface WitStream {
    <T>(token: unknown): { readable: HostStream<T>; writable: StreamWriter<T> };
    U8: unknown;
}
interface WitFuture {
    <T>(token: unknown): { readable: HostFuture<T>; writable: FutureWriter<T> };
    from<T>(value: T, token: unknown): { readable: HostFuture<T> };
    types: Record<string, unknown>;
    [key: string]: unknown;
}
declare const wit: { Stream: WitStream; Future: WitFuture };

declare module 'cosmonic:agent/session@0.4.2' {
    export function current(): Promise<Snapshot>;
    export function clear(): Promise<void>;
    export function read(start: number, limit: number): Promise<(NewEntry & { seq: number })[]>;
    export function commit(
        entries: NewEntry[],
        state: Uint8Array | undefined,
        options: {
            requireState?: { tag: 'absent' } | Variant<'version', string>;
            requireEnd?: JournalEnd;
            commitId?: string;
        },
    ): Promise<Variant<'committed', { version?: string; end: JournalEnd }> | Variant<'stale', Snapshot>>;
}
declare module 'cosmonic:agent/tools@0.4.2' {
    export function list(): Promise<
        {
            name: string;
            title?: string;
            description: string;
            annotations: { readOnly?: boolean };
            input:
                | Variant<'json-schema', string>
                | Variant<'grammar', { syntax: string; definition: string }>
                | { tag: 'text' };
        }[]
    >;
    export function call(request: HostCall): Promise<{ output: HostPart[]; structured?: string; isError: boolean }>;
}
declare module 'cosmonic:agent/models@0.4.2' {
    export function describe(alias: string): Promise<{ contextLength?: number }>;
    export function open(alias: string): Promise<Disposable>;
}
declare module 'cosmonic:agent/chat@0.4.2' {
    interface Completion {
        model: string;
        message: { content: AssistantPart[] };
        finishReason: { tag: string };
        usage: { promptTokens?: number; completionTokens?: number };
    }
    export function chat(
        model: Disposable,
        input: HostStream<HostMessage>,
        done: HostFuture<unknown>,
        options: object,
    ): Promise<[HostStream<{ delta: { tag: string; val?: string } }>, HostFuture<HostResult<Completion, unknown>>]>;
}
declare module 'wasi:clocks/monotonic-clock@0.3.0' {
    export function now(): number;
    export function waitFor(ns: number): Promise<void>;
}
declare module 'wasi:random/random@0.3.0' {
    export function getRandomBytes(length: number): Uint8Array;
}
declare module 'wasi:http/types@0.3.0' {
    export class Fields {
        static fromList(entries: [string, Uint8Array][]): Fields;
        copyAll(): [string, Uint8Array][];
    }
    export class Request {
        getMethod(): string | Variant<string, string>;
        getPathWithQuery(): string | undefined;
        getHeaders(): Fields;
        static consumeBody(request: Request, result: HostFuture<unknown>): [AsyncIterable<Uint8Array>, unknown];
    }
    export class Response {
        static new(fields: Fields, stream: HostStream<unknown>, trailers: HostFuture<unknown>): [Response, unknown];
        setStatusCode(status: number): void;
    }
}
