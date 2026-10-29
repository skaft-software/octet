import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createContext } from '../lib/api.mjs';
import { contextFacts, getSettings, transformInput } from '../lib/context-api.mjs';
import { projectContext } from '../lib/provider-context.mjs';
import { owner } from './helper.mjs';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

function harness(method = 'hook/run', host = {}) {
  const errors = [];
  const runtime = new Runtime({ extensions: ['/reviewed/first.ts', '/reviewed/second.ts'] }, {
    notify: async (method, params) => { if (method === 'notification') errors.push(params); },
  });
  runtime.uninstallChildren(); // This harness does not exercise the child facade.
  runtime.backgroundError = error => errors.push(error);
  const store = { id: 1, method, controller: new AbortController(), pending: new Set(), errors: [], live: true };
  runtime.bind({ context: { workspace: '/actual', resource_owner: owner, host } }, store, { foreground: true });
  return { runtime, store, errors };
}
const image = { Image: { source: { Inline: 'eA==' }, media_type: 'image/png', detail: null } };
const input = { phase: 'input', text: 'raw', source: 'interactive', images: [image] };

test('input chains transforms, retains omitted images, and [] clears images', async () => {
  const { runtime, store } = harness(); const seen = [];
  runtime.events.set('input', [
    { factory: 0, handler(event) { seen.push(event.text); return { action: 'transform', text: 'first' }; } },
    { factory: 1, handler(event) { seen.push(event.text, event.images[0].data); return { action: 'transform', text: 'last', images: [] }; } },
  ]);
  assert.deepEqual(await transformInput(runtime, input, store), { action: 'transform', text: 'last', images: [] });
  assert.deepEqual(seen, ['raw', 'first', 'eA==']);
});
test('handled input short-circuits remaining handlers', async () => {
  const { runtime, store } = harness();
  runtime.events.set('input', [{ factory: 0, handler: () => ({ action: 'handled' }) },
    { factory: 1, handler: () => { throw Error('must not run'); } }]);
  assert.deepEqual(await transformInput(runtime, input, store), { action: 'handled' });
});
test('ordinary input handler errors continue without leaking the input', async () => {
  const { runtime, store, errors } = harness();
  runtime.events.set('input', [{ factory: 0, handler: () => { throw Error('secret input'); } },
    { factory: 1, handler: () => ({ action: 'transform', text: 'survived' }) }]);
  assert.deepEqual(await transformInput(runtime, input, store), { action: 'transform', text: 'survived', images: [image] });
  assert.equal(errors.length, 1); assert.equal(errors[0].title, '[Extension issues]');
  assert.match(errors[0].message, /input callback failed/); assert.doesNotMatch(errors[0].message, /secret input/);
});
test('input snapshots handlers once; unsubscribe does not alter an active dispatch', async () => {
  const { runtime, store } = harness(); let second = false;
  const entries = [{ factory: 0, handler: () => { entries.pop(); } }, { factory: 1, handler: () => { second = true; } }];
  runtime.events.set('input', entries);
  assert.deepEqual(await transformInput(runtime, input, store), { action: 'continue' }); assert.equal(second, true);
});
test('image replacement uses real canonical image bytes and MIME type', async () => {
  const { runtime, store } = harness();
  runtime.events.set('input', [{ factory: 0, handler: () => ({ action: 'transform', text: 'image', images: [{ type: 'image', data: 'eQ==', mimeType: 'image/jpeg' }] }) }]);
  assert.deepEqual(await transformInput(runtime, input, store), { action: 'transform', text: 'image', images: [{ Image: { source: { Inline: 'eQ==' }, media_type: 'image/jpeg', detail: null } }] });
});
test('session directory derives only from the authoritative native session file', () => {
  const { runtime, store } = harness();
  const manager = createContext(runtime, store).sessionManager;
  assert.throws(() => manager.getSessionDir(), /session_file snapshot not supplied/);
  store.state.host.session_file = null;
  assert.throws(() => manager.getSessionDir(), /no persistent session directory/);
  store.state.host.session_file = '/actual/native-sessions/session.jsonl';
  assert.equal(manager.getSessionDir(), '/actual/native-sessions');
  assert.equal(manager.getSessionFile(), '/actual/native-sessions/session.jsonl');
  store.state.alive = false;
  assert.throws(() => manager.getSessionDir(), /not_foreground_owner/);
});

function systemPromptHarness() {
  const { runtime, store } = harness('command/execute');
  const calls = [];
  let result = { text: 'actual native service text\n文' };
  runtime.features.add('system_prompt_read');
  runtime.active.set(store.id, store);
  runtime.transport.requestSync = (method, params, options) => {
    calls.push({ method, params, options });
    return result;
  };
  return { runtime, store, calls, ctx: createContext(runtime, store), reply(value) { result = value; } };
}

test('getSystemPrompt synchronously reads exact current native service text, including empty', () => {
  const { runtime, store, ctx, calls, reply } = systemPromptHarness();
  // Even a leftover provider-preparation fact is not command authority.
  store.state.host.system_prompt = 'stale provider preparation';
  assert.equal(ctx.getSystemPrompt(), 'actual native service text\n文');
  assert.deepEqual(calls[0].params, { parent_request_id: store.id, resource_owner: owner });
  assert.equal(calls[0].method, 'context/system_prompt');
  assert.equal(calls[0].options.parent, store.id);
  assert.equal(calls[0].options.signal, store.controller.signal);
  reply({ text: 'changed live Agent text' });
  assert.equal(ctx.getSystemPrompt(), 'changed live Agent text');
  reply({ text: '' });
  assert.equal(ctx.getSystemPrompt(), '');
  // The native contract allows controls except NUL and counts UTF-8 bytes.
  const maximum = '文'.repeat(87381) + '\t';
  assert.equal(Buffer.byteLength(maximum), 262144);
  reply({ text: maximum });
  assert.equal(ctx.getSystemPrompt(), maximum);
  assert.equal(calls.length, 4);
  assert.equal(store.state.host.system_prompt, 'stale provider preparation');
  runtime.active.delete(store.id);
});

test('getSystemPrompt refuses absent negotiation, retired/settled/reused parents and cancellation without cached fallback', () => {
  const { runtime, store, ctx, calls } = systemPromptHarness();
  store.state.host.system_prompt = 'must not disclose ambient text';
  runtime.features.delete('system_prompt_read');
  assert.throws(() => ctx.getSystemPrompt(), /unsupported_feature.*system_prompt_read/);
  runtime.features.add('system_prompt_read');
  const issued = store.state.owner;
  store.state.owner = undefined;
  assert.throws(() => ctx.getSystemPrompt(), /not_foreground_owner/);
  store.state.owner = issued; store.state.alive = false;
  assert.throws(() => ctx.getSystemPrompt(), /not_foreground_owner/);
  store.state.alive = true; store.live = false;
  assert.throws(() => ctx.getSystemPrompt(), /live request|active numeric parent/);
  store.live = true;
  runtime.active.set(store.id, { ...store, controller: new AbortController() });
  assert.throws(() => ctx.getSystemPrompt(), /live request|active numeric parent/);
  runtime.active.set(store.id, store); runtime.stopping = true;
  assert.throws(() => ctx.getSystemPrompt(), /draining/);
  runtime.stopping = false; store.controller.abort(new Error('cancelled prompt read'));
  assert.throws(() => ctx.getSystemPrompt(), /cancelled prompt read/);
  assert.equal(calls.length, 0);
});

test('getSystemPrompt refuses malformed, oversized, NUL and non-UTF-8 native replies without disclosure in errors', () => {
  const { ctx, reply } = systemPromptHarness();
  for (const result of [undefined, null, [], {}, { text: null }, { text: 4 }, { text: 'private', extra: true },
    { text: '文'.repeat(87382) }, { text: 'private\0text' }, { text: '\uD800' }]) {
    reply(result);
    assert.throws(() => ctx.getSystemPrompt(), error => {
      assert.ok([-32601, -32602].includes(error.code));
      assert.doesNotMatch(error.message, /private|文/);
      return true;
    });
  }
});

test('hasUI respects the native frontend consumer as well as negotiated support', () => {
  const { runtime, store } = harness('hook/run', { has_ui: false });
  runtime.features.add('remote_ui');
  const ctx = createContext(runtime, store);
  assert.equal(ctx.hasUI, false);
  store.state.host.has_ui = true;
  assert.equal(ctx.hasUI, true);
  runtime.features.delete('remote_ui');
  assert.equal(ctx.hasUI, false);
});

test('observation failures report the factory and continue without weakening veto or owner fences', async () => {
  const { runtime, store, errors } = harness();
  runtime.config.extensions = ['/reviewed/broken.ts', '/reviewed/kept.ts'];
  const failure = Object.assign(new Error('unsupported_feature ctx.missing: private text'), { code: -32601 });
  let kept = 0;
  const handlers = [{ factory: 0, handler() { throw failure; } }, { factory: 1, handler() { kept++; } }];
  runtime.events.set('turn_start', handlers);
  assert.deepEqual(await runtime.runEvent('turn_start', {}, store), { action: 'continue' });
  assert.equal(kept, 1);
  assert.equal(errors[0].title, '[Extension issues]');
  assert.ok(errors[0].message.includes('/reviewed/broken.ts'));
  assert.match(errors[0].message, /turn_start callback.*API or event field.*not support/);
  assert.match(errors[0].message, /Next:/);
  assert.doesNotMatch(errors[0].message, /private text|ctx\.missing/);
  runtime.events.set('tool_call', handlers);
  await assert.rejects(runtime.runEvent('tool_call', {}, store, { veto: true }), error => error === failure);
  handlers[0].handler = () => { throw Object.assign(new Error('owner lost'), { code: -32002 }); };
  await assert.rejects(runtime.runEvent('turn_start', {}, store), /owner lost/);
  handlers[0].handler = () => { store.controller.abort(); throw new Error('ordinary error'); };
  await assert.rejects(runtime.runEvent('turn_start', {}, store), /abort/i);
  assert.equal(kept, 1);
});

test('facts refuse absent snapshots and preserve false project trust', () => {
  const { runtime, store } = harness('command/execute');
  const facts = contextFacts(runtime, store);
  assert.throws(() => facts.mode, /authoritative mode/);
  assert.throws(() => facts.isProjectTrusted(), /authoritative project_trusted/);
  store.state.host = { mode: 'rpc', project_trusted: false, system_prompt_options: { cwd: '/actual', customPrompt: 'base' }, settings: { defaultProvider: 'actual' } };
  assert.equal(facts.mode, 'rpc'); assert.equal(facts.isProjectTrusted(), false);
  const options = facts.getSystemPromptOptions(); options.customPrompt = 'mutated';
  assert.equal(facts.getSystemPromptOptions().customPrompt, 'base');
  const settings = getSettings(runtime, store); settings.defaultProvider = 'mutated';
  assert.equal(getSettings(runtime, store).defaultProvider, 'actual');
  store.method = 'hook/run'; assert.throws(() => facts.getSystemPromptOptions(), /command context only/);
});
test('getSettings reads the user Pi settings the way Pi merges them; a host snapshot wins', () => {
  // macOS TMPDIR may traverse /var -> /private/var. Keep this positive
  // fixture canonical: linked ancestors are intentionally refused by discovery.
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'pi-settings-')));
  const agentDir = join(root, 'agent'), workspace = join(root, 'project');
  mkdirSync(agentDir, { recursive: true }); mkdirSync(join(workspace, '.pi'), { recursive: true });
  writeFileSync(join(agentDir, 'settings.json'), JSON.stringify({ theme: 'dark', defaultTools: ['read', 'bash'], powerline: { segments: ['cwd'], icons: true } }));
  writeFileSync(join(workspace, '.pi', 'settings.json'), JSON.stringify({ defaultTools: ['+grep'], powerline: { segments: ['model'] } }));
  const { runtime, store } = harness('command/execute');
  runtime.config.pi_agent_dir = agentDir; store.state.workspace = workspace;
  // Untrusted: project settings never count.
  assert.deepEqual(getSettings(runtime, store), { theme: 'dark', defaultTools: ['read', 'bash'], powerline: { segments: ['cwd'], icons: true } });
  store.state.host = { project_trusted: true };
  const merged = getSettings(runtime, store);
  assert.deepEqual(merged, { theme: 'dark', defaultTools: ['read', 'bash', '+grep'], powerline: { segments: ['model'], icons: true } });
  merged.theme = 'mutated';
  assert.equal(getSettings(runtime, store).theme, 'dark');
  store.state.host = { settings: { defaultProvider: 'actual' } };
  assert.deepEqual(getSettings(runtime, store), { defaultProvider: 'actual' });
  rmSync(root, { recursive: true, force: true });
});

test('isIdle follows the host run and compaction lifecycle', async () => {
  const { runtime, store } = harness();
  runtime.features.add('lifecycle_events');
  const ctx = createContext(runtime, store);
  assert.equal(ctx.isIdle(), true);
  const resource_owner = owner;
  await runtime.notify({ method: 'turn/started', params: { resource_owner } });
  assert.equal(ctx.isIdle(), false);
  await runtime.notify({ method: 'turn/settled', params: { resource_owner } });
  assert.equal(ctx.isIdle(), true);
  await runtime.notify({ method: 'compaction/started', params: { resource_owner } });
  assert.equal(ctx.isIdle(), false);
  await runtime.notify({ method: 'compaction/settled', params: { resource_owner } });
  assert.equal(ctx.isIdle(), true);
  store.state.host = { ...store.state.host, is_idle: false };
  assert.equal(ctx.isIdle(), false);
});

async function fullContext(handlers, tools = []) {
  const { runtime, store, errors } = harness(); runtime.features.add('session_entries');
  runtime.events.set('context', [{ factory: 0, handler(event) { assert.equal(event.messages[0].role, 'user'); event.messages[0].content[0].text = 'conversation'; } }]);
  runtime.events.set('context_with_system', handlers.map(handler => ({ factory: 0, handler })));
  const params = { hook: 'provider_context', context: { workspace: '/actual', resource_owner: owner, host: {} },
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 1, operation_id: 'provider-context:1', owner, expected_head: null },
    payload: { request: { system: 'base', messages: [{ User: { content: [{ Text: 'original' }] } }], tools },
      preparation: { resource_owner: owner.session_id, session_id: 'actual-session', head: null, tool_generation: 1 } } };
  return { result: await projectContext(runtime, params, store), errors };
}
test('context_with_system follows conversation transforms and chains prompt/message edits', async () => {
  const { result } = await fullContext([
    event => { assert.equal(event.messages[1].content[0].text, 'conversation'); event.messages[0].content = 'changed'; },
    event => { assert.equal(event.messages[0].content, 'changed'); event.messages[0].sections = { extra: 'section' }; event.messages.push({ role: 'user', content: 'appended' }); },
  ]);
  assert.deepEqual(result.provider_context, { system: 'changed\n\nsection', messages: [{ User: { content: [{ Text: 'conversation' }] } }, { User: { content: [{ Text: 'appended' }] } }] });
});
test('dropping a leading system with no tools clears the native prompt and reports Pi diagnostic', async () => {
  const { result, errors } = await fullContext([event => ({ messages: event.messages.slice(1) })]);
  assert.equal(result.provider_context.system, null); assert.equal(errors.length, 1);
});
test('unsupported tool changes fail explicitly, not silently ignored', async () => {
  await assert.rejects(fullContext([event => { event.messages[0].toolsAdded = []; }], [{ name: 'read', description: 'Read', parameters: { type: 'object' } }]), /cannot replace the advertised tool snapshot/);
});
