/*
 * One run or compaction at a time in this instance: the backstop against two
 * drivers on one session (the driver holds the real lock). Taken when a run
 * or compaction is accepted and released when it has made its last commit.
 */

import { createRunLock } from '../../../shared/js/run-rules.ts';

// The typed, compact and HTTP adapters operate on the same instance.
export const { acquire, release } = createRunLock();
