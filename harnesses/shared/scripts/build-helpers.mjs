/*
 * Shared command execution, the repository path, direct-entrypoint checks and
 * CLI error handling for the harness scripts. Importing it runs no commands.
 */

import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// The repository root (this file is harnesses/shared/scripts/build-helpers.mjs).
export const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));

export function run(command, args, options = {}) {
    try {
        return execFileSync(command, args, { stdio: 'inherit', ...options });
    } catch (error) {
        if (error.code === 'ENOENT') {
            throw new Error(`${command} not found on PATH`);
        }
        throw error;
    }
}

export function capture(command, args, options = {}) {
    return run(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...options });
}

export function isMain(url) {
    return process.argv[1] && resolve(process.argv[1]) === fileURLToPath(url);
}

export async function runMain(action) {
    try {
        await action();
    } catch (error) {
        console.error(`  !! ${error.message}`);
        process.exitCode = error.status || 1;
    }
}
