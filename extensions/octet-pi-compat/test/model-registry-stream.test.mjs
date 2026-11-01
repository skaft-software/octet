import test from 'node:test';
import assert from 'node:assert/strict';
import { modelRegistry } from '../lib/providers.mjs';
import { host } from './helper.mjs';

function fixture(features = ['nested_inference_v1']) {
  const frames = [], store = { id: 7, method: 'tool/call', live: true, controller: new AbortController(), state: { alive: true, owner: { session_id: 'stream-session', extension_instance_id: 'stream-instance', process_generation: 1 } } };
  let settle;
  const runtime = { features: new Set(features), stopping: false,
    current: () => store, assertOwner(value) { assert.equal(value, store); },
    require(feature) { if (!this.features.has(feature)) throw new Error(`unsupported_feature ${feature}`); },
    hostCall(method, params, value) { assert.equal(value, store); frames.push({ method, params }); return new Promise(resolve => { settle = resolve; }); },
    track: work => work,
    transport: { childId: 1, notify: async (method, params) => { frames.push({ method, params }); } },
    backgroundError(error) { throw error; },
  };
  return { runtime, store, frames, registry: modelRegistry(() => ({ pi_models: { all_models: [host.model_view] } }), runtime),
    settle: value => settle(value),
    async event(kind, payload = {}, sequence = undefined) {
      const { registryInferenceEvent } = await import('../lib/model-registry-stream.mjs');
      return registryInferenceEvent(runtime, { inference_id: frames[0].params.inference_id, sequence: sequence ?? this.sequence++, kind, payload });
    }, sequence: 0,
  };
}
const context = { systemPrompt: 'judge the proposal', messages: [{ role: 'user', content: 'proposal', timestamp: 1 }] };
const model = { provider: host.model_view.provider, id: host.model_view.id, baseUrl: 'https://ignored.invalid', cost: { output: 99999 } };
const final = () => ({ message: { content: [{ Text: 'advice' }], model: 'native-model', protocol: 'open_ai_chat' },
  usage: { input_tokens: 2, output_tokens: 3, cache_read_tokens: 0, cache_write_tokens: 0, cache_write_1h_tokens: 0, reasoning_tokens: 0, total_tokens: 5 },
  cost: { input: 2, output: 3, reasoning: 0, cache_read: 0, cache_write: 0, total: 5, total_picodollars_remainder: 123 },
  stop_reason: 'end_turn', timestamp: 42 });

test('registry stream returns synchronously, exposes early host deltas and resolves one authoritative result', async () => {
  const value = fixture(), stream = value.registry.streamSimple(model, context, { reasoning: 'low', maxTokens: 32 });
  assert.equal(typeof stream.then, 'undefined');
  assert.equal(stream.result(), stream.result());
  const iterator = stream[Symbol.asyncIterator]();
  await value.event('started');
  assert.equal((await iterator.next()).value.type, 'start');
  await value.event('text_start', { index: 0 });
  await value.event('text_delta', { index: 0, delta: 'advice' });
  assert.equal((await iterator.next()).value.type, 'text_start');
  assert.equal((await iterator.next()).value.delta, 'advice');
  assert.equal(value.frames[0].method, 'provider/infer');
  assert.deepEqual(value.frames[0].params.model, { provider: model.provider, id: model.id });
  assert.equal(value.frames[0].params.request.messages[0].User.content[0].Text, 'proposal');
  assert.doesNotMatch(JSON.stringify(value.frames), /ignored.invalid|99999|apiKey|credentials/);
  await value.event('text_end', { index: 0 });
  value.settle(final());
  const events = [];
  for await (const event of iterator) events.push(event);
  const result = await stream.result();
  assert.equal(events.at(-1).message, result);
  assert.equal(result.model, model.id);
  assert.equal(result.provider, model.provider);
  assert.equal(result.timestamp, 42);
  assert.equal(result.content[0].text, 'advice');
  assert.equal(result.usage.totalTokens, 5);
  assert.equal(result.usage.cost.total, 5 / 1e6 + 123 / 1e12);
});

test('unnegotiated inference ends as a Pi error stream without credential or transport fallback', async () => {
  const value = fixture([]), stream = value.registry.streamSimple(model, context);
  const events = [];
  for await (const event of stream) events.push(event);
  assert.equal(events[0].type, 'error');
  assert.equal((await stream.result()).stopReason, 'error');
  assert.deepEqual(value.frames, []);
});

for (const options of [{ apiKey: 'not-authority' }, { headers: {} }, { fetch() {} }, { reasoning: 'ultra' }]) {
  test(`registry inference refuses unsupported options ${Object.keys(options)}`, async () => {
    const value = fixture(), stream = value.registry.streamSimple(model, context, options);
    assert.equal((await stream.result()).stopReason, 'error');
    assert.deepEqual(value.frames, []);
  });
}

test('caller abort cancels the native attempt but waits for its accounted terminal outcome', async () => {
  const value = fixture(), controller = new AbortController();
  const stream = value.registry.streamSimple(model, context, { signal: controller.signal });
  await value.event('started');
  controller.abort();
  await Promise.resolve();
  assert.equal(value.frames[1].method, 'provider/infer/cancel');
  assert.equal(value.frames[1].params.id, 'pi:1');
  let finished = false;
  stream.result().then(() => { finished = true; });
  await Promise.resolve();
  assert.equal(finished, false);
  value.settle({ cancelled: true });
  assert.equal((await stream.result()).stopReason, 'aborted');
});

test('parent abort through real Runtime and Transport waits for native settlement', async () => {
  const { Runtime } = await import('../lib/runtime.mjs');
  const { Transport } = await import('../lib/transport.mjs');
  const value = fixture();
  const transport = new Transport(null, { onMessage() {}, onLost() {} });
  transport.send = async frame => { value.frames.push(frame); };
  const request = transport.request.bind(transport);
  transport.request = (method, params, options) => {
    assert.equal(options.timeout, 60000);
    return request(method, params, options);
  };
  value.runtime.transport = transport;
  value.runtime.hostCall = Runtime.prototype.hostCall;
  value.runtime.scope = { getStore: () => value.store };
  value.runtime.active = new Map([[value.store.id, value.store]]);
  const stream = value.registry.streamSimple(model, context);
  assert.equal(value.frames[0]?.method, 'provider/infer');
  let finished = false;
  stream.result().then(() => { finished = true; });
  try {
    value.store.controller.abort();
    await new Promise(setImmediate);
    assert.equal(finished, false);
    assert.equal(value.frames[1].method, 'provider/infer/cancel');
    assert.equal(value.frames[1].params.id, 'pi:1');
    transport.response({ jsonrpc: '2.0', id: 'pi:1', result: { cancelled: true } });
    assert.equal((await stream.result()).stopReason, 'aborted');
  } finally { transport.fail(new Error('test complete')); }
});

test('an authoritative native child cancellation is an aborted Pi terminal', async () => {
  const { Runtime } = await import('../lib/runtime.mjs');
  const { Transport } = await import('../lib/transport.mjs');
  const value = fixture();
  const transport = new Transport(null, { onMessage() {}, onLost() {} });
  transport.send = async frame => { value.frames.push(frame); };
  value.runtime.transport = transport;
  value.runtime.hostCall = Runtime.prototype.hostCall;
  value.runtime.scope = { getStore: () => value.store };
  value.runtime.active = new Map([[value.store.id, value.store]]);
  const stream = value.registry.streamSimple(model, context);
  assert.equal(value.frames[0]?.method, 'provider/infer');
  try {
    transport.cancel('pi:1');
    assert.equal((await stream.result()).stopReason, 'aborted');
  } finally { transport.fail(new Error('test complete')); }
});

test('event sequence gaps cancel rather than silently lose nested model output', async () => {
  const value = fixture(), stream = value.registry.streamSimple(model, context);
  await assert.rejects(value.event('started', {}, 3), /sequence/);
  assert.equal(value.frames[1].method, 'provider/infer/cancel');
  value.settle(final());
  assert.equal((await stream.result()).stopReason, 'error');
});

test('commands and nonempty tool contexts cannot acquire native inference authority', async () => {
  const command = fixture();
  command.store.method = 'command/execute';
  assert.equal((await command.registry.streamSimple(model, context).result()).stopReason, 'error');
  assert.deepEqual(command.frames, []);
  const tool = fixture();
  assert.equal((await tool.registry.streamSimple(model, { ...context, tools: [{ name: 'read' }] }).result()).stopReason, 'error');
  assert.deepEqual(tool.frames, []);
});

test('exiting the async iterator cancels transport and still leaves result() awaiting settlement', async () => {
  const value = fixture(), stream = value.registry.streamSimple(model, context);
  await value.event('started');
  for await (const event of stream) { assert.equal(event.type, 'start'); break; }
  assert.equal(value.frames[1].method, 'provider/infer/cancel');
  value.settle({ cancelled: true });
  assert.equal((await stream.result()).stopReason, 'aborted');
  assert.deepEqual(await stream.next(), { value: undefined, done: true });
});

test('a malformed or mismatched terminal reply cannot fabricate a successful Pi answer', async () => {
  const value = fixture(), stream = value.registry.streamSimple(model, context);
  await value.event('started');
  value.settle(final());
  const result = await stream.result();
  assert.equal(result.stopReason, 'error');
  assert.equal(result.usage, undefined);
});

test('bounded event queues cancel overflow without treating a later native reply as success', async () => {
  const value = fixture(), stream = value.registry.streamSimple(model, context);
  await value.event('started');
  await value.event('text_start', { index: 0 });
  let rejected = false;
  for (let index = 0; index < 1100; index++) {
    try { await value.event('text_delta', { index: 0, delta: 'x' }); }
    catch (error) { assert.match(error.message, /bound/); rejected = true; break; }
  }
  assert.equal(rejected, true);
  assert.equal(value.frames[1].method, 'provider/infer/cancel');
  value.settle(final());
  assert.equal((await stream.result()).stopReason, 'error');
});

test('a terminal response with an unfinished streamed block is refused', async () => {
  const value = fixture(), stream = value.registry.streamSimple(model, context);
  await value.event('started');
  await value.event('text_start', { index: 0 });
  await value.event('text_delta', { index: 0, delta: 'advice' });
  value.settle(final());
  assert.equal((await stream.result()).stopReason, 'error');
});

test('a duplicate block ending cancels rather than accepting malformed native events', async () => {
  const value = fixture(), stream = value.registry.streamSimple(model, context);
  await value.event('started');
  await value.event('text_start', { index: 0 });
  await value.event('text_delta', { index: 0, delta: 'advice' });
  await value.event('text_end', { index: 0 });
  await assert.rejects(value.event('text_end', { index: 0 }), /part state/);
  assert.equal(value.frames[1].method, 'provider/infer/cancel');
  value.settle(final());
  assert.equal((await stream.result()).stopReason, 'error');
});

test('Runtime notification routing assembles native events synchronously before the terminal response', async () => {
  const { Runtime } = await import('../lib/runtime.mjs');
  const value = fixture(), stream = value.registry.streamSimple(model, context);
  const pending = [];
  for (const [kind, payload] of [['started', {}], ['text_start', { index: 0 }], ['text_delta', { index: 0, delta: 'advice' }], ['text_end', { index: 0 }]]) {
    pending.push(Runtime.prototype.notify.call(value.runtime, { method: 'provider/infer_event', params: { inference_id: value.frames[0].params.inference_id, sequence: pending.length, kind, payload } }));
  }
  assert.equal(stream.queue.length, 4);
  value.settle(final());
  await Promise.all(pending);
  assert.equal((await stream.result()).stopReason, 'stop');
});
