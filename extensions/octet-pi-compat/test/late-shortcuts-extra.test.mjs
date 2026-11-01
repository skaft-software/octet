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

test('two late shortcuts keep unique stable native IDs and dispatch through replaced handlers', async t => {
  const h = harness(t), seen = [];
  h.runtime.transport.requestSync = (method, params, options) => {
    h.calls.push({ method, params, options });
    return {};
  };
  h.run(() => h.api.registerShortcut('ctrl+alt+f', { description: 'Fleet overlay', handler() { seen.push('fleet:old'); } }));
  h.run(() => h.api.registerShortcut('ctrl+alt+r', { description: 'Roster', handler() { seen.push('roster'); } }));
  assert.deepEqual(h.calls.map(call => [call.method, call.params.id, call.params.key, call.params.description, call.options.parent]), [
    ['shortcut/register', 'pi-runtime-shortcut-1', 'ctrl+alt+f', 'Fleet overlay', 7],
    ['shortcut/register', 'pi-runtime-shortcut-2', 'ctrl+alt+r', 'Roster', 7],
  ]);
  assert.deepEqual(h.runtime.metadata().shortcuts.map(shortcut => shortcut.name), ['pi-shortcut-0', 'pi-runtime-shortcut-1', 'pi-runtime-shortcut-2']);
  h.run(() => h.api.registerShortcut('ctrl+alt+f', { description: 'Fleet overlay', handler() { seen.push('fleet:new'); } }));
  assert.equal(h.calls.length, 2);
  assert.deepEqual(h.runtime.metadata().shortcuts.map(shortcut => shortcut.name), ['pi-shortcut-0', 'pi-runtime-shortcut-1', 'pi-runtime-shortcut-2']);
  await h.runtime.notify({ method: 'shortcut/trigger', params: { id: 'pi-runtime-shortcut-1', resource_owner: owner } });
  await h.runtime.notify({ method: 'shortcut/trigger', params: { id: 'pi-runtime-shortcut-2', resource_owner: owner } });
  assert.deepEqual(seen, ['fleet:new', 'roster']);
});

test('another factory cannot replace a published late shortcut', async t => {
  const h = harness(t);
  let original = 0, intruder = 0;
  h.runtime.transport.requestSync = (method, params, options) => {
    h.calls.push({ method, params, options });
    return {};
  };
  h.run(() => h.api.registerShortcut('ctrl+alt+f', { description: 'Fleet overlay', handler() { original++; } }));
  const published = h.runtime.shortcuts.get('ctrl+alt+f'), metadata = h.runtime.metadata().shortcuts;
  assert.throws(() => h.run(() => h.other.registerShortcut('ctrl+alt+f', { description: 'Fleet overlay', handler() { intruder++; } })), /belongs to another factory/);
  assert.equal(h.calls.length, 1);
  assert.equal(h.runtime.shortcuts.get('ctrl+alt+f'), published);
  assert.deepEqual(h.runtime.metadata().shortcuts, metadata);
  await h.runtime.notify({ method: 'shortcut/trigger', params: { id: 'pi-runtime-shortcut-1', resource_owner: owner } });
  assert.equal(original, 1);
  assert.equal(intruder, 0);
});

test('native rejection and malformed shortcut acknowledgements publish nothing', t => {
  const cases = [
    [() => { throw Error('native key conflict'); }, /native key conflict/],
    [() => null, /shortcut registration acknowledgement/],
    [() => [], /shortcut registration acknowledgement/],
    [() => ({ accepted: true }), /shortcut registration acknowledgement/],
  ];
  for (const [requestSync, expected] of cases) {
    const h = harness(t);
    h.runtime.transport.requestSync = requestSync;
    assert.throws(() => h.run(() => h.api.registerShortcut('ctrl+alt+f', { description: 'Fleet overlay', handler() {} })), expected);
    assert.equal(h.runtime.shortcuts.has('ctrl+alt+f'), false);
    assert.deepEqual(h.runtime.metadata().shortcuts.map(shortcut => shortcut.name), ['pi-shortcut-0']);
    assert.equal(h.runtime.metadata().shortcuts.some(shortcut => shortcut.key === 'ctrl+alt+f'), false);
  }
});
