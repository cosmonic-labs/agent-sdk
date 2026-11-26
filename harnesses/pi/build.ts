/*
 * Build the Pi agent harness component, in three steps:
 *
 *   1. bundle component/src/ with esbuild (host interfaces stay imports)
 *   2. componentize it with componentize-qjs (QuickJS, WASI 0.3) from
 *      component/wit/: the result, dist/pi-agent.qjs.wasm, exports
 *      cosmonic:agent/run@0.4.2 and wasi:http/handler@0.3.0
 *   3. compose deny-fs in front of it (component/compose.wac)
 *
 * `run` is unstable (`@unstable(feature = agent-run)`), and componentize-qjs
 * has no way to enable a WIT feature, so step 2 reads a copy of wit/ in
 * dist/ with that gate taken off. The gate only hides items from tools that
 * did not opt in; the component's types are the same either way.
 *
 * Step 3 exists because the QuickJS runtime links wasi-libc's filesystem
 * support whether or not the agent touches a file, so the bare component
 * imports wasi:filesystem. deny-fs answers those imports inside the component
 * with an empty filesystem, and the published artifact imports none.
 *
 * COMPONENTIZE_QJS names the CLI (default: `componentize-qjs` on PATH). It
 * must carry patches/componentize-qjs.patch (see README): upstream's
 * initializer cannot snapshot a component that serves HTTP or imports an
 * async host function. `cargo` and `wac` must be on PATH.
 * Load esbuild only when bundling; callers may supply a compatible bundle
 * function to reuse the build orchestration without installing that package.
 */

import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { isMain, run, runMain, ungatedWit } from '../shared/scripts/build-helpers.mjs';

export async function main({
    directory = fileURLToPath(new URL('.', import.meta.url)),
    componentizer = process.env.COMPONENTIZE_QJS || 'componentize-qjs',
    composer = process.env.WAC || 'wac',
    bundle,
}: { directory?: string; componentizer?: string; composer?: string; bundle?: typeof import('esbuild').build } = {}) {
    directory = resolve(directory);
    const build = bundle ?? (await import('esbuild')).build;
    await build({
        absWorkingDir: directory,
        entryPoints: ['component/src/entry.ts'],
        bundle: true,
        format: 'esm',
        platform: 'neutral',
        mainFields: ['module', 'main'],
        conditions: ['import'],
        external: ['cosmonic:*', 'wasi:*'],
        outfile: 'dist/agent.js',
        logLevel: 'warning',
    });

    ungatedWit(resolve(directory, 'component/wit'), resolve(directory, 'dist/wit'));
    run(
        componentizer,
        ['--wit', 'dist/wit', '--world', 'pi-agent', '--js', 'dist/agent.js', '-o', 'dist/pi-agent.qjs.wasm'],
        { cwd: directory },
    );

    // The committed Cargo.lock is preserved unless regeneration is requested.
    const locked = process.env.REGEN_LOCK === '1' ? [] : ['--locked'];
    run('cargo', ['build', ...locked, '--target', 'wasm32-wasip2', '--release'], {
        cwd: resolve(directory, 'deny-fs'),
    });

    run(
        composer,
        [
            'compose',
            'component/compose.wac',
            '--dep',
            'cosmonic:deny-fs=deny-fs/target/wasm32-wasip2/release/deny_fs.wasm',
            '--dep',
            'cosmonic:pi-agent=dist/pi-agent.qjs.wasm',
            '-o',
            'dist/pi-agent.wasm',
        ],
        { cwd: directory },
    );
    console.log('built dist/pi-agent.wasm');
}

if (isMain(import.meta.url)) {
    await runMain(() => main());
}
