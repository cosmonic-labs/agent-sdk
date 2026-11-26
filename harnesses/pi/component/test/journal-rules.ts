/*
 * Test Pi journal recovery, tool outcomes, and transcript formatting with
 * Node's test runner; this module registers tests rather than a command-line main.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
    FREEFORM_TEXT,
    agentArguments,
    agentSchema,
    offeredInput,
    rememberListed,
    toolArguments,
} from '../../../shared/js/tool-input.ts';
import {
    OP_KIND,
    OP_RESULT_KIND,
    STEP_KIND,
    closingEntries,
    outcomeForCallError,
    outcomeForToolError,
    stopsTheRun,
    outcomeOf,
    readableTranscript,
    resolutionEntries,
    stepWithoutImages,
    toolOps,
    unresolvedOps,
} from '../src/journal-rules.ts';

const enc = new TextEncoder();
const e = (kind: string, v: unknown) => ({ kind, data: enc.encode(typeof v === 'string' ? v : JSON.stringify(v)) });

test('an op with a result is resolved; one without is not, in order', () => {
    const entries = [
        e(STEP_KIND, { role: 'user', text: 'hi' }),
        e(OP_KIND, { op: 'call_1', target: 'tool', name: 'iss_position' }),
        e(OP_KIND, { op: 'call_2', target: 'tool', name: 'distance_km' }),
        e(OP_RESULT_KIND, { op: 'call_1', outcome: 'succeeded' }),
        e(OP_KIND, { op: 'm1', target: 'model', name: 'default' }),
        e(OP_KIND, 'not json'),
    ];
    assert.deepEqual(
        unresolvedOps(entries).map((u) => u.op),
        ['call_2', 'm1'],
    );
});

test('an unresolved tool call is reported, never repeated', () => {
    const out = resolutionEntries([{ op: 'call_2', target: 'tool', name: 'distance_km' }]);
    assert.equal(out.length, 2);
    assert.deepEqual(out[0], [OP_RESULT_KIND, { op: 'call_2', outcome: 'unresolved' }]);
    assert.equal(out[1][0], STEP_KIND);
    assert.equal(out[1][1].role, 'error');
    assert.deepEqual(out[1][1].tools, ['distance_km']);
    assert.match(out[1][1].text, /call_2/);
});

test('an unresolved model call is closed and left to repeat', () => {
    assert.deepEqual(resolutionEntries([{ op: 'm1', target: 'model', name: 'default' }]), [
        [OP_RESULT_KIND, { op: 'm1', outcome: 'cancelled' }],
    ]);
});

test('a step too large for its images loses only them', () => {
    const withoutImages = JSON.parse(
        new TextDecoder().decode(
            stepWithoutImages(
                enc.encode(JSON.stringify({ role: 'tool', text: 'plotted', images: ['data:image/png;base64,AA'] })),
            )!,
        ),
    );
    assert.deepEqual(withoutImages.images, []);
    assert.equal(withoutImages.imagesDropped, true);
    assert.equal(withoutImages.text, 'plotted');
    assert.equal(stepWithoutImages(enc.encode(JSON.stringify({ role: 'user', text: 'x' }))), null);
});

test('each tool call becomes an op keyed by its call id, arguments included', () => {
    const ops = toolOps({
        role: 'assistant',
        content: [
            { type: 'text', text: 'checking' },
            { type: 'toolCall', id: 'call_9', name: 'distance_km', arguments: { lat1: 1 } },
        ],
    });
    assert.deepEqual(ops, [
        { id: 'call_9', data: { op: 'call_9', target: 'tool', name: 'distance_km', arguments: { lat1: 1 } } },
    ]);
});

test('a freeform tool call becomes an op recording its text', () => {
    const text = '*** Begin Patch\n*** End Patch\n';
    const ops = toolOps({
        role: 'assistant',
        content: [
            {
                type: 'toolCall',
                id: 'call_p',
                name: 'apply_patch',
                arguments: agentArguments({ tag: 'text', val: text }),
            },
        ],
    });
    assert.deepEqual(ops, [
        { id: 'call_p', data: { op: 'call_p', target: 'tool', name: 'apply_patch', arguments: text } },
    ]);
});

test('tool arguments pass through as the model wrote them', () => {
    const text = '{"not": "parsed"}';
    assert.deepEqual(toolArguments(agentArguments({ tag: 'text', val: text })), { tag: 'text', val: text });
    assert.deepEqual(toolArguments(agentArguments({ tag: 'json', val: '{"lat1":1}' })), {
        tag: 'json',
        val: '{"lat1":1}',
    });
    assert.deepEqual(agentArguments({ tag: 'json', val: 'not json' }), {});
});

test('listed tools are offered as listed, freeform included', () => {
    const freeform = { tag: 'grammar', val: { syntax: 'lark', definition: 'start: /.+/' } } as const;
    const text = { tag: 'text' } as const;
    const schema = { tag: 'json-schema', val: '{ "type": "object" }' } as const;
    rememberListed([
        { name: 'apply_patch', input: freeform },
        { name: 'note', input: text },
        { name: 'search', input: schema },
    ]);
    assert.deepEqual(offeredInput({ name: 'apply_patch', parameters: agentSchema(freeform) }), freeform);
    assert.deepEqual(offeredInput({ name: 'note', parameters: agentSchema(text) }), text);
    assert.deepEqual(agentSchema(text), {
        type: 'object',
        properties: { [FREEFORM_TEXT]: { type: 'string' } },
        required: [FREEFORM_TEXT],
    });
    assert.deepEqual(offeredInput({ name: 'search', parameters: agentSchema(schema) }), schema);
    assert.deepEqual(offeredInput({ name: 'own', parameters: { type: 'object' } }), {
        tag: 'json-schema',
        val: '{"type":"object"}',
    });
    assert.deepEqual(agentSchema(freeform), agentSchema(text));
});

test('closing an op never claims it succeeded unless it did', () => {
    assert.equal(outcomeOf('committed'), 'cancelled');
    assert.equal(outcomeOf('dispatched'), 'unresolved');
    assert.equal(outcomeOf({ done: 'failed' }), 'failed');
});

test('a call dropped mid-flight is unresolved and reported', () => {
    const out = closingEntries('call_3', 'send_email', 'dispatched');
    assert.equal(out.length, 2);
    assert.deepEqual(out[0], [OP_RESULT_KIND, { op: 'call_3', outcome: 'unresolved' }]);
    assert.equal(out[1][0], STEP_KIND);
    assert.equal(out[1][1].role, 'error');
    assert.deepEqual(out[1][1].tools, ['send_email']);
    assert.match(out[1][1].text, /call_3/);
    // The tool host reporting the call cancelled is the same: it was made.
    assert.deepEqual(closingEntries('call_3', 'send_email', { done: 'unresolved' }), out);
});

test('a call never made is cancelled without an error step', () => {
    assert.deepEqual(closingEntries('call_4', 'send_email', 'committed'), [
        [OP_RESULT_KIND, { op: 'call_4', outcome: 'cancelled' }],
    ]);
    assert.deepEqual(closingEntries('call_5', 'send_email', { done: 'succeeded' }), [
        [OP_RESULT_KIND, { op: 'call_5', outcome: 'succeeded' }],
    ]);
});

test('an old transcript format is recognized as unreadable', () => {
    assert.equal(readableTranscript([{ role: 'user', content: 'hi' }]), true);
    assert.equal(readableTranscript({ messages: [] }), false);
    assert.equal(readableTranscript([{ content: 'no role' }]), false);
});

test('a call that was cancelled or timed out is unresolved; other errors failed', () => {
    assert.equal(outcomeForCallError('cancelled'), 'unresolved');
    assert.equal(outcomeForCallError('timed-out'), 'unresolved');
    assert.equal(outcomeForCallError('upstream'), 'failed');
    assert.equal(outcomeForCallError(undefined), 'failed');
});

test('a tools@0.4.0 call that never started is cancelled; one that may have run is unresolved', () => {
    const notDispatched = (tag: string) => ({ tag: 'not-dispatched', val: { tag } });
    assert.equal(outcomeForToolError(notDispatched('unknown-tool')), 'cancelled');
    assert.equal(outcomeForToolError(notDispatched('denied')), 'cancelled');
    assert.equal(outcomeForToolError({ tag: 'outcome-unknown', val: { tag: 'timed-out' } }), 'unresolved');
    assert.equal(outcomeForToolError(undefined), 'unresolved');
    // A read-only tool that timed out has nothing to resolve.
    assert.equal(outcomeForToolError({ tag: 'outcome-unknown', val: { tag: 'timed-out' } }, true), 'failed');
});

test('an unknown outcome stops the run, except a timeout on a read-only tool', () => {
    const unknown = (tag: string) => ({ tag: 'outcome-unknown', val: { tag } });
    assert.equal(stopsTheRun({ tag: 'not-dispatched', val: { tag: 'timed-out' } }), false);
    assert.equal(stopsTheRun(unknown('upstream'), true), true);
    assert.equal(stopsTheRun(unknown('cancelled'), true), true);
    assert.equal(stopsTheRun(unknown('timed-out')), true);
    assert.equal(stopsTheRun(unknown('timed-out'), false), true);
    assert.equal(stopsTheRun(unknown('timed-out'), true), false);
    assert.equal(stopsTheRun(undefined), true);
});
