import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { modelTurn } from '../lib/model-turns.mjs';
import { handleRunMessage } from '../lib/run-messages.mjs';
import { launch, owner, root } from './helper.mjs';

const usage = { input_tokens: 13, output_tokens: 11, cache_read_tokens: 7, cache_write_tokens: 5,
  cache_write_1h_tokens: 2, reasoning_tokens: 3, total_tokens: 36 };
const cost = { input: 101, output: 203, reasoning: 17, cache_read: 29, cache_write: 31, total: 383, total_picodollars_remainder: 400000 };
const piUsage = { input: 13, output: 11, cacheRead: 7, cacheWrite: 5, cacheWrite1h: 2, reasoning: 3, totalTokens: 36,
  cost: { input: 101 / 1e6, output: 220 / 1e6, cacheRead: 29 / 1e6, cacheWrite: 31 / 1e6, total: 383 / 1e6 + 400000 / 1e12 } };
const entry = { id: 'committed-assistant', parent: null, timestamp_unix_ms: 1200,
  value: { type: 'message', Assistant: { model: 'actual-response-model', protocol: 'open_ai_chat', content: [{ Text: 'actual answer' }] } } };
const metadata = (overrides = {}) => ({ assistant_entry_id: entry.id, model: 'actual-response-model', usage: structuredClone(usage), cost: structuredClone(cost), stop_reason: 'end_turn', ...overrides });
const payload = (details = metadata()) => ({ kind: 'model_turn_end', run_id: 'run:input', turn_index: 0, timestamp_ms: 1300,
  assistant_entry: structuredClone(entry), assistant_metadata: details, tool_result_entries: [] });
function harness() {
  const events = [], issues = [];
  const runtime = { require() {}, bind() {}, assertOwner() {}, assertSessionOwner() {}, metadata: () => ({ hooks: ['model_turn_start', 'model_turn_end'] }),
    queued: (_store, work) => work(), flush: async () => {}, runEvent: async (_type, event) => events.push(event),
    reportObservationError: event => issues.push(event) };
  const store = { leaf: { grant: true }, controller: new AbortController(), state: { host: { model: 'wrong-current-model', model_view: { api: 'openai-responses', provider: 'wrong-current-provider' } } } };
  const turn = body => modelTurn(runtime, { hook: body.kind, payload: body, context: { resource_owner: owner } }, store);
  const wire = (method, body = {}) => handleRunMessage(runtime, method, body, store);
  return { events, issues, runtime, store, turn, wire };
}

for (const [native, pi] of [['end_turn', 'stop'], ['stop_sequence', 'stop'], ['pause_turn', 'stop'], ['max_tokens', 'length'], ['tool_use', 'toolUse'], ['refusal', 'error']]) {
  test(`committed metadata projects native ${native} without changing usage or response model`, async () => {
    const h = harness();
    await h.turn(payload(metadata({ stop_reason: native })));
    assert.deepEqual(h.issues, []);
    const message = h.events.find(event => event.type === 'turn_end').message;
    assert.equal(message.model, 'actual-response-model');
    assert.equal(message.api, 'openai-completions');
    assert.equal(message.stopReason, pi);
    assert.deepEqual(message.usage, piUsage);
    assert.equal(message.timestamp, 1200);
    if (native === 'refusal') assert.equal(message.errorMessage, 'The model refused to complete the request');
    assert.equal(Object.hasOwn(message, 'provider'), false, 'current provider is not historical response identity');
    assert.equal(Object.hasOwn(message, 'rawStopReason'), false, 'canonical stop is not provider-native raw stop metadata');
  });
}

test('committed per-message metadata survives message_end, turn_end and agent_end, not current model or run totals', async () => {
  const h = harness();
  await h.wire('turn/started');
  await h.wire('message/updated', { delta: 'provisional' });
  const initial = h.events.find(event => event.type === 'message_start').message;
  assert.equal(initial.stopReason, 'pending');
  assert.equal(initial.usage.totalTokens, 0);
  await h.turn(payload());
  const second = payload(metadata({ assistant_entry_id: 'second-assistant', model: 'second-model', stop_reason: 'max_tokens',
    usage: { ...usage, input_tokens: 1, total_tokens: 24 } }));
  second.turn_index = 1; second.assistant_entry.id = 'second-assistant'; second.assistant_entry.value.Assistant.model = 'second-model';
  await h.turn(second);
  await h.wire('turn/settled', { outcome: 'completed' });
  const messages = h.events.filter(event => event.type === 'message_end').map(event => event.message);
  const turns = h.events.filter(event => event.type === 'turn_end').map(event => event.message);
  const final = h.events.find(event => event.type === 'agent_end').messages;
  assert.deepEqual(messages, turns);
  assert.deepEqual(final, turns);
  assert.deepEqual(final.map(message => [message.model, message.stopReason, message.usage.input, message.usage.totalTokens]),
    [['actual-response-model', 'stop', 13, 36], ['second-model', 'length', 1, 24]]);
  assert.equal(initial.stopReason, 'pending', 'old partial snapshots stay provisional');
  assert.equal(initial.usage.totalTokens, 0);
  // Fleet-notify's normal completion guard and final text access must not throw.
  const last = final.filter(message => message.role === 'assistant').at(-1);
  assert.equal(['aborted', 'error'].includes(last.stopReason), false);
  assert.equal(last.content.filter(part => part.type === 'text').map(part => part.text).join(''), 'actual answer');
});

test('unpriced committed usage stays available, but every final observation refuses fictional cost', async () => {
  const h = harness();
  await h.wire('turn/started');
  await h.turn(payload(metadata({ cost: null })));
  await h.wire('turn/settled', { outcome: 'completed' });
  assert.deepEqual(h.issues, []);
  for (const message of [h.events.find(e => e.type === 'message_end').message,
    h.events.find(e => e.type === 'turn_end').message, h.events.at(-1).messages[0]]) {
    assert.equal(message.stopReason, 'stop');
    assert.equal(message.usage.totalTokens, 36);
    assert.throws(() => message.usage.cost, /unsupported_feature .*usage.cost/);
  }
});

for (const native of ['deferred', 'steered', 'unrecognized-provider-status']) {
  test(`unrepresentable committed stop ${native} is reported, not turned into successful completion`, async () => {
    const h = harness();
    await h.wire('turn/started');
    await h.wire('message/updated', { delta: 'unfinished observation' });
    await h.turn(payload(metadata({ stop_reason: native })));
    await h.wire('turn/settled', { outcome: 'completed' });
    assert.deepEqual(h.issues, ['turn_end']);
    assert.equal(h.events.some(e => e.type === 'turn_end'), false);
    const final = h.events.at(-1).messages[0];
    assert.throws(() => final.stopReason, /unsupported_feature .*stopReason/);
    assert.throws(() => final.usage, /unsupported_feature .*usage/);
  });
}

for (const [name, change] of [
  ['another assistant entry', data => { data.assistant_entry_id = 'not-this-entry'; }],
  ['another response model', data => { data.model = 'not-this-model'; }],
  ['unsafe token integer', data => { data.usage.input_tokens = Number.MAX_SAFE_INTEGER + 1; }],
  ['negative token count', data => { data.usage.output_tokens = -1; }],
  ['unknown usage field', data => { data.usage.fabricated = 1; }],
  ['invalid cost remainder', data => { data.cost.total_picodollars_remainder = 1000000; }],
]) {
  test(`committed metadata refuses ${name} without borrowing current or aggregate facts`, async () => {
    const h = harness(), data = metadata(); change(data);
    await h.turn(payload(data));
    assert.deepEqual(h.issues, ['turn_end']);
    assert.equal(h.events.some(e => e.type === 'turn_end'), false);
  });
}

test('missing native accounting is explicitly unavailable on all final observations', async () => {
  const h = harness();
  await h.wire('turn/started');
  await h.turn(payload(null));
  await h.wire('turn/settled', { outcome: 'completed' });
  for (const message of [h.events.find(e => e.type === 'message_end').message,
    h.events.find(e => e.type === 'turn_end').message, h.events.at(-1).messages[0]]) {
    assert.equal(message.model, 'actual-response-model');
    assert.throws(() => message.usage, /unsupported_feature .*usage/);
    assert.throws(() => message.stopReason, /unsupported_feature .*stopReason/);
  }
});

test('legacy accounting without a recorded stop reason retains counts without inventing completion', async () => {
  const h = harness();
  await h.turn(payload(metadata({ model: null, stop_reason: null })));
  const message = h.events.find(event => event.type === 'turn_end').message;
  assert.equal(message.model, 'actual-response-model');
  assert.deepEqual(message.usage, piUsage);
  assert.throws(() => message.stopReason, /unsupported_feature .*stopReason/);
});

for (const outcome of ['completed', 'cancelled', 'failed', 'limit_reached']) {
  test(`uncommitted ${outcome} stream never publishes pending or zero accounting as a final fact`, async () => {
    const h = harness();
    h.runtime.metadata = () => ({ hooks: [] });
    await h.wire('turn/started');
    await h.wire('message/updated', { delta: 'partial answer' });
    await h.wire('message/settled');
    await h.wire('turn/settled', { outcome });
    const final = h.events.find(event => event.type === 'message_end').message;
    assert.deepEqual(final.content, [{ type: 'text', text: 'partial answer' }]);
    assert.throws(() => final.usage, /unsupported_feature .*usage/);
    assert.throws(() => final.model, /unsupported_feature .*model/);
    // A failed/cancelled run can follow a successful provider response: this
    // coarse outcome is not authority for that assistant's provider stop.
    assert.throws(() => final.stopReason, /unsupported_feature .*stopReason/);
    assert.deepEqual(h.events.at(-1).messages, [final]);
  });
}

for (const outcome of ['failed', 'cancelled']) {
  test(`a later ${outcome} run cannot rewrite a committed assistant's successful stop`, async () => {
    const h = harness();
    await h.wire('turn/started');
    await h.turn(payload());
    await h.wire('turn/settled', { outcome });
    assert.equal(h.events.at(-1).messages[0].stopReason, 'stop');
    assert.deepEqual(h.events.at(-1).messages[0].usage, piUsage);
  });
}

for (const fixture of ['run-messages-turns', 'run-messages']) {
  test(`real process ${fixture} notifications retain committed final metadata without requiring a turn_end subscriber`, async t => {
    const peer = launch(t, [join(root, `test/fixtures/${fixture}.ts`)]);
    assert.ok(peer.metadata.hooks.includes('model_turn_end'));
    await peer.init(['remote_ui', 'session_entries', 'lifecycle_events', 'lifecycle_events_v2', 'before_prompt_state_v1']);
    await peer.start();
    const before = await peer.request('hook/run', { hook: 'before_prompt', payload: { prompt: 'question', system_prompt: 'system' }, context: peer.context() }).response;
    assert.ok(before.result, JSON.stringify(before));
    peer.notify('turn/started', { resource_owner: owner });
    peer.notify('message/updated', { resource_owner: owner, message_id: 'stream', delta: 'partial', deltas: 1 });
    const body = payload();
    const reply = await peer.request('hook/run', { hook: body.kind, payload: body,
      context: peer.context({ session_entries: [entry], session_branch: [entry], session_leaf_id: entry.id }),
      session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'metadata:2', owner, expected_head: entry.id } }).response;
    assert.ok(reply.result, JSON.stringify(reply));
    peer.notify('turn/settled', { resource_owner: owner, outcome: 'completed' });
    await peer.wait(frame => frame.method === 'notification' && frame.params.message?.startsWith('evt:{"type":"agent_end"'));
    const events = peer.seen.filter(frame => frame.method === 'notification' && frame.params.message?.startsWith('evt:')).map(frame => JSON.parse(frame.params.message.slice(4)));
    const messages = [events.find(event => event.type === 'message_end' && event.message.role === 'assistant').message,
      events.at(-1).messages.find(message => message.role === 'assistant')];
    if (fixture === 'run-messages-turns') messages.push(events.find(event => event.type === 'turn_end').message);
    for (const message of messages) {
      assert.equal(message.stopReason, 'stop');
      assert.deepEqual(message.usage, piUsage);
      assert.equal(message.model, 'actual-response-model');
    }
    assert.equal(peer.seen.some(frame => frame.method === 'notification' && frame.params.title === '[Extension issues]'), false);
    await peer.close();
  });
}
