import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createAPI } from '../lib/api.mjs';
import { owner } from './helper.mjs';

function harness(t) {
  const calls = [], runtime = new Runtime({ extensions: ['/factory.ts', '/other.ts'] }, {
    requestSync(method, params, options) { calls.push({ method, params, options }); return {}; },
  });
  runtime.uninstallChildren();
  runtime.features = new Set(['shortcuts']);
  const store = { id: 7, live: true, method: 'hook/run', hook: 'session_start', controller: new AbortController(), pending: new Set(), errors: [] };
  runtime.bind({ context: { resource_owner: owner, workspace: '/work', host: {} } }, store, { foreground: true });
  runtime.active.set(store.id, store);
  const api = createAPI(runtime, 0), other = createAPI(runtime, 1);
  api.registerShortcut('ctrl+x', { description: 'Static shortcut', handler() {} });
  runtime.loaded = true;
  t.after(() => runtime.active.clear());
  return { runtime, store, api, other, calls, run: fn => runtime.scope.run(store, fn) };
}

test('late hook shortcut installs its handler and native trigger ID only after exact native ACK', async t => {
  const h = harness(t);
  let invoked = 0;
  h.runtime.transport.requestSync = (method, params, options) => {
    assert.equal(h.runtime.shortcuts.has('ctrl+alt+f'), false);
    h.calls.push({ method, params, options });
    return {};
  };
  h.run(() => h.api.registerShortcut('ctrl+alt+f', { description: 'Fleet overlay', handler() { invoked++; } }));
  assert.deepEqual(h.calls[0].params, { parent_request_id: 7, resource_owner: owner, id: 'pi-runtime-shortcut-1', key: 'ctrl+alt+f', description: 'Fleet overlay' });
  assert.equal(h.calls[0].method, 'shortcut/register');
  assert.equal(h.calls[0].options.parent, 7);
  assert.equal(h.calls[0].options.signal, h.store.controller.signal);
  assert.deepEqual(h.runtime.metadata().shortcuts.map(value => value.name), ['pi-shortcut-0', 'pi-runtime-shortcut-1']);
  await h.runtime.notify({ method: 'shortcut/trigger', params: { id: 'pi-runtime-shortcut-1', resource_owner: owner } });
  assert.equal(invoked, 1);
  h.run(() => h.api.registerShortcut('ctrl+alt+f', { description: 'Fleet overlay', handler() { invoked += 10; } }));
  assert.equal(h.calls.length, 1);
  await h.runtime.notify({ method: 'shortcut/trigger', params: { id: 'pi-runtime-shortcut-1', resource_owner: owner } });
  assert.equal(invoked, 11);
});

test('late shortcut refuses missing feature, native conflict, malformed ACK and reserved chords without publishing', t => {
  const h = harness(t);
  h.runtime.features.clear();
  assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+alt+f', { handler() {} })), /unsupported_feature shortcuts/);
  h.runtime.features.add('shortcuts');
  h.runtime.transport.requestSync = () => { throw Error('native key conflict'); };
  assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+alt+f', { handler() {} })), /native key conflict/);
  h.runtime.transport.requestSync = () => ({ accepted: true });
  assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+alt+f', { handler() {} })), /shortcut registration acknowledgement/);
  for (const key of ['ctrl+alt+d', 'ctrl+g']) assert.throws(() => h.run(() => h.api.registerShortcut(key, { handler() {} })), /reserved host rescue/);
  assert.equal(h.runtime.shortcuts.has('ctrl+alt+f'), false);
  assert.deepEqual(h.runtime.metadata().shortcuts.map(value => value.name), ['pi-shortcut-0']);
});

test('shortcut replacement preserves factory ownership and native metadata', t => {
  const h = harness(t), previous = h.runtime.shortcuts.get('ctrl+x');
  assert.throws(() => h.run(() => h.other.registerShortcut('ctrl+x', { description: 'Static shortcut', handler() {} })), /belongs to another factory/);
  assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+x', { description: 'changed', handler() {} })), /native catalog metadata is immutable/);
  assert.equal(h.runtime.shortcuts.get('ctrl+x'), previous);
  assert.deepEqual(h.calls, []);
});

test('late shortcut registration fences retired owners, cancellation and draining', t => {
  const h = harness(t);
  h.store.state.alive = false;
  assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+alt+f', { handler() {} })), /not_foreground_owner/);
  h.store.state.alive = true; h.runtime.stopping = true;
  assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+alt+f', { handler() {} })), /host is draining/);
  h.runtime.stopping = false; h.store.controller.abort(Error('cancelled origin'));
  assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+alt+f', { handler() {} })), /cancelled origin/);
  assert.deepEqual(h.calls, []);
});

test('retained shortcut registration carries exact owner without a stale active parent', t => {
  const h = harness(t);
  h.store.live = false; h.runtime.active.clear();
  h.run(() => h.api.registerShortcut('ctrl+alt+f', { handler() {} }));
  assert.equal(h.calls[0].options.parent, undefined);
  assert.deepEqual(h.calls[0].params.resource_owner, owner);
});
