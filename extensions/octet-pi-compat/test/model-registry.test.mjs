import test from 'node:test';
import assert from 'node:assert/strict';
import { modelRegistry, registerProvider, unregisterProvider } from '../lib/providers.mjs';
import { host } from './helper.mjs';

const native = host.model_view;
const extension = { ...native, provider: 'sol-advisor-mock', id: 'advisor-model', base_url: 'http://127.0.0.1:9' };
const unconfigured = { ...native, provider: 'unconfigured', id: 'known-model' };
function fixture(features = []) {
  const facts = { pi_models: { all_models: [native, extension, unconfigured], available_models: [native, extension],
    configured_auth: { [native.provider]: true, [extension.provider]: true, unconfigured: false } } };
  const calls = [], store = { controller: new AbortController() };
  const runtime = {
    features: new Set(features), current: () => store,
    require(feature) { if (!this.features.has(feature)) throw new Error(`unsupported_feature ${feature}`); },
    async hostCall(method, params, owner) { calls.push({ method, params, owner }); return runtime.reply; },
    reply: { ok: true, apiKey: 'explicit-fixture-key' },
  };
  return { facts, calls, store, runtime, registry: modelRegistry(() => facts, runtime) };
}

test('Pi registry separates full known catalog from authenticated availability, including extension models', () => {
  const { registry, facts } = fixture();
  assert.deepEqual(registry.getAll().map(model => model.provider), [native.provider, extension.provider, 'unconfigured']);
  assert.deepEqual(registry.getAvailable().map(model => model.provider), [native.provider, extension.provider]);
  assert.equal(registry.find(extension.provider, extension.id).baseUrl, extension.base_url);
  assert.equal(registry.find('unconfigured', unconfigured.id).id, unconfigured.id);
  assert.equal(registry.find('other', extension.id), undefined);
  assert.equal(registry.find(extension.provider, 'missing'), undefined);
  registry.find(extension.provider, extension.id).input.push('image');
  assert.deepEqual(facts.pi_models.all_models[1].input, native.input);
  facts.pi_models.all_models = [unconfigured];
  assert.equal(registry.find(extension.provider, extension.id), undefined);
  assert.equal(registry.getAll().length, 1);
});

test('Pi hasConfiguredAuth reports provider configuration without resolving credentials', () => {
  const { registry, calls } = fixture();
  assert.equal(registry.hasConfiguredAuth(native), true);
  assert.equal(registry.hasConfiguredAuth(extension), true);
  assert.equal(registry.hasConfiguredAuth(unconfigured), false);
  assert.equal(registry.hasConfiguredAuth({ provider: 'unknown', id: 'none' }), false);
  assert.deepEqual(calls, []);
});

test('getApiKeyForProvider returns undefined without a disclosure grant or configured provider', async () => {
  const { registry, calls } = fixture();
  assert.equal(await registry.getApiKeyForProvider(native.provider), undefined);
  assert.equal(await registry.getApiKeyForProvider('unknown'), undefined);
  assert.equal(await registry.getApiKeyForProvider('unconfigured'), undefined);
  assert.deepEqual(calls, []);
});

test('getApiKeyForProvider uses only the existing exact model credential broker when granted', async () => {
  const { registry, runtime, calls, store } = fixture(['provider_credentials']);
  assert.equal(await registry.getApiKeyForProvider(native.provider), 'explicit-fixture-key');
  assert.deepEqual(calls, [{ method: 'provider/credentials', params: { provider: native.provider, model: native.id }, owner: store }]);
  runtime.reply = { ok: true, headers: { 'x-example': 'header-only' } };
  assert.equal(await registry.getApiKeyForProvider(native.provider), undefined);
  runtime.reply = { ok: false, error: 'No API key found' };
  assert.equal(await registry.getApiKeyForProvider(native.provider), undefined);
  runtime.hostCall = async () => { throw new Error('private resolver failure'); };
  assert.equal(await registry.getApiKeyForProvider(native.provider), undefined);
});

function explicitFixture() {
  const value = fixture();
  const model = { id: 'own-model', name: 'Own Model', api: 'openai-completions', input: ['text'], reasoning: false,
    contextWindow: 8192, maxTokens: 1024, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } };
  registerProvider(value.runtime, 0, 'explicit-local', { api: model.api, baseUrl: 'https://extension.invalid/v1',
    apiKey: 'explicit-process-owned-key', models: [model], streamSimple() {} });
  value.runtime.initialized = true;
  return { ...value, model: { ...model, provider: 'explicit-local' } };
}

test('string registration resolves only its explicit process-owned key without native disclosure permission', async () => {
  const value = explicitFixture();
  assert.equal(await value.registry.getApiKeyForProvider('explicit-local'), 'explicit-process-owned-key');
  assert.deepEqual(await value.registry.getApiKeyAndHeaders({ ...value.model, headers: { 'x-forged': 'forged' } }),
    { ok: true, apiKey: 'explicit-process-owned-key', headers: {} });
  value.runtime.features.add('provider_credentials');
  assert.equal((await value.registry.getApiKeyAndHeaders({ ...value.model, id: 'not-declared' })).ok, false);
  assert.deepEqual(value.calls, []);
});

test('string registration credentials stop at aborted owners and unregister, never native auth fallback', async () => {
  const value = explicitFixture();
  value.store.controller.abort();
  assert.equal(await value.registry.getApiKeyForProvider('explicit-local'), undefined);
  await assert.rejects(value.registry.getApiKeyAndHeaders(value.model));
  assert.deepEqual(value.calls, []);
  value.runtime.initialized = false;
  unregisterProvider(value.runtime, 0, 'explicit-local');
  assert.equal(await value.registry.getApiKeyForProvider('explicit-local'), undefined);
  await assert.rejects(value.registry.getApiKeyAndHeaders(value.model), /provider_credentials/);
  assert.deepEqual(value.calls, []);
});

function customFixture(resolve = () => ({ auth: { apiKey: 'process-owned-key' } })) {
  const value = fixture(['provider_credentials']);
  const model = { id: 'own-model', name: 'Own Model', provider: 'custom-local', api: 'openai-completions', baseUrl: 'https://extension.invalid/v1', input: ['text'], reasoning: false,
    contextWindow: 8192, maxTokens: 1024, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    compat: { supportsStore: false }, thinkingLevelMap: { high: null }, headers: { 'x-model': 'model-value' } };
  registerProvider(value.runtime, 0, { id: model.provider, auth: { apiKey: { resolve } }, getModels: () => [model], stream() {}, streamSimple() {} });
  value.runtime.initialized = true;
  return { ...value, model };
}

test('registry custom provider snapshots retain full process-owned metadata without native transport disclosure', () => {
  const value = customFixture(() => { assert.fail('model inspection cannot resolve auth'); });
  const model = value.registry.find('custom-local', 'own-model');
  assert.deepEqual(model.compat, { supportsStore: false });
  assert.deepEqual(model.thinkingLevelMap, { high: null });
  model.compat.supportsStore = true;
  assert.equal(value.registry.find('custom-local', 'own-model').compat.supportsStore, false);
  assert.equal(value.registry.getAll().length, 4);
  assert.equal(value.registry.getAvailable().some(model => model.provider === 'custom-local'), false);
  const projection = { ...extension, provider: 'custom-local', id: 'own-model' };
  value.facts.pi_models.all_models.push(projection);
  value.facts.pi_models.available_models.push(projection);
  assert.equal(value.registry.getAll().length, 4);
  const available = value.registry.getAvailable().find(model => model.provider === 'custom-local');
  assert.deepEqual(available.compat, { supportsStore: false });
  available.compat.supportsStore = true;
  assert.equal(value.registry.getAvailable().find(model => model.provider === 'custom-local').compat.supportsStore, false);
  assert.deepEqual(value.calls, []);
});

test('registry custom provider authentication uses only its own resolver without native credential permission', async () => {
  const value = customFixture(() => ({ auth: { apiKey: 'process-owned-key', headers: { 'x-auth': 'auth-value', 'x-model': 'auth-value' } }, env: { LOCAL_FIXTURE: 'fixture' } }));
  value.runtime.features.clear();
  assert.equal(await value.registry.getApiKeyForProvider('custom-local'), 'process-owned-key');
  const auth = await value.registry.getApiKeyAndHeaders({ ...value.model, headers: { 'x-forged': 'not-admitted' } });
  assert.deepEqual(auth, { ok: true, apiKey: 'process-owned-key', headers: { 'x-auth': 'auth-value', 'x-model': 'model-value' }, env: { LOCAL_FIXTURE: 'fixture' } });
  assert.deepEqual(value.calls, []);
});

test('missing or failing custom authentication cannot fall through to a native credential broker', async () => {
  for (const resolve of [() => undefined, () => { throw new Error('private-fixture-error'); }]) {
    const value = customFixture(resolve);
    assert.equal(await value.registry.getApiKeyForProvider('custom-local'), undefined);
    const result = await value.registry.getApiKeyAndHeaders(value.model);
    assert.equal(result.ok, false);
    assert.doesNotMatch(JSON.stringify(result), /private-fixture-error/);
    assert.deepEqual(value.calls, []);
  }
});
