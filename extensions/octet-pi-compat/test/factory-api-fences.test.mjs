import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createAPI, createContext } from '../lib/api.mjs';
import { owner } from './helper.mjs';

test('captured API functions and event bus cannot revive a failed factory', t => {
  const runtime = new Runtime({ extensions: [] }, {}), api = createAPI(runtime, 0);
  t.after(() => runtime.uninstallChildren());
  const register = api.registerCommand, emit = api.events.emit;
  register('old', { handler() {} });
  const object = { changed: false }, control = createAPI(runtime, 1);
  control.events.on('identity', value => { assert.equal(value, object); value.changed = true; });
  control.events.emit('identity', object); assert.equal(object.changed, true);
  runtime.forgetFactory(0, '/reviewed/failed.ts');
  assert.throws(() => register('resurrected', { handler() {} }), /failed extension factory/);
  assert.throws(() => emit('identity', object), /failed extension factory/);
  assert.throws(() => api.on('session_start', () => {}), /failed extension factory/);
  assert.equal(runtime.commands.has('old'), false); assert.equal(runtime.commands.has('resurrected'), false);
});

test('late command callback replacement is real; changed native metadata or new names refuse before mutation', t => {
  const runtime = new Runtime({ extensions: [] }, {}), api = createAPI(runtime, 0);
  t.after(() => runtime.uninstallChildren());
  const first = { handler() {}, description: 'reviewed' };
  api.registerCommand('same', first); runtime.loaded = true;
  const replacement = { handler() {}, description: 'reviewed' };
  api.registerCommand('same', replacement); assert.equal(runtime.commands.get('same').definition, replacement);
  assert.throws(() => api.registerCommand('new', { handler() {} }), /no dynamic registration protocol/);
  assert.throws(() => api.registerCommand('same', { handler() {}, description: 'not published' }), /metadata is immutable/);
  assert.equal(runtime.commands.get('same').definition, replacement);
  assert.throws(() => createAPI(runtime, 1).registerCommand('same', replacement), /another factory/);
});

test('readonly session facade exposes complete branch/tree/projection getters without live writes', t => {
  const runtime = new Runtime({ extensions: [] }, {});
  t.after(() => runtime.uninstallChildren());
  const entries = [{ id: 'root', parentId: null, type: 'custom', customType: 'state', data: 1, timestamp: '2026-01-01T00:00:00Z' },
    { id: 'later', parentId: 'root', type: 'message', message: { role: 'user', content: 'hi' }, timestamp: '2026-01-02T00:00:00Z' }];
  const state = { owner, alive: true, workspace: '/native-workspace', host: { session_entries: entries, session_branch: entries, session_leaf_id: 'later', session_labels: { root: 'bookmark' } } };
  const store = { id: 1, state, controller: new AbortController() }; runtime.foreground = state;
  const manager = createContext(runtime, store).sessionManager;
  assert.equal(manager.getCwd(), '/native-workspace'); assert.equal(manager.getHeader(), null);
  assert.equal(manager.getLeafEntry().id, 'later'); assert.equal(manager.getLabel('root'), 'bookmark');
  assert.deepEqual(manager.getBranch('root').map(e => e.id), ['root']); assert.deepEqual(manager.getBranch('missing'), []);
  assert.deepEqual(manager.getTree().map(node => [node.entry.id, node.label, node.children[0].entry.id]), [['root', 'bookmark', 'later']]);
  assert.deepEqual(manager.buildContextEntries().map(e => e.id), ['root', 'later']);
  assert.deepEqual(manager.buildSessionProjection().messages, [{ role: 'user', content: 'hi' }]);
  assert.throws(() => manager.appendCustomEntry('illegal', {}), /unsupported_feature/);
});
