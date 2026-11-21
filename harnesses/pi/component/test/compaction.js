/*
 * Test Pi's context compaction helpers with Node's test runner: when to
 * summarize, what is summarized and kept, the shape of the compacted
 * transcript and of the agent.compaction.v1 entry, and the view that maps the
 * loop's full transcript onto the compacted one. Registers tests; no main.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
    SUMMARY_PREAMBLE,
    Superseded,
    View,
    beforeCall,
    charged,
    closeCompaction,
    beforeLastPerson,
    compactResultData,
    compacted,
    compactionAccepted,
    compactionInfo,
    compactWithinLimits,
    entryData,
    estimateTokens,
    kickoff,
    limitReached,
    overThreshold,
    pipeModelEvents,
    plan,
    transcriptText,
    turnStart,
} from '../src/compaction.js';

// A system message, `n` earlier exchanges, then the current question.
function transcript(n) {
    const out = [{ role: 'system', content: 'You are Pi.', toolsAdded: [{ name: 'iss_position' }] }];
    for (let i = 0; i < n; i++) {
        out.push({ role: 'user', content: `question ${i}` });
        out.push({ role: 'assistant', content: [{ type: 'text', text: `answer ${i}` }] });
    }
    out.push({ role: 'user', content: 'the current question' });
    return out;
}

test('the threshold counts the reserve and needs a known window', () => {
    assert.equal(overThreshold(10_000, null), false);
    assert.equal(overThreshold(10_000, 0), false);
    // 90 percent of 8192 is 7372.8, counting the 1024-token reserve.
    assert.equal(overThreshold(6_300, 8192), false);
    assert.equal(overThreshold(6_400, 8192), true);
    // An 80 percent threshold would have compacted already.
    assert.equal(overThreshold(6_300, 8192, 0.8), true);
});

test('only the conversation before the kickoff is summarized; system messages are kept', () => {
    const t = transcript(3);
    t.push({ role: 'assistant', content: [{ type: 'toolCall', id: 'c1', name: 'iss_position', arguments: {} }] });
    t.push({
        role: 'toolResult',
        toolCallId: 'c1',
        toolName: 'iss_position',
        content: [{ type: 'text', text: 'over the Pacific' }],
    });
    const p = plan(t);
    assert.equal(p.at, 7);
    assert.equal(p.system.length, 1);
    assert.equal(p.older.length, 6);
    assert.deepEqual(
        p.kept.map((m) => m.role),
        ['user', 'assistant', 'toolResult'],
    );
});

test('nothing worth summarizing is no plan', () => {
    assert.equal(plan(transcript(0)), null);
    // Already a summary and a turn: summarizing again would not shrink it.
    const again = compacted(plan(transcript(3)), 'summary', 'llama3.2');
    assert.equal(plan(again), null);
});

test('a single completed exchange before a new kickoff is summarized', () => {
    const p = plan(transcript(1));
    assert.ok(p, 'one real exchange is worth summarizing');
    assert.deepEqual(
        p.older.map((m) => m.content),
        ['question 0', [{ type: 'text', text: 'answer 0' }]],
    );
    assert.equal(p.kept[0].content, 'the current question');
});

test('a large first exchange that overflows an 8,192 token window has a plan', () => {
    const t = [
        { role: 'user', content: 'u'.repeat(24_000) },
        { role: 'assistant', content: [{ type: 'text', text: 'a'.repeat(4_000) }] },
        { role: 'user', content: 'n'.repeat(12_000) },
    ];
    assert.equal(estimateTokens(t), 10_000);
    assert.ok(overThreshold(estimateTokens(t), 8192));
    const p = plan(t);
    assert.ok(p);
    assert.equal(p.older.length, 2);
    assert.equal(p.kept.length, 1);
});

test('only messages a compaction wrote count as a prior summary, never look-alike text', () => {
    // The same two messages without the compaction marker are a real exchange,
    // even when their text is the agent's preamble.
    const lookalike = [
        { role: 'user', content: SUMMARY_PREAMBLE },
        { role: 'assistant', content: [{ type: 'text', text: 'summary' }] },
        { role: 'user', content: 'next' },
    ];
    assert.ok(plan(lookalike));
    // A summary pair followed by a real exchange is summarized again, the old
    // summary with it.
    const after = compacted(plan(transcript(3)), 'summary', 'm');
    after.push({ role: 'assistant', content: [{ type: 'text', text: 'answer' }] });
    after.push({ role: 'user', content: 'and then' });
    const p = plan(after);
    assert.ok(p);
    assert.equal(p.older.length, 4);
});

test("the summary is the model's, never system text or the person's", () => {
    const p = plan(transcript(3));
    const c = compacted(p, 'We talked about the ISS.', 'llama3.2');
    assert.deepEqual(
        c.map((m) => m.role),
        ['system', 'user', 'assistant', 'user'],
    );
    assert.equal(c[1].content, SUMMARY_PREAMBLE);
    assert.equal(c[2].content[0].text, 'We talked about the ISS.');
    assert.equal(c[2].model, 'llama3.2');
    assert.ok(c[1].compaction && c[2].compaction);
    // The person's question is still the kickoff, so the turn goes on.
    assert.equal(kickoff(c), 3);
    assert.equal(c[3].content, 'the current question');
});

test('the entry has the reserved fields', () => {
    assert.deepEqual(
        entryData({
            trigger: 'threshold',
            summary: 's',
            model: 'm',
            throughEnd: 41,
            kept: 2,
            tokensBefore: 9000,
            tokensAfter: 1200,
        }),
        {
            trigger: 'threshold',
            summary: 's',
            model: 'm',
            kept: 2,
            tokensBefore: 9000,
            tokensAfter: 1200,
            through: { start: 0, end: 41 },
        },
    );
    const overflow = entryData({ trigger: 'overflow', summary: 's', model: 'm', throughEnd: null });
    assert.equal(overflow.trigger, 'overflow');
    assert.equal('through' in overflow, false);
});

test('the compacted event carries the entry fields without the summary, model or focus', () => {
    const data = entryData({
        trigger: 'threshold',
        summary: 'the summary',
        model: 'm',
        throughEnd: 41,
        kept: 2,
        tokensBefore: 9000,
        tokensAfter: 1200,
    });
    assert.deepEqual(compactionInfo(data, 44), {
        through: { start: 0, end: 41 },
        trigger: 'threshold',
        kept: 2,
        tokensBefore: 9000,
        tokensAfter: 1200,
        seq: 44,
    });
    assert.equal(compactionInfo(data, null).seq, undefined);
    // No range, no event: the event cannot say what it covers.
    assert.equal(compactionInfo(entryData({ trigger: 'overflow', summary: 's', model: 'm' }), 3), null);
});

test('the summarizer gets tool output cut short and the newest lines when it must drop some', () => {
    const big = 'x'.repeat(5000);
    const text = transcriptText([
        { role: 'user', content: 'first' },
        { role: 'toolResult', toolName: 'read', content: [{ type: 'text', text: big }] },
    ]);
    assert.ok(text.includes('[tool read]'));
    assert.ok(text.includes('3000 more characters'));
    const capped = transcriptText(
        [
            { role: 'user', content: 'oldest' },
            { role: 'user', content: 'newest' },
        ],
        20,
    );
    assert.ok(capped.includes('newest') && !capped.includes('oldest'));
});

test('estimates grow with the transcript', () => {
    assert.ok(estimateTokens(transcript(50)) > estimateTokens(transcript(1)) * 5);
});

test("the view maps the loop's full transcript onto the compacted one", () => {
    const view = new View();
    const full = transcript(3);
    assert.equal(view.apply(full), full);
    const p = plan(full);
    const next = compacted(p, 'summary', 'm');
    view.replace(full, full, p.at, next.slice(0, next.length - p.kept.length));
    assert.deepEqual(view.apply(full), next);
    // The loop goes on appending to its own copy; the view keeps up.
    const later = [...full, { role: 'assistant', content: [{ type: 'text', text: 'done' }] }];
    const viewed = view.apply(later);
    assert.equal(viewed.length, next.length + 1);
    assert.equal(viewed[viewed.length - 1].content[0].text, 'done');
    assert.equal(view.index(7), 3, "the person's message moves to just after the summary");

    // A second compaction, of an already compacted view.
    for (let i = 0; i < 3; i++) {
        later.push(
            { role: 'user', content: `more ${i}` },
            { role: 'assistant', content: [{ type: 'text', text: 'ok' }] },
        );
    }
    later.push({ role: 'user', content: 'latest' });
    const viewed2 = view.apply(later);
    const p2 = plan(viewed2);
    const next2 = compacted(p2, 'summary 2', 'm');
    view.replace(later, viewed2, p2.at, next2.slice(0, next2.length - p2.kept.length));
    assert.deepEqual(view.apply(later), next2);
    assert.equal(view.apply(later).at(-1).content, 'latest');
});

// A run's counters and limits, and a compaction whose summary call is charged
// like the agent's: `usage` is what the summary call reports.
function budgeted(limits, usage = { input: 10, output: 10 }) {
    const counts = { steps: 0, toolCalls: 0, input: 0, output: 0 };
    const calls = [];
    const compact = async (full) => {
        await charged(counts, async () => {
            calls.push(full.length);
            return { text: 'summary', usage };
        });
        const p = plan(full);
        return compacted(p, 'summary', 'm');
    };
    return { counts, calls, compact, limits: { maxSteps: null, maxTotalTokens: null, deadline: null, ...limits } };
}

// A transcript past the threshold of an 8,192 token window, with earlier
// conversation to summarize.
function overflowing() {
    return [
        { role: 'user', content: 'u'.repeat(24_000) },
        { role: 'assistant', content: [{ type: 'text', text: 'a'.repeat(4_000) }] },
        { role: 'user', content: 'n'.repeat(12_000) },
    ];
}

test('the limits read as the finish reason, tokens first, then time, then steps', () => {
    const limits = { maxSteps: 2, maxTotalTokens: 100, deadline: 50 };
    assert.equal(limitReached({ steps: 1, input: 10, output: 10 }, limits, 0), null);
    assert.equal(limitReached({ steps: 2, input: 10, output: 10 }, limits, 0), 'max-steps');
    assert.equal(limitReached({ steps: 2, input: 10, output: 10 }, limits, 50), 'max-duration');
    assert.equal(limitReached({ steps: 2, input: 60, output: 40 }, limits, 50), 'max-total-tokens');
    assert.equal(limitReached({ steps: 99, input: 1e9, output: 0 }, {}, 1e9), null, 'no limits, never reached');
});

test('a summary call counts as a model call, even when it fails', async () => {
    const counts = { steps: 0, input: 0, output: 0 };
    await charged(counts, async () => ({ usage: { input: 7, output: 3 } }));
    assert.deepEqual(counts, { steps: 1, input: 7, output: 3 });
    await assert.rejects(
        charged(counts, async () => {
            throw new Error('refused');
        }),
    );
    assert.equal(counts.steps, 2);
});

test('a failed summary call keeps the usage it reported', async () => {
    const counts = { steps: 0, input: 0, output: 0 };
    await assert.rejects(
        charged(counts, async () => {
            throw Object.assign(new Error('the model call failed'), { usage: { input: 40, output: 2 } });
        }),
        /the model call failed/,
    );
    assert.deepEqual(counts, { steps: 1, input: 40, output: 2 });
});

test('with one model call allowed, a summary is the call and the normal call does not run', async () => {
    const b = budgeted({ maxSteps: 1 });
    const r = await beforeCall(overflowing(), {
        view: new View(),
        window: 8192,
        canCompact: true,
        counts: b.counts,
        limits: b.limits,
        now: () => 0,
        compact: b.compact,
    });
    assert.equal(b.calls.length, 1, 'the summary was made');
    assert.equal(b.counts.steps, 1);
    assert.equal(r.limit, 'max-steps', 'so the run stops before the normal call');
});

test('a summary that spends the token budget is not followed by a generation', async () => {
    const b = budgeted({ maxTotalTokens: 500 }, { input: 450, output: 60 });
    const r = await beforeCall(overflowing(), {
        view: new View(),
        window: 8192,
        canCompact: true,
        counts: b.counts,
        limits: b.limits,
        now: () => 0,
        compact: b.compact,
    });
    assert.equal(b.counts.input + b.counts.output, 510);
    assert.equal(r.limit, 'max-total-tokens');
});

test('no summary is started once a limit is reached', async () => {
    for (const [limits, counts, at, reason] of [
        [{ maxSteps: 2 }, { steps: 2 }, 0, 'max-steps'],
        [{ maxTotalTokens: 100 }, { input: 100 }, 0, 'max-total-tokens'],
        [{ deadline: 10 }, {}, 10, 'max-duration'],
    ]) {
        const b = budgeted(limits);
        Object.assign(b.counts, counts);
        const r = await compactWithinLimits({
            counts: b.counts,
            limits: b.limits,
            now: () => at,
            compact: () => b.compact(overflowing()),
        });
        assert.equal(b.calls.length, 0, reason);
        assert.equal(r.limit, reason);
        assert.equal(r.next, null);
    }
});

test('within its limits a run summarizes and goes on to the normal call', async () => {
    const b = budgeted({ maxSteps: 2, maxTotalTokens: 1000, deadline: 100 });
    const full = overflowing();
    const r = await beforeCall(full, {
        view: new View(),
        window: 8192,
        canCompact: true,
        counts: b.counts,
        limits: b.limits,
        now: () => 0,
        compact: b.compact,
    });
    assert.equal(r.limit, null);
    assert.equal(b.counts.steps, 1);
    assert.ok(r.messages[0].compaction && r.messages[1].compaction);
    assert.equal(r.messages.at(-1), full.at(-1));
    // Under the threshold, or with nothing to summarize, nothing is checked or called.
    const quiet = budgeted({ maxSteps: 1 });
    quiet.counts.steps = 1;
    const small = [{ role: 'user', content: 'hi' }];
    const s = await beforeCall(small, {
        view: new View(),
        window: 8192,
        canCompact: true,
        counts: quiet.counts,
        limits: quiet.limits,
        now: () => 0,
        compact: quiet.compact,
    });
    assert.deepEqual(s, { messages: small, limit: null });
    assert.equal(quiet.calls.length, 0);
});

test("the turn starts at the person's real seq, even across gaps", () => {
    const step = 'agent.step.v1';
    // The host skipped numbers: the step is not at the commit's end - 1.
    const landed = [
        { seq: 17n, kind: 'agent.op-result.v1' },
        { seq: 20n, kind: step },
        { seq: 25n, kind: 'agent.run.v1' },
        { seq: 26n, kind: step },
    ];
    assert.deepEqual(turnStart(landed, 25, 12), { person: 20, before: 17 }, 'later writers are not ours');
    // Nothing ahead of the step in its commit: the end before it.
    assert.deepEqual(
        turnStart(
            [
                { seq: 40n, kind: step },
                { seq: 44n, kind: 'agent.run.v1' },
            ],
            44,
            31,
        ),
        {
            person: 40,
            before: 31,
        },
    );
    // A journal that starts with this turn has nothing before it.
    assert.deepEqual(turnStart([{ seq: 0n, kind: step }], 0, null), { person: 0, before: null });
    // Nothing readable: no seq is made up.
    assert.deepEqual(turnStart([], 9, 3), { person: null, before: null });
});

// A stream that records what is pushed to it and whether it was ended.
function recorder() {
    return {
        events: [],
        ended: false,
        push(ev) {
            this.events.push(ev.type === 'error' ? `error:${ev.message ?? ''}` : ev.type);
        },
        end() {
            this.ended = true;
        },
    };
}

// A model call's events, then optionally a throw.
async function* events(types, thrown) {
    for (const type of types) {
        yield { type };
    }
    if (thrown) {
        throw thrown;
    }
}

const fail = (e) => ({ type: 'error', message: e.message });

test('a model call that throws ends the stream with an error instead of hanging', async () => {
    const out = recorder();
    const never = () => assert.fail('nothing to retry');
    await pipeModelEvents(out, events(['start', 'text_delta'], new Error('boom')), {
        shouldRetry: () => false,
        retry: never,
        fail,
    });
    assert.deepEqual(out.events, ['start', 'text_delta', 'error:boom']);
    assert.equal(out.ended, true);
});

test('a retry that throws, building it or streaming it, ends the stream with an error', async () => {
    for (const retry of [
        async () => {
            throw new Error('normalizing failed');
        },
        async () => events(['start'], new Error('normalizing failed')),
    ]) {
        const out = recorder();
        await pipeModelEvents(out, events(['error']), { shouldRetry: () => true, retry, fail });
        assert.equal(out.events.at(-1), 'error:normalizing failed');
        assert.equal(out.ended, true);
    }
});

test('an overflow is retried once only before any text of the reply was streamed', async () => {
    const retried = [];
    const options = {
        shouldRetry: () => true,
        retry: async () => {
            retried.push(true);
            return events(['start', 'text_delta', 'done']);
        },
        fail,
    };
    // Refused before the reply started, or right after its start event.
    for (const first of [['error'], ['start', 'error']]) {
        const out = recorder();
        await pipeModelEvents(out, events(first), options);
        assert.deepEqual(out.events, ['start', 'text_delta', 'done'], 'one start, the retry only');
    }
    assert.equal(retried.length, 2);
    // Refused after text was streamed: the reader keeps what it saw, and the
    // refusal is passed on.
    const out = recorder();
    await pipeModelEvents(out, events(['start', 'text_start', 'text_delta', 'error']), options);
    assert.deepEqual(out.events, ['start', 'text_start', 'text_delta', 'error:']);
    assert.equal(retried.length, 2, 'no retry');
});

// A session store with conditional commits: a commit carrying `version` and
// `end` lands only when neither moved since, as the host's does.
function store() {
    const s = { version: 1, end: 3, entries: [], state: 'larger' };
    s.commit = (entries, { state, version, end, tooLarge } = {}) => {
        if (version != null && (version !== s.version || end !== s.end)) {
            return 'stale';
        }
        if (tooLarge) {
            return 'too-large';
        }
        s.entries.push(...entries);
        s.end += entries.length;
        if (state != null) {
            s.state = state;
            s.version++;
        }
        return 'committed';
    };
    return s;
}

// One run of the loop's step: compact if over the threshold, then the model
// call, which counts only when made. Superseded ends the run, as main.js
// turns it into a run that ends superseded with no further call.
async function step(messages, compact) {
    let modelCalls = 0;
    try {
        const r = await beforeCall(messages, {
            view: new View(),
            window: 8192,
            canCompact: true,
            counts: { steps: 0, input: 0, output: 0 },
            limits: {},
            now: () => 0,
            compact,
        });
        modelCalls++;
        return { status: 'done', modelCalls, messages: r.messages };
    } catch (e) {
        if (e instanceof Superseded) {
            return { status: 'superseded', modelCalls };
        }
        throw e;
    }
}

// Compact with a summary paused until `go` resolves, writing as session.js
// does: the conditional commit, then the failed op result when it cannot land.
function compactor(s, { go = Promise.resolve(), tooLarge = false } = {}) {
    return async (messages) => {
        const seen = { version: s.version, end: s.end };
        await go;
        const next = [{ role: 'assistant', content: 'summary' }, messages.at(-1)];
        const outcome = await closeCompaction({
            owned: true,
            install: async () =>
                s.commit([{ kind: 'agent.op-result.v1', outcome: 'succeeded' }, { kind: 'agent.compaction.v1' }], {
                    state: 'smaller',
                    ...seen,
                    tooLarge,
                }),
            closeFailed: async () => {
                s.commit([{ kind: 'agent.op-result.v1', outcome: 'failed' }]);
            },
        });
        if (outcome === 'superseded') {
            throw new Superseded();
        }
        return outcome === 'compacted' ? next : null;
    };
}

const crowded = () => [
    { role: 'user', content: 'old '.repeat(10000) },
    { role: 'assistant', content: [{ type: 'text', text: 'prior' }] },
    { role: 'user', content: 'new' },
];

test('a competing commit while the summary runs ends the run superseded, with no model call', async () => {
    const s = store();
    let release;
    const go = new Promise((r) => (release = r));
    const running = step(crowded(), compactor(s, { go }));
    // Another writer commits while the summary is in flight.
    s.commit([{ kind: 'agent.step.v1' }], { state: 'theirs', version: s.version, end: s.end });
    release();
    const r = await running;
    assert.equal(r.status, 'superseded');
    assert.equal(r.modelCalls, 0, 'no model call after losing the session');
    assert.equal(s.state, 'theirs', 'the other writer keeps the state');
    assert.deepEqual(
        s.entries.map((e) => [e.kind, e.outcome]),
        [
            ['agent.step.v1', undefined],
            ['agent.op-result.v1', 'failed'],
        ],
        'the summary op is closed as failed, with no compaction entry',
    );
});

test('a run that already lost the session makes no further model call, nor a summary', async () => {
    let summarized = false;
    let modelCalls = 0;
    let status = 'done';
    try {
        await beforeCall(crowded(), {
            view: new View(),
            window: 8192,
            canCompact: true,
            counts: { steps: 0, input: 0, output: 0 },
            limits: {},
            now: () => 0,
            compact: async () => {
                summarized = true;
                return null;
            },
            lost: () => true,
        });
        modelCalls++;
    } catch (e) {
        assert.ok(e instanceof Superseded);
        status = 'superseded';
    }
    assert.equal(status, 'superseded');
    assert.equal(modelCalls, 0);
    assert.equal(summarized, false);
});

test('a failed summary that loses the session ends the run superseded, with no model call', async () => {
    const s = store();
    let release;
    const go = new Promise((r) => (release = r));
    // As session.js closes a failed summary: the failed result with the
    // unchanged transcript under conditions, then again without them.
    const failing = async () => {
        const seen = { version: s.version, end: s.end };
        await go;
        const r = s.commit([{ kind: 'agent.op-result.v1', outcome: 'failed' }], { state: 'larger', ...seen });
        if (r === 'committed') {
            return null;
        }
        s.commit([{ kind: 'agent.op-result.v1', outcome: 'failed' }]);
        throw new Superseded();
    };
    const running = step(crowded(), failing);
    s.commit([{ kind: 'agent.step.v1' }], { state: 'theirs', version: s.version, end: s.end });
    release();
    const r = await running;
    assert.equal(r.status, 'superseded');
    assert.equal(r.modelCalls, 0);
    assert.equal(s.state, 'theirs');
    assert.deepEqual(
        s.entries.map((e) => [e.kind, e.outcome]),
        [
            ['agent.step.v1', undefined],
            ['agent.op-result.v1', 'failed'],
        ],
    );
});

test('a compaction too large to save is harmless: the run goes on uncompacted', async () => {
    const s = store();
    const messages = crowded();
    const r = await step(messages, compactor(s, { tooLarge: true }));
    assert.equal(r.status, 'done');
    assert.equal(r.modelCalls, 1);
    assert.deepEqual(r.messages, messages, 'the transcript it had');
    assert.equal(s.state, 'larger');
    assert.deepEqual(
        s.entries.map((e) => e.outcome),
        ['failed'],
    );
});

test('a writer that already lost the session does not try the commit and stops', async () => {
    let tried = false;
    let closed = false;
    const outcome = await closeCompaction({
        owned: false,
        install: async () => {
            tried = true;
            return 'committed';
        },
        closeFailed: async () => {
            closed = true;
        },
    });
    assert.equal(outcome, 'superseded');
    assert.equal(tried, false);
    assert.equal(closed, true);
});

test('a requested compaction records the focus, and its result whether it summarized', () => {
    const data = entryData({
        trigger: 'requested',
        summary: 's',
        model: 'm',
        throughEnd: 7,
        kept: 2,
        focus: 'the API',
    });
    assert.equal(data.trigger, 'requested');
    assert.equal(data.focus, 'the API');
    assert.equal('focus' in entryData({ trigger: 'threshold', summary: 's', model: 'm' }), false);
    assert.deepEqual(compactResultData('c1', 'done', { finish: 'done' }, true), {
        compact: 'c1',
        outcome: 'done',
        finish: 'done',
        compacted: true,
    });
    assert.deepEqual(compactResultData('c1', 'failed', { error: 'superseded' }), {
        compact: 'c1',
        outcome: 'failed',
        error: 'superseded',
        compacted: false,
    });
});

test('a compaction id is known by its accepted entry, a namespace apart from runs', () => {
    const enc = new TextEncoder();
    const entries = [
        { kind: 'agent.run.v1', data: enc.encode(JSON.stringify({ run: 'c2' })) },
        { kind: 'agent.compact.v1', data: enc.encode(JSON.stringify({ compact: 'c1', focus: 'x' })) },
    ];
    assert.equal(compactionAccepted(entries, 'c1'), true);
    assert.equal(compactionAccepted(entries, 'c2'), false);
});

test("a requested summary covers the journal up to the person's latest message", () => {
    const step = (seq, role) => ({ seq, kind: 'agent.step.v1', data: JSON.stringify({ role }) });
    const entries = [
        step(0, 'user'),
        { seq: 1, kind: 'agent.run.v1', data: '{}' },
        step(2, 'assistant'),
        { seq: 3, kind: 'agent.run-result.v1', data: '{}' },
        step(5, 'user'),
        { seq: 6, kind: 'agent.run.v1', data: '{}' },
        step(7, 'assistant'),
        { seq: 9, kind: 'agent.run-result.v1', data: '{}' },
        // After what the state covers: another writer's.
        step(10, 'user'),
    ];
    assert.equal(beforeLastPerson(entries, 9), 3, 'across the gap at 4');
    assert.equal(beforeLastPerson(entries.slice(0, 4), 3), null, 'one exchange: nothing before it');
    assert.equal(beforeLastPerson([], 3), null);
});
