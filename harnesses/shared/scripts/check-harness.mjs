#!/usr/bin/env node
/*
 * The sandbox gate for an agent harness: validate the built component, then
 * hold its root world to an ALLOWLIST. Anything not listed fails, so a new
 * capability needs a deliberate change here rather than slipping past a list
 * of names to refuse.
 *
 * A harness may import:
 *   - cosmonic:agent interfaces, all at one version (the host's model call,
 *     its tools and its session);
 *   - WASI clocks and random, and the io and cli plumbing a language runtime
 *     links (no filesystem, no sockets);
 *   - wasi:http/types@0.3, which alone cannot send a request.
 * It must export cosmonic:agent/run, may export cosmonic:agent/compact beside
 * it, and may export wasi:http/handler@0.3 (an export grants nothing).
 *
 *   node harnesses/shared/scripts/check-harness.mjs <component.wasm>
 */

import { basename } from 'node:path';

import { capture, isMain, run, runMain } from './build-helpers.mjs';

const ALLOWED_IMPORTS = [
    /^cosmonic:agent\/[a-z0-9-]+@\d+\.\d+\.\d+$/,
    /^wasi:io\/(error|poll|streams)@0\.2\.\d+$/,
    /^wasi:clocks\/(wall-clock|monotonic-clock|system-clock|types)@0\.[23]\.\d+$/,
    /^wasi:random\/(random|insecure-seed)@0\.[23]\.\d+$/,
    /^wasi:cli\/(environment|exit|stdin|stdout|stderr|terminal-input|terminal-output|terminal-stdin|terminal-stdout|terminal-stderr)@0\.[23]\.\d+$/,
    /^wasi:http\/types@0\.3\.\d+$/,
];
const ALLOWED_EXPORTS = [
    /^cosmonic:agent\/(run|compact)@\d+\.\d+\.\d+$/,
    /^wasi:http\/handler@0\.3\.\d+$/,
];

// Everything up to the first `;` or `{`: an inline instance import spanning
// several lines reads as `name: interface`, which nothing allows.
const items = (wit, keyword) =>
    wit
        .split('\n')
        .map((line) => line.match(new RegExp(`^\\s*${keyword} ([^;{]*)`))?.[1]?.trim())
        .filter(Boolean);

export function checkHarness(wasm) {
    if (!wasm) {
        throw new Error('usage: node harnesses/shared/scripts/check-harness.mjs <component.wasm>');
    }
    run('wasm-tools', ['validate', wasm]);
    const wit = capture('wasm-tools', ['component', 'wit', wasm]);
    const imports = items(wit, 'import');
    const exports = items(wit, 'export');
    if (!imports.length) {
        throw new Error(`could not read ${wasm}'s imports; is it a component?`);
    }
    console.log(`  ${basename(wasm)}:`);
    for (const item of [...imports.map((i) => `import ${i}`), ...exports.map((e) => `export ${e}`)]) {
        console.log(`     ${item}`);
    }

    const problems = [];
    for (const item of imports) {
        if (!ALLOWED_IMPORTS.some((allowed) => allowed.test(item))) {
            problems.push(`imports ${item}, which an agent harness may not`);
        }
    }
    for (const item of exports) {
        if (!ALLOWED_EXPORTS.some((allowed) => allowed.test(item))) {
            problems.push(`exports ${item}, which an agent harness may not`);
        }
    }
    const versions = new Set(
        [...imports, ...exports].map((i) => i.match(/^cosmonic:agent\/[a-z0-9-]+@(.+)$/)?.[1]).filter(Boolean),
    );
    if (versions.size !== 1) {
        problems.push(`uses cosmonic:agent at ${versions.size ? [...versions].join(', ') : 'no version'}; exactly one`);
    }
    if (!exports.some((e) => /^cosmonic:agent\/run@/.test(e))) {
        problems.push('does not export cosmonic:agent/run');
    }
    if (problems.length) {
        throw new Error(`${wasm}:\n     ${problems.join('\n     ')}`);
    }
    console.log('     (allowlist holds: no outgoing HTTP, no sockets, no filesystem)');
}

if (isMain(import.meta.url)) {
    await runMain(() => checkHarness(process.argv[2]));
}
