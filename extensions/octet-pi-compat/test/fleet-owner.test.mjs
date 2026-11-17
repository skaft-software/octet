import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createAPI, createContext } from '../lib/api.mjs';
import { owner, host } from './helper.mjs';

function fixture(t) {
  const calls = [];
  const runtime = new Runtime({ extensions: [] }, {
    request: async (method, params) => { calls.push({ method, params }); return { name: params.name }; },
    requestSync: (method, params) => {
      calls.push({ method, params });
      assert.equal(method, 'context/system_prompt');
      return { text: `prompt:${params.resource_owner.session_id}` };
    },
  });
  t.after(() => runtime.uninstallChildren());
  runtime.features = new Set(['session_entries', 'system_prompt_read']);
  let id = 0;
  function bind(who, hook = 'before_tool_call') {
    const store = { id: ++id, method: 'hook/run', hook, live: true, controller: new AbortController(), pending: new Set(), errors: [] };
    runtime.active.set(store.id, store);
    runtime.bind({ hook, context: { resource_owner: who, host: { ...host, reasoning: 'off', session_id: who.session_id, has_ui: false } } }, store);
    return store;
  }
  const lead = bind(owner, 'session_start');
  return { runtime, calls, bind, lead, pi: createAPI(runtime, 0) };
}

// Native delegation shares the adapter, but each hook has its own issued owner.
// Pi's subagent example instead starts a separate process per worker:
// packages/coding-agent/examples/extensions/subagent/index.ts:342-348.
test('three live child hook contexts can use session APIs without the lead UI lease', async t => {
  const { runtime, calls, bind, lead, pi } = fixture(t);
  const children = Array.from({ length: 3 }, (_, i) => bind({ ...owner, session_id: `child-${i}` }));
  for (const store of [...children, lead]) {
    const context = createContext(runtime, store);
    await runtime.scope.run(store, async () => {
      assert.equal(pi.getThinkingLevel(), 'off');
      assert.equal(context.sessionManager.getSessionId(), store.state.owner.session_id);
      assert.equal(context.getSystemPrompt(), `prompt:${store.state.owner.session_id}`);
      await pi.setSessionName(store.state.owner.session_id);
    });
  }
  assert.equal(calls.length, 8);
  for (const child of children) {
    assert.throws(() => runtime.assertOwner(child), /inactive session/);
    await runtime.retire(child.state);
  }
  runtime.assertOwner(lead);
  await runtime.scope.run(lead, () => pi.setSessionName('lead still works'));
  assert.throws(() => runtime.scope.run(children[0], () => pi.getThinkingLevel()), /retained context is unavailable/);
});

test('starting child hook bindings and retiring children never replace or retire the lead', async t => {
  const { runtime, bind, lead } = fixture(t);
  for (let i = 0; i < 3; i++) {
    // Native workers enter through owner-scoped hooks, not the frontend's
    // session_start lifecycle (which intentionally switches the root).
    const child = bind({ ...owner, session_id: `started-child-${i}` });
    assert.equal(runtime.foreground, lead.state);
    assert.equal(lead.state.alive, true);
    assert.equal(createContext(runtime, child).sessionManager.getSessionId(), `started-child-${i}`);
    await runtime.retire(child.state);
  }
  runtime.assertOwner(lead);
});

test('process-wide registration keeps its foreground fence while child session reads remain available', t => {
  const runtime = new Runtime({ extensions: [] }, {});
  t.after(() => runtime.uninstallChildren());
  runtime.loaded = true;
  runtime.initialized = true;
  runtime.features.add('dynamic_tools');
  runtime.features.add('provider_proxy_v1');
  const root = { id: 1, method: 'command/execute', controller: new AbortController() };
  runtime.bind({ context: { resource_owner: owner, host } }, root);
  const child = { id: 2, method: 'hook/run', controller: new AbortController() };
  runtime.bind({ context: { resource_owner: { ...owner, session_id: 'worker' }, host: { ...host, reasoning: 'off' } } }, child);
  const pi = createAPI(runtime, 0);
  runtime.scope.run(child, () => {
    assert.equal(pi.getThinkingLevel(), 'off');
    assert.throws(() => pi.registerTool({ name: 'global', description: 'Global tool', parameters: { type: 'object' }, execute() {} }), /not_foreground_owner/);
    assert.throws(() => pi.registerMessageRenderer('global', () => undefined), /not_foreground_owner/);
    assert.throws(() => pi.unregisterProvider('global-provider'), /not_foreground_owner/);
  });
  runtime.assertOwner(root);
});
