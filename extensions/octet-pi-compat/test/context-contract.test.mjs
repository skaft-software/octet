import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createContext } from '../lib/api.mjs';
import { contextFacts, getSettings, transformInput } from '../lib/context-api.mjs';
import { projectContext } from '../lib/provider-context.mjs';
import { owner } from './helper.mjs';

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
  assert.throws(() => getSettings(runtime, store), /authoritative settings/);
  store.state.host = { mode: 'rpc', project_trusted: false, system_prompt_options: { cwd: '/actual', customPrompt: 'base' }, settings: { defaultProvider: 'actual' } };
  assert.equal(facts.mode, 'rpc'); assert.equal(facts.isProjectTrusted(), false);
  const options = facts.getSystemPromptOptions(); options.customPrompt = 'mutated';
  assert.equal(facts.getSystemPromptOptions().customPrompt, 'base');
  const settings = getSettings(runtime, store); settings.defaultProvider = 'mutated';
  assert.equal(getSettings(runtime, store).defaultProvider, 'actual');
  store.method = 'hook/run'; assert.throws(() => facts.getSystemPromptOptions(), /command context only/);
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
