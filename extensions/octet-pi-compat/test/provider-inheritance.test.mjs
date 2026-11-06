import test from 'node:test';
import assert from 'node:assert/strict';
import { registerProvider, modelRegistry, startProviderRegistration, prepareProviderStream, startProviderStream, nativeProviderPayload, nativeProviderEvent } from '../lib/providers.mjs';
import * as ai from '../shims/ai.mjs';
import { nativeProviderModels } from '../lib/native-provider.mjs';

const native = { id: 'claude-test', name: 'Claude Test', api: 'anthropic-messages', provider: 'anthropic', reasoning: false, input: ['text'], contextWindow: 8192, maxTokens: 1024, cost: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0 } };
function fixture() {
  const frames = [], pending = [], store = { controller: new AbortController() };
  let complete;
  const done = new Promise(resolve => { complete = resolve; });
  const runtime = { initialized: false, features: new Set(['provider_proxy_v1', 'provider_inheritance_v1']),
    require(feature) { assert.ok(this.features.has(feature)); }, current: () => store,
    scope: { run: (_, fn) => fn() }, track(promise) { pending.push(promise); return promise; },
    transport: { async request(method, params) { frames.push({ method, params }); return method === 'providers/inherit' ? { models: [native] } : { revision: 1 }; }, async notify(method, params) { frames.push({ method, params }); if (method === 'providers/complete') complete(); } },
    backgroundError(error) { complete(); pending.push(Promise.reject(error)); },
  };
  return { runtime, frames, pending, store, done, registry: modelRegistry(() => ({}), runtime) };
}

test('partial registration inherits native models without resolving credentials or running its callback', async () => {
  const value = fixture();
  registerProvider(value.runtime, 0, 'anthropic', { api: 'anthropic-messages', streamSimple() { assert.fail('registration is not inference'); } });
  assert.deepEqual(value.frames, []);
  value.runtime.initialized = true;
  startProviderRegistration(value.runtime);
  await value.done;
  await Promise.all(value.pending);
  assert.deepEqual(value.frames.map(frame => frame.method), ['providers/inherit', 'providers/register', 'providers/complete']);
  const wire = value.frames[1].params;
  assert.equal(wire.models[0].api_name, native.id);
  assert.equal(wire.models[0].pi_metadata.inherit_provider, 'anthropic');
  assert.equal(wire.models[0].pi_metadata.pricing.input, 1e6);
  assert.doesNotMatch(JSON.stringify(value.frames), /apiKey|headers|authorization/i);
});

test('Pi re-registration merges defined values; undefined does not erase the prior stream or models', () => {
  const value = fixture(), streamSimple = () => {};
  const { provider, ...definition } = native;
  registerProvider(value.runtime, 0, 'anthropic', { api: 'anthropic-messages', models: [definition], streamSimple });
  registerProvider(value.runtime, 1, 'anthropic', { baseUrl: 'https://example.invalid', streamSimple: undefined });
  const config = value.registry.getRegisteredProviderConfig('anthropic');
  assert.equal(config.streamSimple, streamSimple);
  assert.deepEqual(config.models, [definition]);
  assert.equal(config.baseUrl, 'https://example.invalid');
  assert.equal(config.api, native.api);
});

test('native Anthropic delegate is callable but cannot run outside an admitted provider request', () => {
  assert.equal(typeof ai.anthropicMessagesApi, 'function');
  const api = ai.anthropicMessagesApi();
  assert.equal(typeof api.streamSimple, 'function');
  assert.throws(() => api.streamSimple(native, { messages: [] }), /provider.*(request|stream)/i);
});

async function admitted(callback, delegate) {
  const value = fixture();
  const original = value.runtime.transport.request;
  value.runtime.transport.request = async (method, params) => method === 'provider/delegate' ? delegate(params, value) : original(method, params);
  registerProvider(value.runtime, 0, 'anthropic', { api: 'anthropic-messages', streamSimple: callback });
  value.runtime.initialized = true;
  startProviderRegistration(value.runtime);
  await value.done;
  const request = { system: 'native system', messages: [{ User: { content: [{ Text: 'native input' }] } }], tools: [], tool_choice: 'auto', stop: [], output_format: { type: 'text' }, reasoning: { type: 'off' }, max_output_tokens: 32, cache_retention: 'short', session_id: null };
  await prepareProviderStream(value.runtime, { stream_id: 'stream-1', provider_id: 'anthropic', model_id: value.frames.find(frame => frame.method === 'providers/register').params.models[0].id, request }, value.store);
  let complete;
  const terminal = new Promise(resolve => { complete = resolve; });
  const notify = value.runtime.transport.notify;
  value.runtime.transport.notify = async (method, params) => { await notify(method, params); if (method === 'provider/event' && ['error', 'finished'].includes(params.kind)) complete(params); };
  startProviderStream(value.runtime, 'stream-1');
  return { ...value, terminal, request };
}
const nativeResponse = () => ({ message: { model: 'native-model-id', protocol: 'anthropic_messages', content: [{ Text: 'native response' }] }, usage: { input_tokens: 2, output_tokens: 3, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: 5 }, stop_reason: 'end_turn', timestamp: 123 });

test('admitted native delegation preserves tools, usage and route while composing onPayload', { timeout: 5000 }, async () => {
  let payloadCalls = 0, delegateCalls = 0;
  const value = await admitted((model, context, options) => ai.anthropicMessagesApi().streamSimple(model, { ...context, systemPrompt: 'wrapped system' }, { ...options, onPayload(payload, model) { payloadCalls++; assert.equal(model.id, native.id); payload.system = 'wrapped payload'; } }), async (params, fixture) => {
    delegateCalls++;
    assert.equal(params.stream_id, 'stream-1');
    assert.equal(params.on_payload, true);
    // Pi normalizes a wrapper's systemPrompt into a leading system message and
    // replays it before the original prompt (installed Pi 1.0.2 behaviour).
    assert.equal(params.request.system, 'wrapped system\n\nnative system');
    assert.deepEqual(params.request.tools, []);
    assert.equal(params.request.max_output_tokens, 32);
    assert.deepEqual(await nativeProviderPayload(fixture.runtime, { stream_id: params.stream_id, payload: { system: 'native payload', model: native.id } }), { payload: { system: 'wrapped payload', model: native.id } });
    return nativeResponse();
  });
  assert.equal((await value.terminal).kind, 'finished');
  assert.equal(payloadCalls, 1);
  assert.equal(delegateCalls, 1);
  const events = value.frames.filter(frame => frame.method === 'provider/event');
  assert.deepEqual(events.map(frame => frame.params.kind), ['started', 'text_start', 'text_delta', 'text_end', 'usage', 'finished']);
  assert.equal(events[2].params.payload.delta, 'native response');
  assert.equal(events[4].params.payload.total_tokens, 5);
});

for (const callback of ['onResponse', 'transformHeaders']) {
  test(`native delegation refuses unforwarded package ${callback} callbacks`, { timeout: 5000 }, async () => {
    let calls = 0;
    const value = await admitted((model, context, options) => ai.anthropicMessagesApi().streamSimple(model, context, { ...options, [callback]() { assert.fail('callback must not be accepted without wire forwarding'); } }), async () => { calls++; return nativeResponse(); });
    assert.equal((await value.terminal).kind, 'error');
    await Promise.all(value.pending);
    assert.equal(calls, 0);
  });
}

for (const mutation of ['tools', 'signal', 'tokens', 'reasoning', 'model', 'duplicate']) {
  test(`native delegation refuses ${mutation} authority changes before a new host attempt`, { timeout: 5000 }, async () => {
    let calls = 0;
    const value = await admitted((model, context, options) => {
      const api = ai.anthropicMessagesApi();
      if (mutation === 'tools') context = { ...context, tools: [{ name: 'extra', description: '', parameters: {} }] };
      if (mutation === 'signal') options = { ...options, signal: new AbortController().signal };
      if (mutation === 'tokens') options = { ...options, maxTokens: 33 };
      if (mutation === 'reasoning') options = { ...options, reasoning: 'high' };
      if (mutation === 'model') model = { ...model, id: 'other' };
      if (mutation === 'duplicate') api.streamSimple(model, context, options);
      return api.streamSimple(model, context, options);
    }, async () => { calls++; return nativeResponse(); });
    assert.equal((await value.terminal).kind, 'error');
    assert.equal(calls, mutation === 'duplicate' ? 1 : 0);
    assert.doesNotMatch(JSON.stringify(value.frames), /extra|other/);
  });
}

test('native transport errors become data-free stream errors', { timeout: 5000 }, async () => {
  const value = await admitted((...args) => ai.anthropicMessagesApi().streamSimple(...args), async () => { throw new Error('canary-secret-in-private-provider-error'); });
  assert.equal((await value.terminal).kind, 'error');
  assert.doesNotMatch(JSON.stringify(value.frames), /canary-secret/);
});

test('single native-provider object replaces regular registration and inherits only its stated native catalogue', async () => {
  const value = fixture();
  const facade = nativeProviderModels().getProvider('anthropic');
  const declared = facade.getModels()[0];
  const provider = { ...facade, name: 'Wrapped Anthropic',
    stream() { assert.fail('registration is not inference'); }, streamSimple() { assert.fail('registration is not inference'); } };
  const original = value.runtime.transport.request;
  value.runtime.transport.request = async (method, params) => method === 'providers/inherit'
    ? (value.frames.push({ method, params }), { models: [{ ...native, id: declared.id }, native] }) : original(method, params);
  registerProvider(value.runtime, 0, 'anthropic', { api: 'anthropic-messages', streamSimple() {} });
  registerProvider(value.runtime, 1, provider);
  assert.equal(provider.getModels, facade.getModels);
  assert.equal(value.registry.getRegisteredProviderConfig('anthropic'), undefined);
  assert.equal(value.registry.getRegisteredNativeProvider('anthropic'), provider);
  assert.deepEqual(value.registry.getRegisteredProviderIds(), ['anthropic']);
  assert.deepEqual(value.frames, []);
  value.runtime.initialized = true;
  startProviderRegistration(value.runtime);
  await value.done;
  await Promise.all(value.pending);
  assert.equal(value.frames[1].params.models.length, 1);
  assert.equal(value.frames[1].params.models[0].api_name, declared.id);
  assert.doesNotMatch(JSON.stringify(value.frames), /resolve|auth.*apiKey/);
});

test('native-provider facade supplies public inventory and host-only stream delegation without resolving credentials', async () => {
  const { nativeProviderModels } = await import('../lib/native-provider.mjs');
  const models = nativeProviderModels(), provider = models.getProvider('anthropic');
  assert.ok(provider.getModels().some(model => model.provider === 'anthropic'));
  assert.throws(() => provider.auth.apiKey.resolve(), /host-owned/);
  assert.throws(() => provider.streamSimple(provider.getModels()[0], { messages: [] }), /admitted/);
  models.setProvider({ ...provider, name: 'changed' });
  assert.equal(models.getProvider('anthropic').name, 'changed');
  assert.notEqual(nativeProviderModels().getProvider('anthropic').name, 'changed');
});


test('native deltas reach the admitted callback before the native terminal response is available', { timeout: 5000 }, async () => {
  let release, reached;
  const gate = new Promise(resolve => { release = resolve; });
  const early = new Promise(resolve => { reached = resolve; });
  const value = await admitted((...args) => ai.anthropicMessagesApi().streamSimple(...args), async (params, fixture) => {
    for (const [kind, payload] of [['started', {}], ['text_start', { index: 0 }], ['text_delta', { index: 0, delta: 'native response' }]]) {
      assert.deepEqual(nativeProviderEvent(fixture.runtime, { stream_id: params.stream_id, kind, payload }), { accepted: true });
    }
    reached();
    await gate;
    nativeProviderEvent(fixture.runtime, { stream_id: params.stream_id, kind: 'text_end', payload: { index: 0 } });
    return nativeResponse();
  });
  await early;
  for (let i = 0; i < 20 && !value.frames.some(frame => frame.params.kind === 'text_delta'); i++) await new Promise(resolve => setImmediate(resolve));
  assert.ok(value.frames.some(frame => frame.params.kind === 'text_delta'));
  assert.ok(!value.frames.some(frame => frame.params.kind === 'finished'));
  release();
  assert.equal((await value.terminal).kind, 'finished');
});

test('streamed native signed reasoning is projected unchanged at terminal without allowing forged signatures', { timeout: 5000 }, async () => {
  const value = await admitted((...args) => ai.anthropicMessagesApi().streamSimple(...args), async (params, fixture) => {
    for (const [kind, payload] of [['started', {}], ['reasoning_start', { index: 0 }], ['reasoning_delta', { index: 0, delta: 'native thought' }], ['reasoning_end', { index: 0 }]]) {
      nativeProviderEvent(fixture.runtime, { stream_id: params.stream_id, kind, payload });
    }
    return { ...nativeResponse(), message: { model: 'native-model-id', protocol: 'anthropic_messages', content: [{ Reasoning: { text: 'native thought', state: { model: 'native-model-id', protocol: 'anthropic_messages', kind: { AnthropicSignature: { signature: 'native-opaque-signature' } } } } }] } };
  });
  assert.equal((await value.terminal).kind, 'finished');
  assert.ok(value.frames.some(frame => frame.params.kind === 'reasoning_delta'));
  assert.doesNotMatch(JSON.stringify(value.frames), /native-opaque-signature/);
});
