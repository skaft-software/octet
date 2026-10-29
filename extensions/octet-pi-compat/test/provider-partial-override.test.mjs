import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { modelRegistry, registerProvider, startProviderRegistration } from '../lib/providers.mjs';

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
    require(feature) { assert.ok(this.features.has(feature)); },
    transport: {
      async request(method, params) { frames.push({ method, params }); return { revision: 1 }; },
      async notify(method, params) { frames.push({ method, params }); },
    },
    backgroundError(error) { throw error; },
  };
  return { runtime, frames, registry: modelRegistry(() => ({}), runtime) };
}
const unsupportedPartial = error => error.code === -32601
  && /unsupported_feature registerProvider partial override/.test(error.message)
  && /native.*inheritance/.test(error.message)
  && !/invalid_request/.test(error.message);

// The installed Pi 1.0.2 ProviderConfig allows omission of models, including
// api + streamSimple transport overrides. These are synthetic registrations,
// never imports or execution of a user's installed provider factory.
for (const name of ['anthropic', 'openai', 'some-provider']) for (const initialized of [false, true]) {
  test(`${name} (${initialized ? 'live' : 'startup'}): a valid Pi transport-only override is an explicit native contract gap`, () => {
    const { runtime, frames, registry } = fixture();
    runtime.initialized = initialized;
    let called = false;
    assert.throws(() => registerProvider(runtime, 0, name, {
      api: 'anthropic-messages', streamSimple() { called = true; },
    }), unsupportedPartial);
    assert.equal(called, false);
    assert.deepEqual(registry.getRegisteredProviderIds(), []);
    assert.equal(registry.getRegisteredProviderConfig(name), undefined);
    assert.deepEqual(frames, []);
  });
}

for (const config of [{ baseUrl: 'https://example.invalid' }, { api: 'anthropic-messages', models: undefined, streamSimple() {} }]) {
  test(`omitted models (${Object.keys(config).join(', ')}) are not malformed explicit definitions`, () => {
    const { runtime, frames } = fixture();
    assert.throws(() => registerProvider(runtime, 0, 'anthropic', config), unsupportedPartial);
    assert.deepEqual(frames, []);
  });
}

test('native model facts and a credentials grant do not grant provider transport ownership', () => {
  const { runtime, frames } = fixture();
  // A public model snapshot is not the native model's full endpoint, auth,
  // request configuration, or a grant to replace its generation-fenced route.
  runtime.host = { pi_models: { all_models: [{
    id: 'native-model', provider: 'anthropic', api: 'anthropic-messages',
    base_url: 'https://native.invalid', oauth: true, reasoning: false, input: ['text'],
    context_window: 8192, max_tokens: 1024,
  }] } };
  runtime.hostCall = () => assert.fail('registration must not resolve native credentials');
  assert.throws(() => registerProvider(runtime, 0, 'anthropic', {
    api: 'anthropic-messages', streamSimple() { assert.fail('not an admitted transport'); },
  }), unsupportedPartial);
  assert.deepEqual(frames, []);
});

test('refused partial replacement leaves the preceding explicit provider intact', async () => {
  const { runtime, frames, registry } = fixture();
  const config = definition(() => {});
  registerProvider(runtime, 0, 'explicit-provider', config);
  assert.throws(() => registerProvider(runtime, 1, 'explicit-provider', {
    api: 'anthropic-messages', streamSimple() { assert.fail('refused replacement'); },
  }), unsupportedPartial);
  assert.equal(registry.getRegisteredProviderConfig('explicit-provider'), config);
  // Join the existing startup notification without relying on wall-clock delays.
  const completed = new Promise(resolve => {
    const notify = runtime.transport.notify;
    runtime.transport.notify = async (...args) => { await notify(...args); resolve(); };
  });
  runtime.initialized = true;
  startProviderRegistration(runtime);
  await completed;
  assert.deepEqual(frames.map(frame => frame.method), ['providers/register', 'providers/complete']);
  assert.equal(frames[0].params.models[0].api_name, 'explicit-model');
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
    error => error.code === -32601 && /native HTTP overrides/.test(error.message));
  assert.deepEqual(frames, []);
});

test('real adapter capture reports the synthetic partial override gap instead of publishing registrations', t => {
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
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /unsupported_feature registerProvider partial override/);
  assert.doesNotMatch(result.stderr, /invalid_request|must not execute/);
  assert.equal(result.stdout, '', 'refusal must not publish a successful captured catalog');
});
