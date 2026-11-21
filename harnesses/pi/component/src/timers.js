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
