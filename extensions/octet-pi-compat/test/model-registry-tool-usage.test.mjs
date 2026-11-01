import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { modelRegistry } from '../lib/providers.mjs';
import { host } from './helper.mjs';

const owner = { session_id: 'usage-session', extension_instance_id: 'usage-instance', process_generation: 1 };
const terminal = () => ({ message: { content: [{ Text: 'advice' }], model: 'native-model', protocol: 'open_ai_chat' },
  usage: { input_tokens: 2, output_tokens: 3, cache_read_tokens: 0, cache_write_tokens: 0, cache_write_1h_tokens: 0, reasoning_tokens: 0, total_tokens: 5 },
  cost: { input: 2, output: 3, reasoning: 0, cache_read: 0, cache_write: 0, total: 5, total_picodollars_remainder: 123 },
  stop_reason: 'end_turn', timestamp: 42 });

function fixture(t, execute, nativeResult = terminal) {
  const replies = [], calls = [];
  let runtime;
  const transport = { childId: 0, closed: false,
    send: async frame => { replies.push(frame); }, settleParent() {}, cancel() {},
    notify: async () => {},
    async request(method, params) {
      this.childId++;
      calls.push({ method, params });
      assert.equal(method, 'provider/infer');
      const events = [['started', {}], ['text_start', { index: 0 }], ['text_delta', { index: 0, delta: 'advice' }], ['text_end', { index: 0 }]];
      for (const [sequence, [kind, payload]] of events.entries()) await runtime.receive({ jsonrpc: '2.0', method: 'provider/infer_event',
        params: { inference_id: params.inference_id, sequence, kind, payload } });
      return nativeResult();
    },
  };
  runtime = new Runtime({ extensions: [] }, transport);
  runtime.initialized = true;
  runtime.features.add('nested_inference_v1');
  const context = { resource_owner: owner, workspace: '/tmp', host: { ...host, has_ui: false } };
  runtime.bind({ context }, { id: 0, method: 'command/execute' });
  const registry = modelRegistry(() => ({ pi_models: { all_models: [host.model_view] } }), runtime);
  const consult = async () => registry.streamSimple(host.model_view, { messages: [{ role: 'user', content: 'question', timestamp: 1 }] }).result();
  runtime.tools.set('consult', { factory: 0, definition: { name: 'consult', description: 'Consult', parameters: { type: 'object' },
    execute: () => execute({ consult, runtime }) } });
  t.after(() => runtime.uninstallChildren());
  return { runtime, calls, async call(id = 7) {
    await runtime.receive({ jsonrpc: '2.0', id, method: 'tool/call', params: { name: 'consult', arguments: {}, context } });
    return replies.at(-1);
  } };
}

function response(usage) {
  return { content: [{ type: 'text', text: 'advice' }], details: { usage }, structuredContent: { usage }, usage };
}

test('real Runtime tool call accepts an exact settled registry usage echo without ToolOutput accounting', async t => {
  let reported;
  const value = fixture(t, async ({ consult }) => {
    const result = await consult();
    reported = structuredClone(result.usage);
    return response(reported);
  });
  const reply = await value.call();
  assert.equal(reply.error, undefined, JSON.stringify(reply));
  assert.equal(value.calls.length, 1);
  assert.equal(value.calls[0].params.parent_request_id, 7);
  assert.deepEqual(reply.result.metadata.pi_details.usage, reported);
  assert.deepEqual(reply.result.structured_content.usage, reported);
  assert.equal(Object.hasOwn(reply.result, 'usage'), false, 'native inference already owns the authoritative usage');
  assert.equal(Object.hasOwn(reply.result.metadata, 'pi_usage'), false, 'no ToolOutput usage mirror');
});

for (const [name, change] of [
  ['tokens', usage => { usage.input++; }],
  ['cost', usage => { usage.cost.total++; }],
  ['additional external usage', usage => { usage.output++; usage.totalTokens++; }],
  ['extra fields', usage => { usage.external = true; }],
]) test(`real Runtime refuses unmatched ${name}, including mutation of the delivered usage object`, async t => {
  const value = fixture(t, async ({ consult }) => {
    const result = await consult();
    change(result.usage);
    return response(result.usage);
  });
  const reply = await value.call();
  assert.match(reply.error?.message ?? '', /unsupported_feature.*usage/);
  assert.equal(reply.result, undefined);
  assert.equal(value.calls.length, 1);
});

test('unowned external usage cannot be silently stripped', async t => {
  const value = fixture(t, async () => response({ input: 2, output: 3, cacheRead: 0, cacheWrite: 0, totalTokens: 5 }));
  const reply = await value.call();
  assert.match(reply.error?.message ?? '', /unsupported_feature.*usage/);
  assert.equal(value.calls.length, 0);
});

test('an inference still in flight cannot authorize a fabricated terminal usage echo', async t => {
  let pending;
  const value = fixture(t, ({ consult }) => {
    pending = consult();
    return response({ input: 2, output: 3, cacheRead: 0, cacheWrite: 0, totalTokens: 5,
      cost: { input: 2 / 1e6, output: 3 / 1e6, cacheRead: 0, cacheWrite: 0, total: 5 / 1e6 + 123 / 1e12 } });
  });
  const reply = await value.call();
  assert.match(reply.error?.message ?? '', /unsupported_feature.*usage/);
  await pending;
});

test('a failed native attempt cannot authorize a fabricated usage echo', async t => {
  const value = fixture(t, async ({ consult }) => {
    assert.equal((await consult()).stopReason, 'error');
    return response({ input: 2, output: 3, cacheRead: 0, cacheWrite: 0, totalTokens: 5 });
  }, () => { throw Error('private provider failure'); });
  const reply = await value.call();
  assert.match(reply.error?.message ?? '', /unsupported_feature.*usage/);
  assert.doesNotMatch(JSON.stringify(reply), /private provider failure/);
});

test('an earlier invocation cannot authorize a cached echo even when request id and owner are reused', async t => {
  let cached;
  const value = fixture(t, async ({ consult }) => {
    if (cached) return response(cached);
    cached = structuredClone((await consult()).usage);
    return { content: [{ type: 'text', text: 'first' }] };
  });
  assert.equal((await value.call()).error, undefined);
  const reply = await value.call();
  assert.match(reply.error?.message ?? '', /unsupported_feature.*usage/);
  assert.equal(value.calls.length, 1);
});

for (const kind of ['retired', 'other foreground', 'cancelled', 'generation changed']) {
  test(`a ${kind} owner cannot authorize a settled usage echo`, async t => {
    const value = fixture(t, async ({ consult, runtime }) => {
      const usage = structuredClone((await consult()).usage);
      const store = runtime.scope.getStore();
      if (kind === 'retired') store.state.alive = false;
      if (kind === 'other foreground') runtime.foreground = { ...store.state };
      if (kind === 'cancelled') store.controller.abort();
      if (kind === 'generation changed') store.state.owner = { ...store.state.owner, process_generation: 2 };
      return response(usage);
    });
    const reply = await value.call();
    assert.equal(reply.result, undefined);
    assert.match(reply.error?.message ?? '', /unsupported_feature.*usage|not_foreground_owner|request cancelled/);
  });
}
