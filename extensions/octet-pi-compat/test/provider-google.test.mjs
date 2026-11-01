import test from 'node:test';
import assert from 'node:assert/strict';
import { nativeProviderModels } from '../lib/native-provider.mjs';
import { registerProvider, validateProviderInitialization, startProviderRegistration, prepareProviderStream, startProviderStream, cancelProviderStream } from '../lib/providers.mjs';

const model = { id: 'gemini-local', name: 'Local Google', provider: 'google', api: 'google-generative-ai', reasoning: false, input: ['text'], contextWindow: 8192, maxTokens: 1024, cost: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0 } };
function fixture(features = ['provider_proxy_v1', 'provider_inheritance_v1', 'provider_google_generative_ai_v1']) {
  const frames = [], pending = [], store = { controller: new AbortController() };
  let complete, terminal;
  const done = new Promise(resolve => { complete = resolve; });
  const ended = new Promise(resolve => { terminal = resolve; });
  const runtime = { initialized: false, features: new Set(features), current: () => store,
    require(feature) { assert.ok(this.features.has(feature), `missing ${feature}`); },
    scope: { run: (_, work) => work() }, track(work) { pending.push(work); return work; }, backgroundError(error) { complete(error); },
    transport: {
      async request(method, params) { frames.push({ method, params }); return method === 'providers/inherit' ? { models: [model] } : { revision: 1 }; },
      async notify(method, params) { frames.push({ method, params }); if (method === 'providers/complete') complete(); if (method === 'provider/event' && ['error', 'finished'].includes(params.kind)) terminal(params); },
    },
  };
  return { runtime, frames, pending, store, done, ended };
}
async function registered(callback) {
  const value = fixture();
  registerProvider(value.runtime, 0, 'google', { api: model.api, streamSimple: callback });
  validateProviderInitialization(value.runtime);
  value.runtime.initialized = true;
  startProviderRegistration(value.runtime);
  const error = await value.done;
  if (error) throw error;
  return value;
}
const request = () => ({ system: 'google system', messages: [{ User: { content: [{ Text: 'input' }] } }], tools: [], tool_choice: 'auto', stop: [], output_format: { type: 'text' }, reasoning: { type: 'off' }, max_output_tokens: 32, cache_retention: 'short', session_id: null });
async function prepare(value) {
  const wire = value.frames.find(frame => frame.method === 'providers/register').params.models[0];
  await prepareProviderStream(value.runtime, { stream_id: 'google-1', provider_id: 'google', model_id: wire.id, request: request() }, value.store);
}

test('Google inheritance maps only its actual native dialect and never serializes credentials', async () => {
  const value = await registered(() => assert.fail('registration must not infer'));
  const wire = value.frames.find(frame => frame.method === 'providers/register').params;
  assert.equal(wire.models[0].protocol, 'google_generative_ai');
  assert.equal(wire.models[0].api_name, model.id);
  assert.equal(wire.models[0].pi_metadata.inherit_provider, 'google');
  assert.deepEqual(wire.provider.auth, { kind: 'none' });
  assert.doesNotMatch(JSON.stringify(value.frames), /apiKey|authorization|headers/);
});

test('Kernel-style Google wrapper preserves its native facade without running its replaced auth resolver', async () => {
  const value = fixture(), base = nativeProviderModels().getProvider('google');
  const nativeId = base.getModels()[0].id;
  const request = value.runtime.transport.request;
  value.runtime.transport.request = async (method, params) => method === 'providers/inherit'
    ? (value.frames.push({ method, params }), { models: [{ ...model, id: nativeId }] }) : request(method, params);
  registerProvider(value.runtime, 0, { ...base,
    auth: { ...base.auth, apiKey: { resolve() { assert.fail('registration cannot resolve auth'); } } },
    stream: (...args) => base.stream(...args), streamSimple: (...args) => base.streamSimple(...args),
  });
  validateProviderInitialization(value.runtime);
  value.runtime.initialized = true;
  startProviderRegistration(value.runtime);
  const error = await value.done;
  if (error) throw error;
  const wire = value.frames.find(frame => frame.method === 'providers/register').params.models;
  assert.equal(wire.length, 1);
  assert.equal(wire[0].api_name, nativeId);
  assert.equal(wire[0].protocol, 'google_generative_ai');
  assert.equal(wire[0].pi_metadata.inherit_provider, 'google');
});

for (const form of ['string', 'native-object']) test(`Google ${form} registration requires its additional negotiated grant before RPC`, () => {
  const value = fixture(['provider_proxy_v1', 'provider_inheritance_v1']);
  if (form === 'string') registerProvider(value.runtime, 0, 'google', { api: model.api, streamSimple() {} });
  else registerProvider(value.runtime, 0, { ...nativeProviderModels().getProvider('google'), stream() {}, streamSimple() {} });
  assert.throws(() => validateProviderInitialization(value.runtime), /provider_google_generative_ai_v1/);
  assert.deepEqual(value.frames, []);
});

test('unknown dialect and non-inherited Google models remain refused', () => {
  const value = fixture();
  assert.throws(() => registerProvider(value.runtime, 0, 'google', { api: 'google-interactions', streamSimple() {} }), /provider API/);
  const { provider, ...definition } = model;
  assert.throws(() => registerProvider(value.runtime, 0, 'google', { api: model.api, models: [definition], streamSimple() {} }), /native|inherited/);
  assert.deepEqual(value.frames, []);
});

for (const mutation of [null, 'model', 'signal', 'tools', 'duplicate']) test(`admitted Google native delegation ${mutation ?? 'preserves routing and usage'}`, { timeout: 5000 }, async () => {
  const facade = nativeProviderModels().getProvider('google');
  let calls = 0;
  const value = await registered((selected, context, options) => {
    assert.equal(selected.api, model.api);
    assert.equal(options.apiKey, undefined);
    if (mutation === 'model') selected = { ...selected, api: 'openai-completions' };
    if (mutation === 'signal') options = { ...options, signal: new AbortController().signal };
    if (mutation === 'tools') context = { ...context, tools: [{ name: 'extra' }] };
    if (mutation === 'duplicate') facade.streamSimple(selected, context, options);
    return facade.streamSimple(selected, context, options);
  });
  const original = value.runtime.transport.request;
  value.runtime.transport.request = async (method, params) => {
    if (method !== 'provider/delegate') return original(method, params);
    calls++;
    assert.equal(params.stream_id, 'google-1');
    assert.deepEqual(params.request, request());
    return { message: { model: 'native-google-id', protocol: 'google_generative_ai', content: [{ Text: 'Google native response' }] }, usage: { input_tokens: 2, output_tokens: 3, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: 5 }, stop_reason: 'end_turn', timestamp: 123 };
  };
  await prepare(value);
  startProviderStream(value.runtime, 'google-1');
  assert.equal((await value.ended).kind, mutation ? 'error' : 'finished');
  assert.equal(calls, mutation && mutation !== 'duplicate' ? 0 : 1);
  if (!mutation) {
    const events = value.frames.filter(frame => frame.method === 'provider/event');
    assert.equal(events.find(frame => frame.params.kind === 'text_delta').params.payload.delta, 'Google native response');
    assert.equal(events.find(frame => frame.params.kind === 'usage').params.payload.total_tokens, 5);
  }
});

test('Google cancellation aborts the original admitted native callback signal', { timeout: 5000 }, async () => {
  let signal;
  const value = await registered((_, __, options) => { signal = options.signal; return { [Symbol.asyncIterator]() { return this; }, next: () => new Promise(() => {}) }; });
  await prepare(value);
  startProviderStream(value.runtime, 'google-1');
  assert.equal(signal.aborted, false);
  cancelProviderStream(value.runtime, 'google-1');
  assert.equal(signal.aborted, true);
});
