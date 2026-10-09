// Receipt continuations retain an admitted command, never the old session's
// general authority. Native App tests separately qualify lifecycle/persistence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { ownerKey } from '../lib/errors.mjs';
import { owner, host } from './helper.mjs';

function fixture(t) {
  const runtime = new Runtime({ extensions: [] }, {});
  t.after(() => runtime.uninstallChildren());
  const store = { id: 7, method: 'command/execute', live: true, controller: new AbortController() };
  runtime.active.set(store.id, store);
  runtime.bind({ context: { resource_owner: owner, host } }, store);
  const context = { resource_owner: { ...owner, session_id: 'replacement-owner' }, host: { ...host, session_id: 'replacement-id' } };
  return { runtime, store, context };
}

test('setup receipt survives old-session retirement without reviving captured contexts', async t => {
  const { runtime, store, context } = fixture(t);
  const bind = runtime.prepareSessionReplacement(store);
  await runtime.retire(store.state);
  assert.throws(() => runtime.assertOwner(store), /retained context is unavailable/);
  assert.throws(() => runtime.prepareSessionReplacement(store), /retained context is unavailable/);
  const fresh = bind(context, 'replacement-id');
  assert.equal(fresh.controller, store.controller);
  assert.equal(fresh.id, store.id);
  runtime.assertOwner(fresh);
  assert.throws(() => runtime.assertOwner(store), /retained context is unavailable/);
  assert.throws(() => bind(context, 'replacement-id'), /continuation retired/);
});

for (const change of ['settled', 'reused-id', 'cancelled', 'shutdown', 'other-foreground']) {
  test(`setup receipt refuses ${change} command continuation`, async t => {
    const { runtime, store, context } = fixture(t);
    const bind = runtime.prepareSessionReplacement(store);
    await runtime.retire(store.state);
    if (change === 'settled') runtime.active.delete(store.id);
    if (change === 'reused-id') runtime.active.set(store.id, { ...store, controller: new AbortController() });
    if (change === 'cancelled') store.controller.abort(new Error('cancelled'));
    if (change === 'shutdown') runtime.stopping = true;
    if (change === 'other-foreground') runtime.bind({ context: { ...context, resource_owner: { ...owner, session_id: 'intervening-owner' } } }, { method: 'hook/run', id: 8 }, { foreground: true });
    const foreground = runtime.foreground;
    assert.throws(() => bind(context, 'replacement-id'), /not_foreground_owner|cancelled/);
    assert.equal(runtime.foreground, foreground);
    assert.equal(runtime.states.has(ownerKey(context.resource_owner)), false);
  });
}

for (const change of ['session', 'instance', 'generation', 'display-id']) {
  test(`setup receipt validates ${change} before publishing a replacement`, async t => {
    const { runtime, store, context } = fixture(t);
    const bind = runtime.prepareSessionReplacement(store);
    await runtime.retire(store.state);
    if (change === 'session') context.resource_owner.session_id = owner.session_id;
    if (change === 'instance') context.resource_owner.extension_instance_id = 'foreign-instance';
    if (change === 'generation') context.resource_owner.process_generation++;
    if (change === 'display-id') context.host.session_id = 'wrong-session';
    assert.throws(() => bind(context, 'replacement-id'), /replacement receipt owner|setup replacement session id/);
    assert.equal(runtime.foreground, store.state);
    assert.throws(() => bind(context, 'replacement-id'), /continuation retired/);
  });
}

test('a settled or noncommand context cannot prepare a replacement continuation', t => {
  const { runtime, store } = fixture(t);
  for (const method of ['hook/run', 'tool/call', 'shortcut/execute']) {
    assert.throws(() => runtime.prepareSessionReplacement({ ...store, method }), /requires its live command/);
  }
  assert.throws(() => runtime.prepareSessionReplacement({ ...store, live: false }), /requires its live command/);
  runtime.active.delete(store.id);
  assert.throws(() => runtime.prepareSessionReplacement(store), /requires its live command/);
});
