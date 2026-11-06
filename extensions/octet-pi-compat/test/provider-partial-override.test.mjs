import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { modelRegistry, registerProvider, startProviderRegistration, validateProviderInitialization } from '../lib/providers.mjs';

const definition = streamSimple => ({
  api: 'anthropic-messages', baseUrl: 'https://example.invalid', apiKey: 'explicit-dummy',
  models: [{ id: 'explicit-model', name: 'Explicit', reasoning: false, input: ['text'],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 8192, maxTokens: 1024 }],
  streamSimple,
});
function fixture() {
  const frames = [];
  const runtime = {
    initialized: false, features: new Set(['provider_proxy_v1', 'provider_credentials']),
    require(feature) { if (!this.features.has(feature)) throw Object.assign(new Error(`unsupported_feature ${feature}`), { code: -32601 }); },
    transport: {
      async request(method, params) { frames.push({ method, params }); return { revision: 1 }; },
      async notify(method, params) { frames.push({ method, params }); },
    },
    backgroundError(error) { throw error; },
  };
  return { runtime, frames, registry: modelRegistry(() => ({}), runtime) };
}

for (const name of ['anthropic', 'openai', 'some-provider']) for (const initialized of [false, true]) {
  test(`${name} (${initialized ? 'live' : 'startup'}): native inheritance requires an explicitly negotiated host service`, () => {
    const { runtime, frames, registry } = fixture();
    runtime.initialized = initialized;
    let called = false;
    const register = () => registerProvider(runtime, 0, name, {
      api: 'anthropic-messages', streamSimple() { called = true; },
    });
    if (initialized) {
      assert.throws(register, /unsupported_feature provider_inheritance_v1/);
      assert.deepEqual(registry.getRegisteredProviderIds(), []);
    } else {
      register();
      assert.deepEqual(registry.getRegisteredProviderIds(), [name]);
      assert.throws(() => validateProviderInitialization(runtime), /unsupported_feature provider_inheritance_v1/);
    }
    assert.equal(called, false);
    assert.deepEqual(frames, []);
  });
}

for (const config of [{ baseUrl: 'https://example.invalid' }, { api: 'anthropic-messages', models: undefined, streamSimple() {} }]) {
  test(`omitted models (${Object.keys(config).join(', ')}) are deferred, not malformed explicit definitions`, () => {
    const { runtime, frames, registry } = fixture();
    registerProvider(runtime, 0, 'anthropic', config);
    assert.deepEqual(registry.getRegisteredProviderConfig('anthropic'), Object.fromEntries(Object.entries(config).filter(([, value]) => value !== undefined)));
    assert.deepEqual(frames, []);
  });
}

test('public model facts and credentials alone cannot admit a native transport', () => {
  const { runtime, frames } = fixture();
  runtime.initialized = true;
  runtime.host = { pi_models: { all_models: [{ id: 'native-model', provider: 'anthropic', api: 'anthropic-messages' }] } };
  runtime.hostCall = () => assert.fail('registration must not resolve native credentials');
  assert.throws(() => registerProvider(runtime, 0, 'anthropic', {
    api: 'anthropic-messages', streamSimple() { assert.fail('not an admitted transport'); },
  }), /unsupported_feature provider_inheritance_v1/);
  assert.deepEqual(frames, []);
});

test('partial replacement preserves preceding explicit models and merges its new callback', async () => {
  const { runtime, frames, registry } = fixture();
  const config = definition(() => {}), callback = () => assert.fail('registration is not inference');
  registerProvider(runtime, 0, 'explicit-provider', config);
  registerProvider(runtime, 1, 'explicit-provider', { api: 'anthropic-messages', streamSimple: callback });
  assert.equal(registry.getRegisteredProviderConfig('explicit-provider').streamSimple, callback);
  assert.deepEqual(registry.getRegisteredProviderConfig('explicit-provider').models, config.models);
  const completed = new Promise(resolve => {
    const notify = runtime.transport.notify;
    runtime.transport.notify = async (...args) => { await notify(...args); resolve(); };
  });
  runtime.initialized = true;
  startProviderRegistration(runtime);
  await completed;
  assert.deepEqual(frames.map(frame => frame.method), ['providers/register', 'providers/complete']);
  assert.equal(frames[0].params.models[0].api_name, 'explicit-model');
  assert.doesNotMatch(JSON.stringify(frames), /explicit-dummy/);
});

for (const models of [null, {}, new Array(257).fill({})]) {
  test(`malformed or out-of-bounds explicit models (${models === null ? 'null' : Array.isArray(models) ? models.length : 'object'}) retain validation`, () => {
    const { runtime, frames } = fixture();
    assert.throws(() => registerProvider(runtime, 0, 'explicit-provider', { ...definition(() => {}), models }),
      error => error.code === -32602 && /provider models/.test(error.message));
    assert.deepEqual(frames, []);
  });
}

test('explicit models without a custom stream do not fall back to native HTTP', () => {
  const { runtime, frames } = fixture();
  assert.throws(() => registerProvider(runtime, 0, 'explicit-provider', definition(undefined)),
    error => error.code === -32601 && /custom streamSimple/.test(error.message));
  assert.deepEqual(frames, []);
});

test('real adapter capture records transport-only intent without running inference or resolving credentials', t => {
  const directory = mkdtempSync(join(tmpdir(), 'pi-provider-partial-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'provider.mjs');
  writeFileSync(entry, `export default pi => pi.registerProvider('anthropic', {
    api: 'anthropic-messages', streamSimple() { throw new Error('must not execute'); }
  });`);
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('../runner.mjs', import.meta.url)), '--inspect', entry], {
    cwd: directory, encoding: 'utf8', timeout: 10000, maxBuffer: 1048576,
    env: { ...process.env, HOME: directory, OCTET_PI_AGENT_DIR: join(directory, 'no-pi-agent'), PI_CODING_AGENT_DIR: join(directory, 'no-pi-agent') },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stderr + result.stdout, /must not execute|apiKey|authorization/i);
  const captured = JSON.parse(result.stdout);
  assert.deepEqual(captured.result.tools, []);
  assert.deepEqual(captured.result.commands, []);
});
