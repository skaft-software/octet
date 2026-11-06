import test from 'node:test';
import assert from 'node:assert/strict';
import { registerProvider, startProviderRegistration, prepareProviderStream, startProviderStream, cancelProviderStream } from '../lib/providers.mjs';

async function fixture(stream, hook) {
  const frames = [], store = { controller: new AbortController() };
  let registered;
  const registration = new Promise(resolve => { registered = resolve; });
  const runtime = {
    initialized: false, stopping: false, features: new Set(['provider_proxy_v1', 'provider_request_hooks_v1']),
    require(feature) { assert.ok(this.features.has(feature), feature); }, current: () => store,
    track: promise => promise, scope: { run: (_, callback) => callback() },
    backgroundError: error => { throw error; },
    transport: {
      request: async (method, params) => { frames.push({ method, params }); return method.startsWith('provider/hooks/') ? hook(method, params) : { revision: 1 }; },
      notify: async (method, params) => { frames.push({ method, params }); if (method === 'providers/complete') registered(); },
    },
  };
  registerProvider(runtime, 0, 'hook-provider', {
    baseUrl: 'http://127.0.0.1:9/local/', apiKey: 'explicit-local-dummy', api: 'openai-completions',
    models: [{ id: 'local', name: 'Local', reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 8192, maxTokens: 1024 }], streamSimple: stream,
  });
  runtime.initialized = true; startProviderRegistration(runtime); await registration;
  const params = { stream_id: 'hook-stream', provider_id: 'hook-provider', model_id: frames[0].params.models[0].id, request_hooks: true,
    request: { system: 'canonical system', messages: [{ User: { content: [{ Text: 'canonical input' }] } }], tools: [], tool_choice: 'auto', stop: [], output_format: { type: 'text' }, reasoning: { type: 'off' }, max_output_tokens: 32, cache_retention: 'short', session_id: null } };
  return { runtime, frames, store, params };
}

const done = () => ({ type: 'done', reason: 'stop', message: { content: [], usage: { input: 1, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 1 } } });
async function until(predicate) {
  for (let i = 0; i < 1400; i++) { if (predicate()) return; await new Promise(resolve => setTimeout(resolve, 5)); }
  assert.fail('bounded fixture timeout');
}

test('wire callbacks carry the real payload and response and the transformed headers before dispatch', async () => {
  let observed;
  const value = await fixture((_, context, options) => (async function* () {
    assert.equal(context.messages[0].role, 'system');
    assert.equal(options.headers['x-host'], 'header-hook');
    assert.equal(Object.hasOwn(options, 'transformHeaders'), false);
    observed = await options.onPayload({ model: 'actual-wire-model', messages: [{ role: 'user', content: 'actual-wire-input', optional: undefined }], prompt_cache_key: undefined, prompt_cache_retention: undefined });
    await options.onResponse({ status: 201, headers: { 'x-upstream': 'real-response' } });
    yield { type: 'start', partial: { content: [] } };
    yield done();
  })(), (method, params) => {
    assert.equal(params.stream_id, 'hook-stream');
    if (method.endsWith('/headers')) { assert.deepEqual(params.headers, {}); return { headers: { 'x-host': 'header-hook' } }; }
    if (method.endsWith('/payload')) {
      assert.deepEqual(params.payload, { model: 'actual-wire-model', messages: [{ role: 'user', content: 'actual-wire-input' }] });
      return { payload: { ...params.payload, hook: 'payload-hook' } };
    }
    assert.deepEqual(params, { stream_id: 'hook-stream', status: 201, headers: { 'x-upstream': 'real-response' } });
    return { accepted: true };
  });
  await prepareProviderStream(value.runtime, value.params, value.store);
  assert.equal(value.frames.some(frame => frame.method.startsWith('provider/hooks/')), false);
  startProviderStream(value.runtime, 'hook-stream');
  await until(() => value.frames.some(frame => frame.params.kind === 'finished'));
  assert.equal(observed.hook, 'payload-hook');
  assert.deepEqual(value.frames.filter(frame => frame.method.startsWith('provider/hooks/')).map(frame => frame.method), ['provider/hooks/headers', 'provider/hooks/payload', 'provider/hooks/response']);
});

for (const payload of [{ messages: [undefined] }, { value: NaN }, { value: 1n }, { value() {} }]) test('wire payload omission does not admit non-JSON values', async () => {
  const value = await fixture((_, __, options) => (async function* () {
    await options.onPayload(payload);
    assert.fail('invalid wire payload must not dispatch');
  })(), (method, params) => {
    assert.equal(method, 'provider/hooks/headers');
    return { headers: params.headers };
  });
  await prepareProviderStream(value.runtime, value.params, value.store);
  startProviderStream(value.runtime, 'hook-stream');
  await until(() => value.frames.some(frame => frame.params.kind === 'error'));
  assert.equal(value.frames.some(frame => frame.method === 'provider/hooks/payload'), false);
  assert.equal(value.frames.some(frame => frame.params.kind === 'finished'), false);
});

test('wire callbacks require the independently negotiated host feature', async () => {
  const value = await fixture(() => assert.fail('must not dispatch'), () => assert.fail('must not call host hooks'));
  value.runtime.features.delete('provider_request_hooks_v1');
  await assert.rejects(prepareProviderStream(value.runtime, value.params, value.store), /provider_request_hooks_v1/);
  await assert.rejects(prepareProviderStream(value.runtime, { ...value.params, request_hooks: false }, value.store), /provider request hooks/);
});

test('cancelling while a header callback is pending prevents dispatch and ignores its late reply', async () => {
  let reply, called;
  const pending = new Promise(resolve => { reply = resolve; });
  const started = new Promise(resolve => { called = resolve; });
  const value = await fixture(() => assert.fail('cancelled job must not dispatch'), method => { assert.equal(method, 'provider/hooks/headers'); called(); return pending; });
  await prepareProviderStream(value.runtime, value.params, value.store);
  startProviderStream(value.runtime, 'hook-stream');
  await started;
  cancelProviderStream(value.runtime, 'hook-stream');
  reply({ headers: { 'x-late': 'discard' } });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(value.frames.some(frame => frame.method === 'provider/event'), false);
});

for (const phase of ['headers', 'payload', 'response']) test(`a ${phase} hook refusal terminates without a successful stream`, async () => {
  const value = await fixture((_, __, options) => (async function* () {
    await options.onPayload({ model: 'wire' });
    await options.onResponse({ status: 200, headers: {} });
    yield { type: 'start', partial: { content: [] } };
    yield done();
  })(), (method, params) => {
    if (method.endsWith(`/${phase}`)) throw new Error('local hook refused');
    if (method.endsWith('/headers')) return { headers: params.headers };
    if (method.endsWith('/payload')) return { payload: params.payload };
    return { accepted: true };
  });
  await prepareProviderStream(value.runtime, value.params, value.store); startProviderStream(value.runtime, 'hook-stream');
  await until(() => value.frames.some(frame => frame.params.kind === 'error'));
  assert.equal(value.frames.some(frame => frame.params.kind === 'finished'), false);
});
