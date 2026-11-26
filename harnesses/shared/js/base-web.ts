/*
 * Web APIs the JavaScript harnesses use that a browser (or StarlingMonkey) provides and
 * QuickJS does not. Minimal: enough for what the agent loop and the HTTP entry
 * actually call, nothing more.
 */

// The embedded runtime may already supply an API. Each fallback implements
// the subset used by the harness and its bundled dependencies.
function provide(name: keyof typeof globalThis, fallback: unknown) {
    if (globalThis[name] == null) Reflect.set(globalThis, name, fallback);
}

// QuickJS has no console, and this component imports no stdio. Keep the last
// lines in memory so they are at least inspectable from a debugger.
const logs: string[] = [];
const keep =
    (lvl: string) =>
    (...a: unknown[]) => {
        logs.push(
            `${lvl}: ${a
                .map((x) =>
                    typeof x === 'string'
                        ? x
                        : (() => {
                              try {
                                  return JSON.stringify(x);
                              } catch {
                                  return String(x);
                              }
                          })(),
                )
                .join(' ')}`,
        );
        if (logs.length > 200) {
            logs.shift();
        }
    };
provide('console', {
    log: keep('log'),
    info: keep('info'),
    warn: keep('warn'),
    error: keep('error'),
    debug: keep('debug'),
});

provide('queueMicrotask', (fn: () => void) => {
    Promise.resolve().then(fn);
});
// setTimeout is defined in entry.js over the WASI clock; clearTimeout is a
// no-op because nothing on Pi's path cancels a timer it set.
provide('clearTimeout', () => {});

class TextEncoderPoly {
    get encoding() {
        return 'utf-8';
    }
    encode(s = '') {
        s = String(s);
        const out = [];
        for (let i = 0; i < s.length; i++) {
            let c = s.charCodeAt(i);
            if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
                const d = s.charCodeAt(i + 1);
                if (d >= 0xdc00 && d <= 0xdfff) {
                    c = 0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00);
                    i++;
                }
            }
            if (c < 0x80) {
                out.push(c);
            } else if (c < 0x800) {
                out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
            } else if (c < 0x10000) {
                out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
            } else {
                out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
            }
        }
        return new Uint8Array(out);
    }
}
class TextDecoderPoly {
    get encoding() {
        return 'utf-8';
    }
    decode(buf?: ArrayBuffer | ArrayBufferView) {
        if (!buf) {
            return '';
        }
        const b =
            buf instanceof Uint8Array
                ? buf
                : ArrayBuffer.isView(buf)
                  ? new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength)
                  : new Uint8Array(buf);
        let s = '';
        for (let i = 0; i < b.length; ) {
            const c = b[i++];
            let cp;
            if (c < 0x80) {
                cp = c;
            } else if (c < 0xe0) {
                cp = ((c & 31) << 6) | (b[i++] & 63);
            } else if (c < 0xf0) {
                cp = ((c & 15) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
            } else {
                cp = ((c & 7) << 18) | ((b[i++] & 63) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
            }
            s += String.fromCodePoint(cp);
        }
        return s;
    }
}
provide('TextEncoder', TextEncoderPoly);
provide('TextDecoder', TextDecoderPoly);

type AbortListener = (event: { type: string; target: AbortSignalPoly }) => void;

class AbortSignalPoly {
    aborted = false;
    reason: unknown;
    onabort: AbortListener | null = null;
    _l: AbortListener[] = [];
    get [Symbol.toStringTag]() {
        return 'AbortSignal';
    }
    addEventListener(t: string, f: AbortListener) {
        if (t === 'abort') {
            this._l.push(f);
        }
    }
    removeEventListener(t: string, f: AbortListener) {
        this._l = this._l.filter((x) => x !== f);
    }
    throwIfAborted() {
        if (this.aborted) {
            throw this.reason;
        }
    }
}
class AbortControllerPoly {
    signal = new AbortSignalPoly();
    abort(reason?: unknown) {
        const s = this.signal;
        if (s.aborted) {
            return;
        }
        s.aborted = true;
        s.reason = reason ?? Object.assign(new Error('This operation was aborted'), { name: 'AbortError' });
        const ev = { type: 'abort', target: s };
        try {
            s.onabort?.(ev);
        } catch {
            /* listener */
        }
        for (const f of s._l) {
            try {
                f(ev);
            } catch {
                /* listener */
            }
        }
    }
}
provide('AbortController', AbortControllerPoly);
provide('AbortSignal', AbortSignalPoly);

provide('structuredClone', (v: unknown) => (v === undefined ? undefined : JSON.parse(JSON.stringify(v))));

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
provide('btoa', (s: string) => {
    let out = '';
    for (let i = 0; i < s.length; i += 3) {
        const a = s.charCodeAt(i),
            b = s.charCodeAt(i + 1),
            c = s.charCodeAt(i + 2);
        const n = (a << 16) | ((b || 0) << 8) | (c || 0);
        out +=
            B64[(n >> 18) & 63] +
            B64[(n >> 12) & 63] +
            (i + 1 < s.length ? B64[(n >> 6) & 63] : '=') +
            (i + 2 < s.length ? B64[n & 63] : '=');
    }
    return out;
});
provide('atob', (s: string) => {
    s = String(s).replace(/[^A-Za-z0-9+/]/g, '');
    let out = '';
    for (let i = 0; i < s.length; i += 4) {
        const n =
            (B64.indexOf(s[i]) << 18) |
            (B64.indexOf(s[i + 1]) << 12) |
            ((B64.indexOf(s[i + 2]) & 63) << 6) |
            (B64.indexOf(s[i + 3]) & 63);
        out += String.fromCharCode((n >> 16) & 255);
        if (s[i + 2] !== undefined) {
            out += String.fromCharCode((n >> 8) & 255);
        }
        if (s[i + 3] !== undefined) {
            out += String.fromCharCode(n & 255);
        }
    }
    return out;
});

class URLPoly {
    protocol!: string;
    host!: string;
    hostname!: string;
    pathname!: string;
    search!: string;
    hash!: string;
    origin!: string;
    href!: string;
    constructor(u: string, base?: string) {
        const s = String(u);
        const m = s.match(/^([a-z][a-z0-9+.-]*:)\/\/([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/i);
        if (!m) {
            if (base) {
                return new URLPoly(new URLPoly(base).origin + (s.startsWith('/') ? s : `/${s}`));
            }
            throw new TypeError(`Invalid URL: ${s}`);
        }
        this.protocol = m[1];
        this.host = m[2];
        this.hostname = m[2].replace(/:\d+$/, '');
        this.pathname = m[3] || '/';
        this.search = m[4] || '';
        this.hash = m[5] || '';
        this.origin = `${this.protocol}//${this.host}`;
        this.href = s;
    }
    toString() {
        return this.href;
    }
}
provide('URL', URLPoly);

// Just enough of ReadableStream for Pi's turn: a source whose start() pushes
// chunks, read by the HTTP bridge as an async iterable, cancellable.
interface StreamSource<T> {
    start?(controller: { enqueue(chunk: T): void; close(): void; error(reason: unknown): void }): void | Promise<void>;
    cancel?(reason?: unknown): void;
}

class ReadableStreamLite<T> {
    private _src: StreamSource<T>;
    private _q: T[] = [];
    private _done = false;
    private _err: unknown;
    private _wake: (() => void) | null = null;
    constructor(src: StreamSource<T> = {}) {
        this._src = src;
        const wake = () => {
            const w = this._wake;
            this._wake = null;
            w?.();
        };
        const controller = {
            enqueue: (c: T) => {
                if (this._done) {
                    throw new TypeError('closed');
                }
                this._q.push(c);
                wake();
            },
            close: () => {
                this._done = true;
                wake();
            },
            error: (e: unknown) => {
                this._err = e;
                this._done = true;
                wake();
            },
        };
        Promise.resolve()
            .then(() => src.start?.(controller))
            .catch((e) => controller.error(e));
    }
    async *[Symbol.asyncIterator]() {
        for (;;) {
            if (this._q.length) {
                yield this._q.shift()!;
                continue;
            }
            if (this._err) {
                throw this._err;
            }
            if (this._done) {
                return;
            }
            await new Promise<void>((r) => {
                this._wake = r;
            });
        }
    }
    cancel(reason?: unknown) {
        this._done = true;
        try {
            this._src.cancel?.(reason);
        } catch {
            /* source */
        }
    }
}
provide('ReadableStream', ReadableStreamLite);

class ResponseLite {
    body: string | Uint8Array | ReadableStreamLite<Uint8Array> | null;
    status: number;
    headers: Record<string, string>;
    constructor(body: ResponseLite['body'] = null, init: { status?: number; headers?: Record<string, string> } = {}) {
        this.body = body;
        this.status = init.status ?? 200;
        this.headers = init.headers ?? {};
    }
}
provide('Response', ResponseLite);
