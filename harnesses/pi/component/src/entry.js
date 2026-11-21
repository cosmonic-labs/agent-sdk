/*
 * Pi on QuickJS: the typed cosmonic:agent/run export, and WASI 0.3's
 * wasi:http/handler for hosts that do not call it. The agent logic is
 * main.js; this file turns a wasi:http request into what main.js expects and
 * its streamed reply into a stream<u8> response body. A client that hangs up
 * makes a body write fail, which cancels Pi's turn.
 */

import '../../../shared/js/base-web.js';
import './timers.js';
import { Fields, Request as WasiRequest, Response as WasiResponse } from 'wasi:http/types@0.3.0';

import { handle } from './main.js';

// The typed cosmonic:agent/run@0.4.2 export; componentize-qjs looks an
// interface export up by its interface name.
export { runExport as run } from './main.js';

// The cosmonic:agent/compact@0.4.2 export: a summary the driver asks for.
export { compactExport as compact } from './requested.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

// The generated type constant for a future payload. Looked up by shape rather
// than by name because the shim qualifies names that would collide.
function futureType(match) {
    const types = wit.Future.types ?? {};
    const k = Object.keys(types).find(match);
    if (!k) {
        throw new Error(`no future type matching; have ${Object.keys(types).join(', ')}`);
    }
    return types[k] ?? wit.Future[k];
}
// future<result<_, error-code>>: how the request body was received.
const BODY_RESULT = () => futureType((k) => k.includes('ERROR_CODE') && !k.includes('OPTION'));
// future<result<option<trailers>, error-code>>: the response trailers.
const TRAILERS = () => futureType((k) => k.includes('ERROR_CODE') && k.includes('OPTION'));

async function readAll(stream) {
    const parts = [];
    let n = 0;
    for await (const chunk of stream) {
        parts.push(chunk);
        n += chunk.length;
    }
    const out = new Uint8Array(n);
    let o = 0;
    for (const p of parts) {
        out.set(p, o);
        o += p.length;
    }
    return out;
}

function toPiRequest(request) {
    const method = request.getMethod();
    const m = typeof method === 'string' ? method : method.tag === 'other' ? method.val : method.tag;
    const path = request.getPathWithQuery() ?? '/';
    const entries = request.getHeaders().copyAll();
    const headers = new Map(entries.map(([k, v]) => [String(k).toLowerCase(), dec.decode(v)]));
    const body = (async () => {
        const { readable: res } = wit.Future.from({ tag: 'ok' }, BODY_RESULT());
        const [stream] = WasiRequest.consumeBody(request, res);
        return dec.decode(await readAll(stream));
    })();
    return {
        method: m.toUpperCase(),
        url: `http://agent${path}`,
        headers: { has: (n) => headers.has(n.toLowerCase()), get: (n) => headers.get(n.toLowerCase()) ?? null },
        json: async () => JSON.parse(await body),
        text: () => body,
        consumed: body,
    };
}

export const handler = {
    async handle(request) {
        const req = toPiRequest(request);
        const res = await handle(req);
        // Always drain the request body, even for routes that ignore it.
        await req.consumed.catch(() => {});

        const headers = Fields.fromList(Object.entries(res.headers ?? {}).map(([k, v]) => [k, enc.encode(String(v))]));
        const { readable, writable } = wit.Stream(wit.Stream.U8);
        const trailers = wit.Future(TRAILERS());

        // Pump the body after returning the response: the host starts sending
        // headers at once and the turn streams as it happens.
        (async () => {
            try {
                if (res.body == null) {
                    /* empty */
                } else if (typeof res.body === 'string') {
                    await writable.writeAll(enc.encode(res.body));
                } else if (res.body instanceof Uint8Array) {
                    await writable.writeAll(res.body);
                } else {
                    for await (const chunk of res.body) {
                        const bytes = typeof chunk === 'string' ? enc.encode(chunk) : chunk;
                        let n;
                        try {
                            n = await writable.writeAll(bytes);
                        } catch (e) {
                            res.body.cancel(e);
                            break;
                        }
                        // A short write means the reader is gone: the person stopped the turn.
                        if (n !== bytes.length) {
                            res.body.cancel(new Error('the client went away'));
                            break;
                        }
                    }
                }
            } finally {
                try {
                    writable.drop();
                } catch {
                    /* gone */
                }
                try {
                    await trailers.writable.write({ tag: 'ok', val: null });
                } catch {
                    /* no reader */
                }
            }
        })();

        const [response] = WasiResponse.new(headers, readable, trailers.readable);
        response.setStatusCode(res.status);
        return response;
    },
};
