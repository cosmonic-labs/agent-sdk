/*
 * A real setTimeout over the WASI clock, installed before anything else loads
 * (ES module imports are evaluated before the importing module's body).
 *
 * The wait is an async host call, so awaiting it hands control back to the
 * host: pending stream writes complete and cancellations arrive. A microtask
 * never does, and a turn that only ever yields to microtasks starves
 * everything else in the component.
 */

import { waitFor } from 'wasi:clocks/monotonic-clock@0.3.0';

globalThis.setTimeout = (fn, ms = 0, ...args) => {
    // At least 1 ms: a zero wait can complete without suspending, and then it
    // hands nothing back to the host.
    waitFor(Math.max(1, Math.floor(Number(ms) || 0)) * 1_000_000).then(
        () => fn(...args),
        () => fn(...args),
    );
    return 0;
};

// setInterval over the same wait: each tick waits again until cleared. The
// QuickJS runtime has no timers of its own, so without this the requested
// compaction's heartbeat throws before its work starts.
const intervals = new Set();
let nextInterval = 1;

globalThis.setInterval = (fn, ms = 0, ...args) => {
    const id = nextInterval++;
    const wait = Math.max(1, Math.floor(Number(ms) || 0)) * 1_000_000;
    intervals.add(id);
    const tick = () => {
        if (!intervals.has(id)) {
            return;
        }
        waitFor(wait).then(
            () => {
                if (intervals.has(id)) {
                    fn(...args);
                    tick();
                }
            },
            () => intervals.delete(id),
        );
    };
    tick();
    return id;
};

globalThis.clearInterval = (id) => {
    intervals.delete(id);
};
