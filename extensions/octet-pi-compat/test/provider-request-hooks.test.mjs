import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
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
    const headers = await options.transformHeaders({});
    assert.equal(headers['x-host'], 'header-hook');
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

test('supplied fetch applies real wire headers without disclosing process credentials to hooks', async t => {
  const received = [];
  const privateHeaders = Object.fromEntries([
    'Authorization', 'Proxy-Authorization', 'Cookie', 'Set-Cookie', 'X-API-Key',
    'API-Key', 'X-Goog-API-Key', 'X-Auth-Token', 'X-Amz-Security-Token',
  ].map((name, index) => [name, `process-owned-canary-${index}`]));
  const server = createServer(async (request, response) => {
    let body = ''; for await (const chunk of request) body += chunk;
    received.push({ headers: request.headers, body: JSON.parse(body) });
    response.writeHead(201, { 'x-real-response': 'yes' }); response.end('local response');
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const value = await fixture((_, __, options) => (async function* () {
    const body = await options.onPayload({ actual: 'wire' });
    const response = await options.fetch(`http://127.0.0.1:${server.address().port}/`, {
      method: 'POST', signal: options.signal, body: JSON.stringify(body),
      headers: { ...privateHeaders, 'x-before': 'remove' },
    });
    await options.onResponse({ status: response.status, headers: Object.fromEntries(response.headers) });
    assert.equal(await response.text(), 'local response');
    yield { type: 'start', partial: { content: [] } };
    yield done();
  })(), (method, params) => {
    if (method.endsWith('/payload')) return { payload: { ...params.payload, edited: true } };
    if (method.endsWith('/headers')) {
      assert.deepEqual(params.headers, { 'x-before': 'remove' });
      return { headers: { 'x-host': ['yes', 'also'] } };
    }
    assert.equal(params.status, 201); assert.equal(params.headers['x-real-response'], 'yes');
    return { accepted: true };
  });
  await prepareProviderStream(value.runtime, value.params, value.store);
  startProviderStream(value.runtime, 'hook-stream');
  await until(() => value.frames.some(frame => ['finished', 'error'].includes(frame.params.kind)));
  assert.equal(value.frames.findLast(frame => frame.method === 'provider/event').params.kind, 'finished');
  assert.equal(received.length, 1);
  for (const [name, value] of Object.entries(privateHeaders)) {
    const header = name.toLowerCase();
    assert.deepEqual(received[0].headers[header], header === 'set-cookie' ? [value] : value);
  }
  assert.equal(received[0].headers['x-host'], 'yes, also');
  assert.equal(received[0].headers['x-before'], undefined);
  assert.deepEqual(received[0].body, { actual: 'wire', edited: true });
  assert.doesNotMatch(JSON.stringify(value.frames), /process-owned-canary-/);
  assert.deepEqual(value.frames.filter(frame => frame.method.startsWith('provider/hooks/')).map(frame => frame.method),
    ['provider/hooks/payload', 'provider/hooks/headers', 'provider/hooks/response']);
});

for (const name of ['Authorization', 'Proxy-Authorization', 'Cookie', 'Set-Cookie', 'X-API-Key', 'API-Key', 'X-Goog-API-Key', 'X-Auth-Token', 'X-Amz-Security-Token']) test(`bridge fetch rejects host-injected ${name} before HTTP dispatch`, async t => {
  const previous = globalThis.fetch;
  let dispatches = 0;
  globalThis.fetch = async () => { dispatches++; throw Error('unexpected dispatch'); };
  t.after(() => { globalThis.fetch = previous; });
  const value = await fixture((_, __, options) => (async function* () {
    await options.fetch('http://127.0.0.1:9/must-not-dispatch', { method: 'POST' });
    assert.fail('host credentials must not be admitted');
  })(), (method, params) => {
    assert.equal(method, 'provider/hooks/headers'); assert.deepEqual(params.headers, {});
    return { headers: { [name]: 'host-injection-canary' } };
  });
  await prepareProviderStream(value.runtime, value.params, value.store);
  startProviderStream(value.runtime, 'hook-stream');
  await until(() => value.frames.some(frame => frame.params.kind === 'error'));
  assert.equal(value.frames.some(frame => frame.params.kind === 'finished'), false);
  assert.equal(dispatches, 0);
  assert.doesNotMatch(JSON.stringify(value.frames), /host-injection-canary/);
});

for (const payload of [{ messages: [undefined] }, { value: NaN }, { value: 1n }, { value() {} }]) test('wire payload omission does not admit non-JSON values', async () => {
  const value = await fixture((_, __, options) => (async function* () {
    await options.transformHeaders({});
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
  const value = await fixture((_, __, options) => (async function* () {
    await options.transformHeaders({});
    assert.fail('cancelled job must not dispatch');
  })(), method => { assert.equal(method, 'provider/hooks/headers'); called(); return pending; });
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
    await options.transformHeaders({});
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
