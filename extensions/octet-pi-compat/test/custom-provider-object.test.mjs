import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { registerProvider, unregisterProvider, checkpointFactoryProviders, validateProviderInitialization, startProviderRegistration, prepareProviderStream, startProviderStream, cancelProviderStream, modelRegistry } from '../lib/providers.mjs';
import { customProviderModels, getCustomProviderApiKey, getCustomProviderAuth, hasCustomProvider, providerState } from '../lib/custom-provider.mjs';
import { nativeProviderDelegate, nativeProviderModels, isNativeProviderFacade } from '../lib/native-provider.mjs';
import { envApiKeyAuth } from '../shims/pi-1.0.2/ai/auth.mjs';
import * as ai from '../shims/ai.mjs';

const row = (id = 'own/model') => ({ id, name: 'Own Model', provider: 'coralbricks', api: 'openai-completions', baseUrl: 'https://custom.invalid/v1', headers: undefined,
  reasoning: true, thinkingLevelMap: { high: 'high', xhigh: null }, compat: { supportsStore: false, maxTokensField: 'max_tokens' }, input: ['text', 'image'],
  contextWindow: 8192, maxTokens: 1024, cost: { input: 0.19, output: 0.6, cacheRead: 0.008, cacheWrite: 0.19 } });
const provider = (overrides = {}) => ({ id: 'coralbricks', name: 'CoralBricks', baseUrl: 'https://custom.invalid/v1', models: [row()],
  auth: { apiKey: { resolve() { return { auth: { apiKey: 'extension-owned-key' }, source: 'extension' }; } } },
  getModels() { return this.models; }, stream() { assert.fail('full stream is not invoked'); }, streamSimple() { assert.fail('registration is not inference'); }, ...overrides });
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const tick = () => new Promise(resolve => setImmediate(resolve));
function fixture() {
  const frames = [], pending = [], errors = [], store = { controller: new AbortController(), state: {} };
  const runtime = { initialized: false, stopping: false, features: new Set(['provider_proxy_v1']),
    require(feature) { if (!this.features.has(feature)) throw new Error(`unsupported_feature ${feature}`); },
    current() { assert.ok(this.initialized, 'admitted invocation only'); return store; }, scope: { run: (_, work) => work() },
    hostCall() { assert.fail('native credentials are never consulted for a custom object'); },
    track(promise) { pending.push(promise); promise.catch(() => {}); return promise; }, backgroundError(error) { errors.push(error); },
    transport: { async request(method, params) { frames.push({ method, params: structuredClone(params) }); assert.notEqual(method, 'providers/inherit'); assert.notEqual(method, 'provider/delegate'); return { revision: 1 }; },
      async notify(method, params) { frames.push({ method, params: structuredClone(params) }); } },
  };
  const registry = modelRegistry(() => ({ pi_models: { all_models: [], available_models: [] } }), runtime);
  async function flush() { for (let i = 0; i < 4; i++) { await providerState(runtime).tail; await tick(); } }
  async function initialize() { validateProviderInitialization(runtime); runtime.initialized = true; startProviderRegistration(runtime); await flush(); }
  return { runtime, frames, pending, errors, store, registry, flush, initialize };
}
const request = () => ({ system: 'native system', messages: [], tools: [], tool_choice: 'auto', stop: [], output_format: { type: 'text' }, reasoning: { type: 'off' }, max_output_tokens: 32, cache_retention: 'short', session_id: null });
async function prepare(value, id = 'stream-1') {
  const model = providerState(value.runtime).committed.get('coralbricks').models[0];
  await prepareProviderStream(value.runtime, { stream_id: id, provider_id: 'coralbricks', model_id: model.wire.id, request: request() }, value.store);
}
function emptyStream() {
  return (async function* () {
    yield { type: 'start', partial: { content: [] } };
    yield { type: 'done', reason: 'stop', message: { content: [], usage: { input: 1, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 1 } } };
  })();
}

test('custom object registration captures its synchronous full catalog, not host inheritance, without effects', async () => {
  const value = fixture();
  let reads = 0, auth = 0, refresh = 0;
  const object = provider({ getModels() { reads++; assert.equal(this, object); return this.models; },
    auth: { apiKey: { resolve() { auth++; assert.fail('inspection cannot resolve auth'); } } },
    refreshModels(context) { refresh++; assert.equal(this, object); assert.equal(context.allowNetwork, false); assert.equal(context.credential, undefined); assert.equal(context.stored, undefined); assert.ok(context.signal instanceof AbortSignal); } });
  object.models[0].headers = { 'x-extension': 'private-header-canary' };
  registerProvider(value.runtime, 0, object);
  assert.equal(reads, 1); assert.equal(auth, 0); assert.equal(refresh, 0);
  assert.equal(hasCustomProvider(value.runtime, 'coralbricks'), true);
  assert.deepEqual(customProviderModels(value.runtime), object.models);
  const copy = customProviderModels(value.runtime); copy[0].compat.supportsStore = true;
  object.models[0].name = 'not published';
  assert.equal(customProviderModels(value.runtime)[0].name, 'Own Model');
  assert.equal(customProviderModels(value.runtime)[0].compat.supportsStore, false);
  assert.equal(value.registry.getRegisteredNativeProvider('coralbricks'), object);
  assert.deepEqual(value.frames, []);
  await value.initialize();
  assert.equal(auth, 0); assert.equal(refresh, 1); assert.equal(reads, 1);
  const registration = value.frames.find(frame => frame.method === 'providers/register');
  assert.equal(registration.params.models[0].api_name, 'own/model');
  assert.equal(registration.params.models[0].pi_metadata.base_url, 'https://custom.invalid/v1');
  assert.equal(registration.params.models[0].pi_metadata.pricing.input, 190000);
  assert.doesNotMatch(JSON.stringify(value.frames), /private-header-canary|supportsStore|thinkingLevelMap|resolve|inherit_provider/);
});

test('undefined metadata stays process-local and per-model endpoint/API are retained', async () => {
  const value = fixture(), object = provider();
  object.models.push({ ...row('second'), api: 'openai-responses', baseUrl: 'https://second.invalid/v1' });
  registerProvider(value.runtime, 0, object);
  assert.ok(Object.hasOwn(customProviderModels(value.runtime)[0], 'headers'));
  assert.equal(customProviderModels(value.runtime)[0].headers, undefined);
  await value.initialize();
  const models = value.frames[0].params.models;
  assert.deepEqual(models.map(model => [model.protocol, model.pi_metadata.base_url]), [['openai_chat', 'https://custom.invalid/v1'], ['openai_responses', 'https://second.invalid/v1']]);
});

test('matching a builtin provider ID alone cannot grant host inheritance or credentials', async () => {
  const value = fixture(), object = provider({ id: 'anthropic', models: [{ ...row(), provider: 'anthropic' }] });
  registerProvider(value.runtime, 0, object);
  assert.equal(isNativeProviderFacade(object), false);
  await value.initialize();
  assert.deepEqual(value.frames.map(frame => frame.method), ['providers/register', 'providers/complete']);
  assert.equal(value.frames[0].params.models[0].pi_metadata.inherit_provider, undefined);
});

test('object registrations replace rather than merge configuration, callbacks, models or auth', async () => {
  const value = fixture(), first = provider(), second = provider({ models: [row('replacement')], auth: {} });
  registerProvider(value.runtime, 0, 'coralbricks', { api: 'openai-completions', baseUrl: 'https://old.invalid', apiKey: 'old-key', models: [Object.fromEntries(Object.entries(row()).filter(([key]) => !['provider', 'baseUrl', 'headers', 'compat', 'thinkingLevelMap'].includes(key)))], streamSimple: emptyStream });
  registerProvider(value.runtime, 0, first);
  registerProvider(value.runtime, 1, second);
  assert.equal(value.registry.getRegisteredProviderConfig('coralbricks'), undefined);
  assert.equal(value.registry.getRegisteredNativeProvider('coralbricks'), second);
  assert.deepEqual(customProviderModels(value.runtime).map(model => model.id), ['replacement']);
  await value.initialize();
  assert.equal(await getCustomProviderApiKey(value.runtime, 'coralbricks'), undefined);
  assert.doesNotMatch(JSON.stringify(value.frames), /old-key|old.invalid|own\/model/);
});

test('admitted custom stream keeps callback binding, metadata and extension-owned resolved auth', async () => {
  const value = fixture(), observed = deferred();
  let authCalls = 0;
  const auth = { resolve({ credential, signal }) { assert.equal(this, auth); assert.equal(credential, undefined); assert.equal(signal.aborted, false); authCalls++; return { auth: { apiKey: 'owned-stream-key', headers: { 'x-resolved': 'yes' }, baseUrl: 'https://auth-owned.invalid/v1' }, env: { OWN_INPUT: 'yes' } }; } };
  const object = provider({ auth: { apiKey: auth }, streamSimple(model, context, options) { assert.equal(this, object); observed.resolve({ model, context, options: { ...options } }); return emptyStream(); } });
  registerProvider(value.runtime, 0, object); await value.initialize();
  await prepare(value); assert.equal(authCalls, 0);
  startProviderStream(value.runtime, 'stream-1');
  const got = await observed.promise;
  assert.equal(authCalls, 1); assert.equal(got.model.id, 'own/model');
  assert.deepEqual(got.model.compat, row().compat); assert.deepEqual(got.model.thinkingLevelMap, row().thinkingLevelMap);
  assert.equal(got.model.baseUrl, 'https://auth-owned.invalid/v1'); assert.equal(got.options.apiKey, 'owned-stream-key');
  assert.deepEqual(got.options.headers, { 'x-resolved': 'yes' }); assert.deepEqual(got.options.env, { OWN_INPUT: 'yes' });
  await value.flush(); assert.ok(value.frames.some(frame => frame.params.kind === 'finished'));
  assert.doesNotMatch(JSON.stringify(value.frames), /owned-stream-key|x-resolved|OWN_INPUT|auth-owned/);
});

test('unconfigured custom auth cannot silently fall back to a native credential or invoke a stream', async () => {
  const value = fixture();
  registerProvider(value.runtime, 0, provider({ auth: { apiKey: { resolve() { return undefined; } } } }));
  await value.initialize(); await prepare(value); startProviderStream(value.runtime, 'stream-1'); await value.flush();
  assert.ok(value.frames.some(frame => frame.params.kind === 'error'));
});

test('custom objects cannot use guarded native delegation or host OpenAI HTTP helpers', async () => {
  const value = fixture();
  value.runtime.features.add('provider_inheritance_v1');
  const object = provider({ id: 'anthropic', models: [{ ...row(), provider: 'anthropic' }], streamSimple: nativeProviderDelegate });
  registerProvider(value.runtime, 0, object); await value.initialize();
  const model = providerState(value.runtime).committed.get('anthropic').models[0];
  await prepareProviderStream(value.runtime, { stream_id: 'stream-1', provider_id: 'anthropic', model_id: model.wire.id, request: request() }, value.store);
  startProviderStream(value.runtime, 'stream-1'); await value.flush();
  assert.ok(value.frames.some(frame => frame.params.kind === 'error'));
  assert.equal(value.frames.some(frame => frame.method === 'provider/delegate'), false);
  assert.throws(() => ai.streamOpenAICompletions(row(), { messages: [] }), /unsupported/);
});

test('registry auth helper is invocation-only and supplies explicit/process-local inputs, never host stores', async t => {
  const value = fixture();
  const previous = process.env.OCTET_CUSTOM_PROVIDER_TEST_KEY;
  t.after(() => { if (previous === undefined) delete process.env.OCTET_CUSTOM_PROVIDER_TEST_KEY; else process.env.OCTET_CUSTOM_PROVIDER_TEST_KEY = previous; });
  process.env.OCTET_CUSTOM_PROVIDER_TEST_KEY = 'process-owned-key';
  registerProvider(value.runtime, 0, provider({ auth: { apiKey: envApiKeyAuth('test', ['OCTET_CUSTOM_PROVIDER_TEST_KEY']) } }));
  await assert.rejects(getCustomProviderApiKey(value.runtime, 'coralbricks'), /admitted/);
  await value.initialize();
  assert.equal(await getCustomProviderApiKey(value.runtime, 'coralbricks'), 'process-owned-key');
  assert.equal(await getCustomProviderApiKey(value.runtime, 'coralbricks', { env: { OCTET_CUSTOM_PROVIDER_TEST_KEY: 'explicit-env-key' } }), 'explicit-env-key');
  assert.equal(await getCustomProviderApiKey(value.runtime, 'coralbricks', { apiKey: 'explicit-request-key' }), 'explicit-request-key');
  assert.equal(await getCustomProviderApiKey(value.runtime, 'missing'), undefined);
  value.runtime.stopping = true;
  await assert.rejects(getCustomProviderApiKey(value.runtime, 'coralbricks'), /admitted/);
});

test('auth helper refuses unavailable file authority and interrupts uncooperative resolution on removal', async () => {
  const value = fixture();
  registerProvider(value.runtime, 0, provider({ auth: { apiKey: { resolve({ ctx }) { return ctx.fileExists('auth.json'); } } } }));
  await value.initialize();
  await assert.rejects(getCustomProviderAuth(value.runtime, 'coralbricks'), /only explicit and process-local/);
  const entered = deferred();
  registerProvider(value.runtime, 0, provider({ auth: { apiKey: { resolve({ signal }) { entered.resolve(signal); return new Promise(() => {}); } } } }));
  await value.flush();
  const resolving = getCustomProviderApiKey(value.runtime, 'coralbricks');
  const signal = await entered.promise;
  unregisterProvider(value.runtime, 0, 'coralbricks');
  await assert.rejects(resolving, /abort/i); assert.equal(signal.aborted, true); await value.flush();
});

test('offline refresh serializes publish updates, updates snapshots after host ACK, and reports booleans', async () => {
  const value = fixture(), refreshed = deferred(), publications = [];
  const object = provider({ async refreshModels(context) {
    assert.equal(context.allowNetwork, false);
    const a = context.publish({ update: () => { publications.push('a'); this.models = [row('a')]; } });
    const b = context.publish({ update: () => { publications.push('b'); this.models = [row('b')]; } });
    refreshed.resolve(await Promise.all([a, b]));
  } });
  registerProvider(value.runtime, 0, object); await value.initialize();
  assert.deepEqual(await refreshed.promise, [true, true]); assert.deepEqual(publications, ['a', 'b']);
  assert.deepEqual(value.frames.filter(frame => frame.method === 'providers/update').map(frame => frame.params.models[0].api_name), ['a', 'b']);
  assert.equal(customProviderModels(value.runtime)[0].id, 'b');
  assert.equal(providerState(value.runtime).committed.get('coralbricks').models[0].pi.id, 'b');
});

test('unsupported model persistence does not report success or execute publication update', async () => {
  const value = fixture(), results = deferred();
  registerProvider(value.runtime, 0, provider({ async refreshModels({ publish }) {
    const a = await publish({ persist: { models: [row()], checkedAt: 1 }, update() { assert.fail('unsupported persistence is not applied'); } });
    const b = await publish({ persist: null, update() { assert.fail('unsupported deletion is not applied'); } });
    results.resolve([a, b]);
  } }));
  await value.initialize(); assert.deepEqual(await results.promise, [false, false]);
  assert.equal(value.frames.some(frame => frame.method === 'providers/update'), false);
});

for (const operation of ['replace', 'remove']) test(`${operation} aborts refresh and accepted jobs; late publication is generation-fenced`, async () => {
  const value = fixture(); let context, signal;
  registerProvider(value.runtime, 0, provider({ refreshModels(input) { context = input; }, streamSimple(_, __, options) {
    signal = options.signal; return { [Symbol.asyncIterator]() { return this; }, next() { return new Promise(() => {}); } };
  } }));
  await value.initialize(); await prepare(value); startProviderStream(value.runtime, 'stream-1'); await tick();
  assert.equal(signal.aborted, false);
  if (operation === 'replace') registerProvider(value.runtime, 1, provider({ models: [row('new')] }));
  else unregisterProvider(value.runtime, 0, 'coralbricks');
  assert.equal(context.signal.aborted, true); assert.equal(signal.aborted, true);
  assert.equal(await context.publish({ update() { assert.fail('stale update cannot execute'); } }), false);
  await value.flush();
  assert.deepEqual(customProviderModels(value.runtime).map(model => model.id), operation === 'replace' ? ['new'] : []);
});

test('replacement during a pending publication ACK prevents stale publication success and local snapshots', async () => {
  const value = fixture(), gate = deferred(), requested = deferred(); let context;
  registerProvider(value.runtime, 0, provider({ refreshModels(input) { context = input; } })); await value.initialize();
  const original = value.runtime.transport.request;
  value.runtime.transport.request = async (method, params) => { const result = await original(method, params); if (params.models?.[0].api_name === 'stale') { requested.resolve(); await gate.promise; } return result; };
  const publication = context.publish({ update() { providerState(value.runtime).desired.get('coralbricks').custom.models = [row('stale')]; } });
  await requested.promise;
  registerProvider(value.runtime, 1, provider({ models: [row('new')] }));
  gate.resolve(); assert.equal(await publication, false); await value.flush();
  assert.equal(customProviderModels(value.runtime)[0].id, 'new');
  assert.equal(providerState(value.runtime).committed.get('coralbricks').models[0].pi.id, 'new');
});

for (const operation of ['replace', 'remove']) test(`failed host ${operation} rolls back process-local registration and catalog`, async () => {
  const value = fixture(), first = provider();
  registerProvider(value.runtime, 0, first); await value.initialize();
  const original = value.runtime.transport.request;
  value.runtime.transport.request = async (method, params) => { await original(method, params); throw new Error('host refusal'); };
  if (operation === 'replace') registerProvider(value.runtime, 1, provider({ models: [row('rejected')] }));
  else unregisterProvider(value.runtime, 0, 'coralbricks');
  await assert.rejects(value.pending.at(-1), /host refusal/); await value.flush();
  assert.equal(value.registry.getRegisteredNativeProvider('coralbricks'), first);
  assert.equal(customProviderModels(value.runtime)[0].id, 'own/model');
  assert.equal(await getCustomProviderApiKey(value.runtime, 'coralbricks'), 'extension-owned-key');
});

test('failed refresh host update retains the last admitted process-local catalog', async () => {
  const value = fixture(); let context;
  const object = provider({ refreshModels(input) { context = input; } });
  registerProvider(value.runtime, 0, object); await value.initialize();
  value.runtime.transport.request = async () => { throw new Error('refresh refused'); };
  await assert.rejects(context.publish({ update() { object.models = [row('uncommitted')]; } }), /refresh refused/);
  assert.equal(customProviderModels(value.runtime)[0].id, 'own/model');
  assert.equal(providerState(value.runtime).committed.get('coralbricks').models[0].pi.id, 'own/model');
});

test('failed startup host registration removes uncommitted custom facts', async () => {
  const value = fixture();
  registerProvider(value.runtime, 0, provider());
  value.runtime.transport.request = async () => { throw new Error('startup refused'); };
  await value.initialize();
  assert.equal(value.errors.length, 1); assert.match(value.errors[0].message, /startup refused/);
  assert.equal(hasCustomProvider(value.runtime, 'coralbricks'), false);
  assert.deepEqual(value.registry.getRegisteredProviderIds(), []);
});

for (const malformed of ['async', 'duplicate', 'foreign', 'function', 'url']) test(`malformed ${malformed} catalog fails before replacing an existing object`, () => {
  const value = fixture(), first = provider(), second = provider();
  registerProvider(value.runtime, 0, first);
  if (malformed === 'async') second.getModels = async () => [row()];
  if (malformed === 'duplicate') second.models.push(row());
  if (malformed === 'foreign') second.models[0].provider = 'openai';
  if (malformed === 'function') second.models[0].compat.callback = () => {};
  if (malformed === 'url') second.models[0].baseUrl = 'https://user:secret@custom.invalid';
  assert.throws(() => registerProvider(value.runtime, 1, second), /invalid|unsupported/);
  assert.equal(value.registry.getRegisteredNativeProvider('coralbricks'), first);
  assert.equal(customProviderModels(value.runtime)[0].id, 'own/model');
});

test('factory rollback restores the previous custom object without stale cancellation', async () => {
  const value = fixture(), first = provider(); registerProvider(value.runtime, 0, first);
  const restore = checkpointFactoryProviders(value.runtime);
  registerProvider(value.runtime, 1, provider({ models: [row('failed-factory')] })); restore();
  assert.equal(value.registry.getRegisteredNativeProvider('coralbricks'), first);
  await value.initialize();
  assert.equal(await getCustomProviderApiKey(value.runtime, 'coralbricks'), 'extension-owned-key');
});

test('switching back to named config removes the custom object rather than merging its catalog', () => {
  const value = fixture(); registerProvider(value.runtime, 0, provider());
  registerProvider(value.runtime, 1, 'coralbricks', { api: 'openai-completions', streamSimple: emptyStream });
  assert.equal(hasCustomProvider(value.runtime, 'coralbricks'), false);
  assert.deepEqual(customProviderModels(value.runtime), []);
  assert.equal(value.registry.getRegisteredNativeProvider('coralbricks'), undefined);
  assert.equal(value.registry.getRegisteredProviderConfig('coralbricks').models, undefined);
});

test('native facade getter provenance survives spread and distinct module identities, not provider IDs', async () => {
  const facade = nativeProviderModels().getProvider('anthropic');
  const duplicate = await import('../lib/native-provider.mjs?custom-provider-provenance');
  assert.equal(duplicate.isNativeProviderFacade({ ...facade, streamSimple() {} }), true);
  assert.equal(duplicate.isNativeProviderFacade({ ...facade, id: 'coralbricks' }), false);
  assert.equal(duplicate.isNativeProviderFacade({ ...facade, getModels() { return facade.getModels(); } }), false);
});

test('process shutdown cancels custom refresh lifecycle without inventing a session owner', async () => {
  const value = fixture(); let context;
  registerProvider(value.runtime, 0, provider({ refreshModels(input) { context = input; } })); await value.initialize();
  cancelProviderStream(value.runtime);
  assert.equal(context.signal.aborted, true);
  assert.equal(await context.publish({ update() { assert.fail('shutdown publication'); } }), false);
});

test('real adapter capture accepts full object rows without auth, refresh or inference', t => {
  const directory = mkdtempSync(join(tmpdir(), 'pi-custom-object-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'provider.mjs');
  writeFileSync(entry, `export default pi => pi.registerProvider({
    id: 'coralbricks', name: 'CoralBricks', baseUrl: 'https://custom.invalid/v1',
    getModels() { return [{ ...${JSON.stringify(row())}, headers: undefined }]; },
    auth: { apiKey: { resolve() { throw new Error('forbidden-capture-auth'); } } },
    refreshModels() { throw new Error('forbidden-capture-refresh'); },
    stream() { throw new Error('forbidden-capture-stream'); },
    streamSimple() { throw new Error('forbidden-capture-simple-stream'); }
  });`);
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('../runner.mjs', import.meta.url)), '--inspect', entry], {
    cwd: directory, encoding: 'utf8', timeout: 10000, maxBuffer: 1048576,
    env: { ...process.env, HOME: directory, OCTET_PI_AGENT_DIR: join(directory, 'no-pi-agent'), PI_CODING_AGENT_DIR: join(directory, 'no-pi-agent') },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stderr + result.stdout, /forbidden-capture-|extension-owned-key/);
  const captured = JSON.parse(result.stdout);
  assert.deepEqual(captured.result.tools, []); assert.deepEqual(captured.result.commands, []);
});

test('custom auth resolves only in the current admitted owner and obeys invocation cancellation', async () => {
  const value = fixture(), entered = deferred(); let calls = 0;
  registerProvider(value.runtime, 0, provider({ auth: { apiKey: { resolve({ signal }) { calls++; entered.resolve(signal); return new Promise(() => {}); } } } }));
  await value.initialize();
  const current = value.runtime.current;
  value.runtime.current = () => { throw new Error('not_foreground_owner'); };
  await assert.rejects(getCustomProviderApiKey(value.runtime, 'coralbricks'), /not_foreground_owner/);
  assert.equal(calls, 0);
  value.runtime.current = current;
  const resolving = getCustomProviderApiKey(value.runtime, 'coralbricks');
  const signal = await entered.promise;
  value.store.controller.abort(); await assert.rejects(resolving, /abort/i);
  assert.equal(signal.aborted, true);
});
