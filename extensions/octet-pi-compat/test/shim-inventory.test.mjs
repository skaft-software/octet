import test from 'node:test';
import assert from 'node:assert/strict';
import * as ai from '../shims/ai.mjs';
import * as coding from '../shims/coding-agent.mjs';
import * as tui from '../shims/tui.mjs';
import * as providers from '../shims/ai-providers.mjs';
import { createHash } from 'node:crypto';

test('Pi transcript and error helpers are available on the emulated path', () => {
  assert.equal(typeof ai.declarationsEqual, 'function');
  assert.equal(typeof ai.toToolDeclaration, 'function');
  assert.equal(typeof ai.isRetryableAssistantError, 'function');
  assert.equal(ai.contentText([{ type: 'text', text: 'a' }, { type: 'image' }, { type: 'text', text: 'b' }], '|'), 'a|b');
  assert.deepEqual(ai.getCurrentSystemMessage([{ role: 'system', content: 'rules', timestamp: 1 }]), { role: 'system', content: 'rules', timestamp: 1 });
});
test('Pi local UI and prompt helpers are exposed without starting a terminal or session', () => {
  assert.equal(typeof tui.MouseRegion, 'function');
  assert.equal(typeof tui.ScrollView, 'function');
  assert.equal(typeof coding.createReadOnlyTools, 'function');
  assert.equal(typeof coding.getLanguageFromPath, 'function');
  assert.equal(typeof coding.highlightCode, 'function');
  assert.equal(typeof coding.formatSkillsForPrompt, 'function');
  assert.equal(typeof coding.renderDiff, 'function');
});

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import * as core from '../shims/agent-core.mjs';
import { bindInstalledPiTheme } from '../lib/theme.mjs';
import { createJiti } from 'jiti';
import { emulatedPiAliases, typeboxAliases } from '../lib/pi-modules.mjs';

const inventory = JSON.parse(readFileSync(new URL('../shims/EXPORTS.pi-1.0.2.json', import.meta.url)));
test('every Pi 1.0.2 public export has an explicit shim binding', () => {
  for (const [pkg, ns] of Object.entries({ ai, 'coding-agent': coding, tui, 'agent-core': core, 'ai-providers': providers })) {
    for (const name of Object.keys(inventory[pkg])) assert.ok(Object.hasOwn(ns, name), `${pkg}.${name}`);
  }
  assert.equal(Object.keys(inventory.ai).length, 127);
  assert.equal(Object.keys(inventory['coding-agent']).length, 156);
  assert.equal(Object.keys(inventory.tui).length, 89);
  assert.equal(Object.keys(inventory['agent-core']).length, 8);
});

test('core, terminal, credentials and direct tool execution do not gain an unmediated runtime', async () => {
  for (const name of Object.keys(core)) assert.throws(() => core[name](), /unsupported_feature/);
  for (const name of ['TuiAltScreen', 'TuiMainScreen', 'getNativeClipboard', 'CombinedAutocompleteProvider']) {
    assert.throws(() => new tui[name](), /unsupported_feature/);
  }
  for (const name of ['createProvider', 'defaultProviderAuthContext', 'stream', 'generateImages']) assert.throws(() => ai[name](), /unsupported_feature/);
  for (const name of ['DefaultPackageManager', 'ProjectTrustStore', 'ExtensionEditorComponent', 'SessionSelectorComponent', 'ModelSelectorComponent', 'ToolExecutionComponent', 'getShellConfig', 'getPowerShellConfig']) {
    assert.throws(() => new coding[name](), /unsupported_feature/);
  }
  for (const name of ['Read', 'Write', 'Edit', 'Bash', 'Find', 'Grep', 'Ls', 'PowerShell']) {
    const definition = coding[`create${name}ToolDefinition`]();
    assert.equal(typeof definition.parameters, 'object');
    await assert.rejects(definition.execute('id', {}), /unsupported_feature/);
  }
});

test('static model inventory and skill formatting work without a managed Pi runtime', () => {
  assert.ok(ai.getProviders().includes('anthropic'));
  assert.ok(ai.getModels('anthropic').length > 0);
  for (const provider of ai.getProviders()) {
    for (const model of ai.getModels(provider)) assert.equal(ai.getModel(provider, model.id), model);
  }
  assert.equal(ai.getModel('not-a-provider', 'not-a-model'), undefined);
  assert.deepEqual(ai.getModels('not-a-provider'), []);
  assert.equal(coding.formatSkillsForPrompt([{ name: 'hidden', disableModelInvocation: true }]), '');
  assert.match(coding.formatSkillsForPrompt([{ name: '<test>', description: '&"\'', filePath: '/test' }], 'bash'), /&lt;test&gt;[\s\S]*&amp;&quot;&apos;/);
});

test('both Pi scopes resolve the core shim and new helpers through jiti', async () => {
  const jiti = createJiti(import.meta.url, { alias: { ...typeboxAliases(), ...emulatedPiAliases() }, tryNative: false });
  for (const prefix of ['@earendil-works', '@mariozechner']) {
    const agent = await jiti.import(`${prefix}/pi-agent-core`);
    assert.equal(typeof agent.Agent, 'function');
    assert.throws(() => new agent.Agent(), /unsupported_feature/);
    const aiShim = await jiti.import(`${prefix}/pi-ai`);
    assert.equal(aiShim.contentText('text'), 'text');
    assert.ok(aiShim.getModels('anthropic').length > 0);
    const ui = await jiti.import(`${prefix}/pi-tui`);
    class Region extends ui.MouseRegion {}
    const child = { render: () => ['ok'], invalidate() {} };
    assert.deepEqual(new Region(child, () => {}).render(80), ['ok']);
  }
});

const reference = process.env.PI_SHIM_REFERENCE_DIR;
test('pinned pure and managed-local helpers agree with real Pi 1.0.2 dist', { skip: !reference && 'set PI_SHIM_REFERENCE_DIR to the reviewed Pi node_modules/@earendil-works directory' }, async () => {
  const provenance = JSON.parse(readFileSync(new URL('../shims/pi-1.0.2/UPSTREAM.json', import.meta.url)));
  for (const [path, expected] of Object.entries(provenance.sha256)) {
    assert.equal(createHash('sha256').update(readFileSync(join(reference, path))).digest('hex'), expected, path);
  }
  const require = createRequire(import.meta.url);
  const realAi = file => require(join(reference, 'pi-ai/dist', file));
  const realCoding = file => require(join(reference, 'pi-coding-agent/dist', file));
  const tools = realCoding('core/tools/index.js');
  const definitions = JSON.parse(readFileSync(new URL('../shims/pi-1.0.2/tool-definitions.json', import.meta.url)));
  for (const name of ['Read', 'Edit', 'Write', 'Bash', 'PowerShell', 'Grep', 'Find', 'Ls']) {
    const original = tools[`create${name}ToolDefinition`]('.');
    const expected = JSON.parse(JSON.stringify(Object.fromEntries(Object.entries(original).filter(([, value]) => typeof value !== 'function'))));
    assert.deepEqual(definitions[original.name], expected, original.name);
    const brokered = coding[`create${name}ToolDefinition`]('.');
    assert.deepEqual(JSON.parse(JSON.stringify(Object.fromEntries(Object.entries(brokered).filter(([, value]) => typeof value !== 'function')))), expected, original.name);
  }
  assert.deepEqual(ai.getProviders(), Object.keys(realAi('models.generated.js').MODELS));
  for (const provider of ai.getProviders()) assert.deepEqual(ai.getModels(provider), Object.values(realAi('models.generated.js').MODELS[provider]));
  const messages = [
    { role: 'system', content: 'head', sections: { a: 'one' }, timestamp: 1, toolsAdded: [{ name: 'a', description: 'a', parameters: { type: 'object' } }] },
    { role: 'user', content: 'hello', timestamp: 2 },
    { role: 'system', content: [{ type: 'text', text: 'tail' }], sections: { a: null, b: 'two' }, timestamp: 3, toolsRemoved: [{ name: 'a' }] },
  ];
  for (const name of ['getCurrentSystemMessage', 'getCurrentSystemPrompt', 'getCurrentTools', 'getInitialSystemMessage', 'withoutInitialSystemMessage', 'getDeclaredTools', 'hasToolRedefinitions', 'hasNonAdditiveToolChanges']) {
    assert.deepEqual(ai[name](messages), realAi('utils/transcript.js')[name](messages), name);
  }
  const tool = { name: 'x', description: 'd', parameters: { type: 'object' }, execute() {} };
  assert.deepEqual(ai.toToolDeclaration(tool), realAi('utils/transcript.js').toToolDeclaration(tool));
  for (const errorMessage of ['429 throttled', 'overloaded', 'insufficient_quota', 'billing', 'unknown']) {
    const message = { stopReason: 'error', errorMessage };
    assert.equal(ai.isRetryableAssistantError(message), realAi('utils/retry.js').isRetryableAssistantError(message));
  }
  const skills = [{ name: 'x<&', description: 'hello "\'', filePath: '/a', disableModelInvocation: false }];
  for (const tool of ['read', 'bash']) assert.equal(coding.formatSkillsForPrompt(skills, tool), realCoding('core/skills.js').formatSkillsForPrompt(skills, tool));
  bindInstalledPiTheme();
  for (const path of ['a.ts', 'b.py', 'x.rs', 'file.unknown']) assert.equal(coding.getLanguageFromPath(path), realCoding('modes/interactive/theme/theme.js').getLanguageFromPath(path));
  for (const lang of ['typescript', undefined, 'not-a-language']) assert.deepEqual(coding.highlightCode('const x = 1;\n', lang), realCoding('modes/interactive/theme/theme.js').highlightCode('const x = 1;\n', lang));
  assert.equal(coding.renderDiff('+1 hi\n-2 old'), realCoding('modes/interactive/components/diff.js').renderDiff('+1 hi\n-2 old'));
  const store = new ai.InMemoryModelsStore();
  await store.write('test', { models: [] });
  assert.deepEqual(await store.read('test'), { models: [] });
  class Stream extends ai.EventStream {}
  const stream = new Stream(value => value.done, value => value.result);
  stream.push({ done: true, result: 'ok' });
  assert.equal(await stream.result(), 'ok');
});

test('environment auth and lazy adapters only use caller inputs and synthetic streams', async () => {
  assert.equal(ai.getEnvApiKey('openai', { OPENAI_API_KEY: 'fixture-key' }), 'fixture-key');
  assert.deepEqual(ai.findEnvKeys('anthropic', { ANTHROPIC_AUTH_TOKEN: 'fixture-bearer', ANTHROPIC_OAUTH_TOKEN: 'fixture-oauth', ANTHROPIC_API_KEY: 'fixture-api-key' }), ['ANTHROPIC_AUTH_TOKEN', 'ANTHROPIC_OAUTH_TOKEN', 'ANTHROPIC_API_KEY']);
  assert.equal(ai.getEnvApiKey('not-a-provider'), undefined);
  const auth = ai.envApiKeyAuth('Fixture', ['FIXTURE_KEY']), signal = new AbortController().signal;
  const calls = [];
  assert.deepEqual(await auth.resolve({ ctx: { env: async key => { calls.push(key); return 'fixture-key'; } }, signal }), { auth: { apiKey: 'fixture-key' }, source: 'FIXTURE_KEY' });
  assert.deepEqual(calls, ['FIXTURE_KEY']);
  assert.deepEqual(await auth.resolve({ ctx: { env() { assert.fail('stored caller credential wins'); } }, credential: { key: 'fixture-stored', env: { NAME: 'fixture' } }, signal }), { auth: { apiKey: 'fixture-stored' }, env: { NAME: 'fixture' }, source: 'stored credential' });
  const model = { api: 'fixture', provider: 'fixture', id: 'fixture' }, message = { stopReason: 'stop', content: [] };
  const source = () => { const stream = ai.createAssistantMessageEventStream(); stream.push({ type: 'done', reason: 'stop', message }); return stream; };
  const stream = ai.lazyStream(model, async () => source());
  assert.ok(stream instanceof ai.AssistantMessageEventStream);
  assert.deepEqual(await stream.result(), message);
  const api = ai.lazyApi(async () => ({ stream: source, streamSimple: source }));
  assert.deepEqual(await api.stream(model, {}).result(), message);
  const error = await ai.lazyStream(model, async () => { throw Error('fixture setup error'); }).result();
  assert.equal(error.stopReason, 'error'); assert.equal(error.errorMessage, 'fixture setup error');
  let loads = 0;
  const oauth = ai.lazyOAuth({ name: 'Fixture', load: async () => {
    loads++;
    return { login: async (...args) => args, refresh: async (...args) => args, toAuth: async value => ({ value }) };
  } });
  assert.equal(loads, 0);
  assert.deepEqual(await oauth.login('interaction', 'options'), ['interaction', 'options']);
  assert.deepEqual(await oauth.refresh('credential', signal), ['credential', signal]);
  assert.deepEqual(await oauth.toAuth('fixture-credential'), { value: 'fixture-credential' });
  assert.equal(loads, 1);
});

test('provider catalog subpath uses static inventory and guarded native delegates on both scopes', async () => {
  const jiti = createJiti(import.meta.url, { alias: { ...typeboxAliases(), ...emulatedPiAliases() }, tryNative: false });
  for (const prefix of ['@earendil-works', '@mariozechner']) {
    const providers = await jiti.import(`${prefix}/pi-ai/providers/all`);
    assert.deepEqual(providers.getBuiltinProviders(), ai.getProviders());
    const models = providers.builtinModels();
    assert.ok(models.getProvider('anthropic'));
    assert.equal(typeof models.getProvider('anthropic').stream, 'function');
    assert.deepEqual(models.getModels('anthropic'), ai.getModels('anthropic'));
    assert.throws(() => models.getProvider('anthropic').stream(ai.getModels('anthropic')[0], { messages: [] }), /admitted provider request/);
    assert.throws(() => models.getProvider('anthropic').auth.apiKey.resolve(), /authentication remains host-owned/);
    assert.throws(() => providers.builtinModels({ store: {} }), /option would not be honored/);
    assert.ok(providers.getBuiltinModelDataGeneratedAt() > 0);
    assert.equal(providers.getAllBuiltinModels('anthropic').length, ai.getModels('anthropic').length + ai.getImageModels('anthropic').length);
  }
});
