import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { Runtime } from '../lib/runtime.mjs';
import { SessionTransport } from '../lib/session-transport.mjs';
import { createContext } from '../lib/api.mjs';
import { ownerKey } from '../lib/errors.mjs';

const owner = { session_id: 'queue-owner', extension_instance_id: 'queue-instance', process_generation: 1 };
const hash = value => createHash('sha256').update(value).digest('hex');
const profile = { profile: 'json-chunks.v1', chunk_bytes: 65536, snapshot_bytes: 1048576,
  generation_bytes: 2097152, owner_views: 8, transfers: 8, view_entries: 1024,
  generation_entries: 2048, projection_bytes: 1048576, projections: 8 };

function fixture() {
  const runtime = Object.assign(Object.create(Runtime.prototype), {
    states: new Map(), initialHost: {}, workspace: '/missing-native-queue-workspace',
    active: new Map(), features: new Set(['remote_ui', 'session_entries', 'session_owner_routes_v1', 'session_snapshot_transport_v1']),
    ui: { surfaces: new Map() },
  });
  const documents = new Map();
  const calls = [];
  runtime.transport = { request: async (method, params) => {
    calls.push(method);
    if (method === 'session/snapshot/release') return { released: true };
    assert.equal(method, 'session/snapshot/read');
    const bytes = documents.get(params.transfer_id);
    const data = bytes.subarray(params.offset, params.offset + params.max_bytes);
    return { transfer_id: params.transfer_id, offset: params.offset, data: data.toString('base64'),
      next_offset: params.offset + data.length, eof: params.offset + data.length === bytes.length };
  } };
  runtime.sessionTransport = new SessionTransport(runtime, profile, 1048576);
  let next = 0;
  const prepare = (revision, host, resourceOwner = owner) => {
    const entry = { id: `entry-${revision}`, parent: null, timestamp_unix_ms: revision,
      value: { type: 'message', User: { content: [{ Text: `history-${revision}` }] } } };
    const document = { entries: [entry], branch_ids: [entry.id], head: entry.id,
      file: '/native/session.jsonl', header: {}, labels: {} };
    const bytes = Buffer.from(JSON.stringify(document));
    const transferId = hash(`${ownerKey(resourceOwner)}:${revision}`);
    documents.set(transferId, bytes);
    const store = { id: ++next, method: 'session/snapshot/prepare', live: true, controller: new AbortController() };
    runtime.active.set(store.id, store);
    const params = { resource_owner: resourceOwner, host, snapshot: { transfer_id: transferId, kind: 'history',
      owner: resourceOwner, view_revision: revision, head: entry.id, bytes: bytes.length,
      sha256: hash(bytes), entry_count: 1, branch_count: 1, preparation: null } };
    return { store, params, run: () => runtime.sessionTransport.prepare(params, store) };
  };
  return { runtime, prepare, calls };
}

for (const actual of [false, true]) {
  test(`older history preparation preserves the actual ${actual} queue and newer non-history facts`, async () => {
    const { runtime, prepare } = fixture();
    const first = prepare(1, { has_pending_messages: !actual, model: 'old-model' });
    await first.run();
    runtime.foreground = first.store.state;
    const retained = createContext(runtime, first.store);
    const delayed = prepare(2, { has_pending_messages: !actual, model: 'old-model' });
    runtime.updated({ resource_owner: owner, host: { has_pending_messages: actual, model: 'new-model' } });
    assert.equal(retained.hasPendingMessages(), actual);
    await delayed.run();
    assert.equal(retained.hasPendingMessages(), actual);
    assert.equal(createContext(runtime, delayed.store).hasPendingMessages(), actual);
    assert.equal(first.store.state.host.model, 'new-model');
    assert.equal(runtime.sessionTransport.host(delayed.store).session_leaf_id, 'entry-2');
    assert.equal(runtime.sessionTransport.current.get(ownerKey(owner)).revision, 2);
  });
}

test('stale history refusal cannot mutate current host facts or read a transfer', async () => {
  const { runtime, prepare, calls } = fixture();
  const first = prepare(2, { has_pending_messages: true, model: 'new-model' });
  await first.run();
  const count = calls.length;
  await assert.rejects(prepare(1, { has_pending_messages: false, model: 'old-model' }).run(), /session view unavailable/);
  assert.equal(calls.length, count);
  assert.equal(first.store.state.host.has_pending_messages, true);
  assert.equal(first.store.state.host.model, 'new-model');
  assert.equal(runtime.sessionTransport.current.get(ownerKey(owner)).revision, 2);
});

test('missing pending-message observations stay unsupported across preparations', async () => {
  const { runtime, prepare } = fixture();
  const first = prepare(1, {});
  await first.run();
  const context = createContext(runtime, first.store);
  assert.throws(() => context.hasPendingMessages(), /snapshot not supplied by the host/);
  await prepare(2, {}).run();
  assert.throws(() => context.hasPendingMessages(), /snapshot not supplied by the host/);
  assert.equal(Object.hasOwn(first.store.state.host, 'has_pending_messages'), false);
});

test('a new exact owner seeds only its supplied observations without borrowing foreground facts', async () => {
  const { runtime, prepare } = fixture();
  const first = prepare(1, { has_pending_messages: true });
  await first.run();
  runtime.foreground = first.store.state;
  const next = prepare(1, {}, { ...owner, session_id: 'other-queue-owner' });
  await next.run();
  assert.equal(runtime.foreground, first.store.state);
  assert.equal(createContext(runtime, first.store).hasPendingMessages(), true);
  assert.throws(() => createContext(runtime, next.store).hasPendingMessages(), /snapshot not supplied by the host/);
  const supplied = prepare(2, { has_pending_messages: false }, next.params.resource_owner);
  await supplied.run();
  assert.equal(createContext(runtime, supplied.store).hasPendingMessages(), false);
});

test('preparing history cannot resurrect a retired owner or replace its facts', async () => {
  const { runtime, prepare, calls } = fixture();
  const first = prepare(1, { has_pending_messages: true });
  await first.run();
  first.store.state.alive = false;
  const count = calls.length;
  await assert.rejects(prepare(2, { has_pending_messages: false }).run(), /settled owner/);
  assert.equal(calls.length, count);
  assert.equal(first.store.state.alive, false);
  assert.equal(first.store.state.host.has_pending_messages, true);
});

test('descriptor owner and generation mismatches refuse before changing host facts', async () => {
  const { runtime, prepare, calls } = fixture();
  const first = prepare(1, { has_pending_messages: true });
  await first.run();
  const count = calls.length;
  for (const changed of [{ session_id: 'wrong-owner' }, { process_generation: 2 }]) {
    const next = prepare(2, { has_pending_messages: false });
    next.params.snapshot.owner = { ...owner, ...changed };
    await assert.rejects(next.run(), /session descriptor identity/);
  }
  runtime.foreground = first.store.state;
  for (const changed of [{ extension_instance_id: 'wrong-instance' }, { process_generation: 2 }]) {
    const foreign = prepare(2, { has_pending_messages: false }, { ...owner, ...changed });
    await assert.rejects(foreign.run(), /process binding/);
    assert.equal(runtime.states.has(ownerKey(foreign.params.resource_owner)), false);
  }
  assert.equal(calls.length, count);
  assert.equal(first.store.state.host.has_pending_messages, true);
});
