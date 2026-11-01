import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { SessionTransport } from '../lib/session-transport.mjs';
import { createContext } from '../lib/api.mjs';
import { owner } from './helper.mjs';

const profile = { profile: 'json-chunks.v1', chunk_bytes: 65536, snapshot_bytes: 1048576,
  generation_bytes: 2097152, owner_views: 64, transfers: 64, view_entries: 256,
  generation_entries: 512, projection_bytes: 1048576, projections: 64 };

function fixture(t) {
  const runtime = new Runtime({ extensions: [] }, {});
  runtime.uninstallChildren();
  runtime.features = new Set(['session_entries', 'session_owner_routes_v1', 'session_snapshot_transport_v1']);
  runtime.sessionTransport = new SessionTransport(runtime, profile, 1048576);
  const store = { id: 7, method: 'hook/run', controller: new AbortController() };
  runtime.bind({ hook: 'session_start', context: { resource_owner: owner, host: { has_pending_messages: false } } }, store, { foreground: true });
  t.after(() => runtime.active.clear());
  const context = createContext(runtime, store);
  return { runtime, store, context, update: host => runtime.updated({ resource_owner: owner, host }) };
}

test('paired queue-only publications update retained context without remote UI or a mount', t => {
  const { runtime, store, context, update } = fixture(t);
  assert.equal(runtime.features.has('remote_ui'), false);
  runtime.ui = { get surfaces() { assert.fail('queue facts cannot acquire a UI surface'); } };
  const before = store.state.host;
  assert.equal(context.hasPendingMessages(), false);
  update({ has_pending_messages: true });
  assert.equal(context.hasPendingMessages(), true);
  update({ has_pending_messages: false });
  assert.equal(context.hasPendingMessages(), false);
  assert.deepEqual(store.state.host, before);
  assert.equal(runtime.sessionTransport.current.size, 0);
});

test('queue-only reads do not change foreground or grant former contexts foreground actions', t => {
  const { runtime, store, context, update } = fixture(t);
  const foreground = { owner: { ...owner, session_id: 'other-live-session' }, alive: true, host: {} };
  runtime.foreground = foreground;
  update({ has_pending_messages: true });
  assert.equal(context.hasPendingMessages(), true);
  assert.equal(runtime.foreground, foreground);
  assert.throws(() => runtime.assertOwner(store), /inactive session/);
});

test('queue-only publications require an existing live exact owner and paired transport', t => {
  const { runtime, store, context, update } = fixture(t);
  const states = runtime.states.size;
  for (const resource_owner of [{ ...owner, session_id: 'unknown' },
    { ...owner, process_generation: owner.process_generation + 1 },
    { ...owner, extension_instance_id: 'foreign-instance' }]) {
    assert.doesNotThrow(() => runtime.updated({ resource_owner, host: { has_pending_messages: true } }));
    assert.equal(runtime.states.size, states);
    assert.equal(context.hasPendingMessages(), false);
  }
  store.state.alive = false;
  assert.doesNotThrow(() => update({ has_pending_messages: true }));
  assert.equal(store.state.host.has_pending_messages, false);
  assert.throws(() => context.hasPendingMessages(), /not_foreground_owner retained context is unavailable/);
  store.state.alive = true;
  runtime.sessionTransport = undefined;
  assert.throws(() => update({ has_pending_messages: true }), /remote_ui/);
  assert.equal(context.hasPendingMessages(), false);
});

test('queue authority never admits mixed host data, missing facts or non-boolean substitutes', t => {
  const { runtime, context, update } = fixture(t);
  for (const host of [{}, { has_pending_messages: null }, { has_pending_messages: 0 },
    { has_pending_messages: 'false' }, { has_pending_messages: true, has_ui: true },
    { has_pending_messages: true, session_entries: [] }]) {
    assert.throws(() => update(host));
    assert.equal(context.hasPendingMessages(), false);
  }
  update({ has_pending_messages: true, session_view_revision: 2 });
  assert.equal(context.hasPendingMessages(), false);
  assert.equal(runtime.sessionTransport.current.size, 1);
});
