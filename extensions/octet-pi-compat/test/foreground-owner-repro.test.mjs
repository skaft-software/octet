import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { Runtime } from '../lib/runtime.mjs';
import { createContext } from '../lib/api.mjs';
import { projectContext } from '../lib/provider-context.mjs';
import { ownerKey } from '../lib/errors.mjs';
import { launch, owner, root } from './helper.mjs';

const fixture = join(root, 'test/fixtures/foreground-owner-repro.ts');
const foreign = { ...owner, session_id: 'synthetic-background-owner' };
const issue = message => frame => frame.method === 'notification'
  && frame.params.title === 'Pi compatibility error' && frame.params.message === message;
const unavailable = { session_entries: null, session_branch: null, session_leaf_id: null, session_file: null };
const ownerContext = (who, host = {}) => ({ workspace: root, resource_owner: who, host });
const contextHook = (who, badHead = false, host = {}) => ({
  hook: 'provider_context',
  context: ownerContext(who, { session_id: who.session_id, session_entries: [], session_branch: [], ...host }),
  session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 1, operation_id: 'synthetic-context:1', owner: who, expected_head: null },
  payload: { request: { messages: [{ User: { content: [{ Text: 'synthetic prompt' }] } }], system: null, tools: [] },
    preparation: { resource_owner: who.session_id, session_id: who.session_id, head: badHead ? 'wrong-head' : null, tool_generation: 0 } },
});
async function started(t, options = {}) {
  const peer = launch(t, [fixture], options);
  await peer.init(['remote_ui', 'composer', 'session_entries', 'lifecycle_events_v2']);
  await peer.start({ session_entries: [], session_branch: [], session_leaf_id: null });
  await peer.wait(frame => frame.method === 'ui/open');
  return peer;
}
async function inspect(peer, who = owner) {
  const reply = await peer.request('command/execute', { name: 'inspect-owner', arguments: [], context: ownerContext(who) }).response;
  assert.ok(reply.result, JSON.stringify(reply));
  const frame = await peer.wait(frame => frame.method === 'notification' && frame.params.message.startsWith('owner-proof:'));
  return JSON.parse(frame.params.message.slice('owner-proof:'.length));
}
const proof = reply => {
  assert.ok(reply.result, JSON.stringify(reply));
  return JSON.parse(reply.result.provider_context.messages.at(-1).User.content[0].Text);
};

// Synthetic RPC-host qualification only. Native append admission/history and
// native-App foreground UI acceptance are separately required release gates.
test('pre-dispatch snapshot refusal plus null history update does not itself retire the adapter owner', async t => {
  const peer = await started(t);
  peer.notify('context/updated', { resource_owner: owner, host: { ...unavailable, session_name: 'after-refusal' } });
  const observed = await inspect(peer);
  assert.match(observed.name, /native session view unavailable/);
  assert.match(observed.entries, /native session view unavailable/);
  peer.notify('context/updated', { resource_owner: owner, host: { session_entries: [], session_branch: [], session_leaf_id: null, session_file: null } });
  assert.equal((await inspect(peer)).name, 'after-refusal', 'a new ready snapshot restores reads without minting another owner');
  assert.equal(peer.seen.some(frame => frame.method === 'notification' && frame.params.title === 'Pi compatibility error'), false);
  assert.equal(peer.seen.some(frame => frame.method === 'session/append_entry'), false);
  await peer.close();
});

test('valid foreign context reads its own complete history without promoting its owner or acquiring UI', async t => {
  const peer = await started(t);
  const history = Array.from({ length: 40 }, (_, i) => ({ id: `entry-${i}`, parentId: i ? `entry-${i - 1}` : null,
    type: 'custom', customType: 'private-proof', data: { i } }));
  const observed = proof(await peer.request('hook/run', contextHook(foreign, false, { session_entries: history })).response);
  assert.deepEqual(observed.entries, history.map(entry => entry.id));
  assert.equal(observed.session, foreign.session_id);
  assert.equal(observed.hasUI, false);
  for (const key of ['chrome', 'notify', 'status', 'composer']) assert.match(observed[key], /not_foreground_owner/);
  assert.equal(peer.seen.some(frame => frame.method === 'ui/chrome' || frame.method === 'ui/close'), false);
  peer.notify('context/updated', { resource_owner: owner, host: { session_name: 'current-native-foreground' } });
  assert.equal((await inspect(peer)).name, 'current-native-foreground');
  peer.notify('session/info_changed', { resource_owner: owner });
  const chrome = await peer.wait(frame => frame.method === 'ui/chrome');
  assert.deepEqual(chrome.params.resource_owner, owner);
  peer.send({ jsonrpc: '2.0', id: chrome.id, result: { tools_expanded: false } });
  assert.equal((await inspect(peer)).name, 'current-native-foreground');
  const refused = await peer.request('command/execute', { name: 'inspect-owner', context: ownerContext(foreign) }).response;
  assert.equal(refused.error?.code, -32002);
  await peer.close();
});

test('foreign background append uses only its live native leaf, never foreground authority', async t => {
  const peer = await started(t, { hold: ['session/append_entry'] });
  const hook = contextHook(foreign, false, { session_name: 'append' });
  const pending = peer.request('hook/run', hook);
  const append = await peer.wait(frame => frame.method === 'session/append_entry');
  assert.equal(append.params.parent_request_id, pending.id);
  assert.deepEqual(append.params.resource_owner, foreign);
  assert.deepEqual(append.params.session_leaf, { grant_id: hook.session_leaf.grant_id, activation_epoch: 1, operation_id: 'synthetic-context:1' });
  peer.send({ jsonrpc: '2.0', id: append.id, result: { entry_id: 'background-committed', head: 'background-committed', successor: null } });
  assert.equal(proof(await pending.response).session, foreign.session_id);
  assert.equal((await inspect(peer)).entries, 0);
  assert.equal(peer.seen.some(frame => frame.method === 'ui/close'), false);
  await peer.close();
});

test('rejected foreign leaf/preparation leaves the original owner current and usable', async t => {
  const peer = await started(t);
  const invalids = [
    p => { p.payload.preparation.head = 'wrong-head'; },
    p => { delete p.payload.preparation.head; },
    p => { p.payload.preparation.resource_owner = owner.session_id; },
    p => { p.payload.preparation.session_id = ''; },
    p => { p.payload.preparation.session_id = 'different-session'; },
    p => { p.payload.preparation.tool_generation = -1; },
    p => { p.payload.preparation.tool_generation = 0.5; },
    p => { p.session_leaf.owner = owner; },
    p => { p.session_leaf.owner = { ...foreign, process_generation: 100 }; },
    p => { p.context.resource_owner = { session_id: 'incomplete' }; },
    p => { p.context.resource_owner.process_generation = -1; },
    p => { p.context.resource_owner = { ...foreign, process_generation: 100 }; p.session_leaf.owner = p.context.resource_owner; },
    p => { p.session_leaf.grant_id = 'malformed'; },
    p => { p.session_leaf.activation_epoch = -1; },
    p => { p.session_leaf.activation_epoch = 1.5; },
    p => { p.session_leaf.expected_head = ''; },
    p => { p.context.host.session_leaf_id = 'snapshot-mismatch'; },
    p => { delete p.session_leaf; },
  ];
  for (let index = 0; index < invalids.length; index++) {
    const params = structuredClone(contextHook(foreign)); invalids[index](params);
    const refused = await peer.request('hook/run', params).response;
    assert.ok(refused.error, `${index}: ${JSON.stringify(refused)}`);
    peer.notify('context/updated', { resource_owner: owner, host: { session_name: `current-${index}` } });
    assert.equal((await inspect(peer)).name, `current-${index}`);
  }
  assert.equal(peer.seen.some(frame => frame.method === 'ui/close' || frame.method === 'session/append_entry'), false);
  await peer.close();
});

test('concurrent background hook and reentrant original-owner commands do not switch owners', async t => {
  const peer = await started(t);
  const pending = peer.request('hook/run', contextHook(foreign, false, { session_name: 'wait' }));
  let waiting;
  for (let n = 0; n < 10; n++) { waiting = await inspect(peer); if (waiting.waiting) break; }
  assert.equal(waiting.waiting, true);
  const next = { ...foreign, session_id: 'second-background-owner' };
  const queued = peer.request('hook/run', contextHook(next));
  peer.notify('context/updated', { resource_owner: owner, host: { session_name: 'still-current' } });
  assert.equal((await inspect(peer)).name, 'still-current');
  assert.ok((await peer.request('command/execute', { name: 'release-owner', context: ownerContext(owner) }).response).result);
  assert.equal(proof(await pending.response).after, foreign.session_id);
  assert.equal(proof(await queued.response).session, next.session_id);
  assert.equal((await inspect(peer)).name, 'still-current');
  await peer.close();
});

test('background cancellation leaves the foreground usable and cannot publish a late reply', async t => {
  const peer = await started(t);
  const pending = peer.request('hook/run', contextHook(foreign, false, { session_name: 'wait' }));
  for (let n = 0; n < 10 && !(await inspect(peer)).waiting; n++) {}
  peer.notify('$/cancelRequest', { id: pending.id });
  let settled = false; pending.response.then(() => { settled = true; });
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(settled, false, 'cancellation must retain the active slot while the callback ignores abort');
  assert.ok((await peer.request('command/execute', { name: 'release-owner', context: ownerContext(owner) }).response).result);
  assert.equal((await pending.response).error?.code, -32800);
  assert.equal((await inspect(peer)).entries, 0);
  assert.equal(peer.seen.filter(frame => frame.id === pending.id && !frame.method).length, 1);
  await peer.close();
});

test('invalid foreign preparation cannot cancel a running foreground hook', async t => {
  const peer = await started(t);
  const pending = peer.request('hook/run', contextHook(owner, false, { session_name: 'wait' }));
  let observed;
  for (let n = 0; n < 10; n++) { observed = await inspect(peer); if (observed.waiting) break; }
  assert.equal(observed.waiting, true);
  assert.ok((await peer.request('hook/run', contextHook(foreign, true)).response).error);
  assert.ok((await peer.request('command/execute', { name: 'release-owner', context: ownerContext(owner) }).response).result);
  assert.equal(proof(await pending.response).after, owner.session_id);
  await peer.close();
});

test('background session_end retires only its own state and prevents hook reuse', async t => {
  const peer = await started(t);
  proof(await peer.request('hook/run', contextHook(foreign)).response);
  assert.ok((await peer.request('hook/run', { hook: 'session_end', payload: { binding: foreign }, context: ownerContext(foreign) }).response).result);
  assert.match((await peer.request('hook/run', contextHook(foreign)).response).error?.message, /settled owner/);
  peer.notify('context/updated', { resource_owner: owner, host: { session_name: 'foreground-intact' } });
  assert.equal((await inspect(peer)).name, 'foreground-intact');
  assert.equal(peer.seen.some(frame => frame.method === 'ui/close'), false);
  await peer.close();
});

test('legitimate session retirement still refuses stale context updates and commands', async t => {
  const peer = await started(t);
  const ended = await peer.request('hook/run', { hook: 'session_end', payload: { binding: owner }, context: ownerContext(owner) }).response;
  assert.ok(ended.result, JSON.stringify(ended));
  peer.notify('context/updated', { resource_owner: owner, host: { session_name: 'stale' } });
  await peer.wait(issue('not_foreground_owner context/updated'));
  const stale = await peer.request('command/execute', { name: 'inspect-owner', arguments: [], context: ownerContext(owner) }).response;
  assert.equal(stale.error?.code, -32002);
  assert.match(stale.error.message, /settled owner/);
  await peer.close();
});

// In-process invariants complement the real adapter subprocess cases above.
function runtimeFixture(t) {
  const runtime = new Runtime({ extensions: [] }, { closed: false, notify: async () => {} });
  t.after(() => runtime.uninstallChildren());
  runtime.features = new Set(['session_entries', 'remote_ui']);
  const store = method => ({ id: 1, method, controller: new AbortController(), pending: new Set(), errors: [], live: true });
  const start = store('hook/run'); start.hook = 'session_start';
  runtime.bind({ hook: 'session_start', context: ownerContext(owner, { session_name: 'foreground', session_entries: [] }) }, start);
  return { runtime, start, store };
}

test('invalid private binding preserves foreground identity, editor, timers, MCP and listeners exactly', async t => {
  const { runtime, start, store } = runtimeFixture(t), state = start.state;
  const listener = () => {}, timer = {}, editor = { store: start };
  const server = { name: 'foreground-server', command: 'synthetic-server', args: [], extensionPath: fixture };
  const mcp = new Map([[server.name, server]]);
  state.branchListeners.add(listener); state.piMcpServers = mcp;
  runtime.timers.handles.set(timer, { store: start, kind: 'Timeout' });
  runtime.ui.surfaces.set('editor', editor);
  runtime.events.set('context', [{ factory: 0, handler: () => assert.fail('invalid preparation ran callback') }]);
  t.after(() => { runtime.timers.handles.clear(); runtime.ui.surfaces.clear(); });
  for (const who of [foreign, owner]) {
    const snapshot = { ...state.host };
    await assert.rejects(projectContext(runtime, contextHook(who, true, { session_name: 'must-not-mutate' }), store('hook/run')), /matching native session_leaf/);
    assert.equal(runtime.foreground, state); assert.equal(state.alive, true);
    assert.deepEqual(state.host, snapshot); assert.equal(runtime.states.size, 1);
    assert.equal(runtime.ui.surfaces.get('editor'), editor); assert.equal(runtime.timers.handles.get(timer).store, start);
    assert.equal(state.piMcpServers, mcp); assert.deepEqual([...state.piMcpServers], [[server.name, server]]);
    assert.deepEqual([...state.branchListeners], [listener]);
  }
});

test('all private hook/tool binding callers are session-scoped; only start/replacement transitions retire foreground', async t => {
  const { runtime, start, store } = runtimeFixture(t), state = start.state;
  for (const hook of ['resources_discover', 'before_provider_request', 'session_before_switch', 'session_before_compact', 'model_turn_start', 'before_prompt', 'before_tool_call']) {
    const scoped = store('hook/run'); runtime.bind({ hook, context: ownerContext(foreign) }, scoped);
    assert.equal(runtime.foreground, state); assert.equal(state.alive, true);
    assert.equal(createContext(runtime, scoped).hasUI, false);
    assert.throws(() => createContext(runtime, scoped).ui.notify('background'), /not_foreground_owner/);
  }
  for (const method of ['tool/call', 'tool/prepare_arguments', 'transcript/render']) {
    runtime.bind({ context: ownerContext(foreign) }, store(method));
    assert.equal(runtime.foreground, state); assert.equal(state.alive, true);
  }
  const next = { ...foreign, session_id: 'legitimate-replacement' };
  runtime.bind({ hook: 'session_start', context: ownerContext(next) }, store('hook/run'));
  assert.equal(state.alive, false); assert.deepEqual(runtime.foreground.owner, next);
  assert.throws(() => runtime.bind({ context: ownerContext(owner) }, store('command/execute')), /settled owner/);
  // Start can revisit an owner, but captured old contexts never revive.
  runtime.bind({ hook: 'session_start', context: ownerContext(owner) }, store('hook/run'));
  assert.notEqual(runtime.foreground, state); assert.equal(state.alive, false);
  assert.equal(runtime.states.get(ownerKey(owner)), runtime.foreground);
  await runtime.retire(runtime.foreground);
  assert.equal(runtime.foreground.alive, false);
});

test('a first private hook never mints a foreground lease before session_start', async t => {
  const runtime = new Runtime({ extensions: [] }, { closed: false }); t.after(() => runtime.uninstallChildren());
  const store = { id: 1, method: 'hook/run', controller: new AbortController() };
  runtime.bind(contextHook(foreign), store);
  assert.equal(runtime.foreground, null);
  assert.throws(() => runtime.assertOwner(store), /not_foreground_owner/);
  runtime.assertSessionOwner(store);
});
