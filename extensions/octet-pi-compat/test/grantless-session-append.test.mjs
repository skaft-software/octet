import assert from 'node:assert/strict';
import test from 'node:test';
import { Runtime } from '../lib/runtime.mjs';
import { SessionTransport } from '../lib/session-transport.mjs';
import { createContext } from '../lib/api.mjs';
import { ownerKey } from '../lib/errors.mjs';
import { owner } from './helper.mjs';

const profile = { profile: 'json-chunks.v1', chunk_bytes: 65536, snapshot_bytes: 1048576,
  generation_bytes: 2097152, owner_views: 64, transfers: 64, view_entries: 256,
  generation_entries: 512, projection_bytes: 1048576, projections: 64 };

function fixture(mutate = value => value) {
  const namespace = 'octet-pi-compat';
  const runtime = Object.create(Runtime.prototype);
  const state = { key: ownerKey(owner), owner, alive: true, workspace: '/native',
    host: { session_entries: [], session_branch: [], session_leaf_id: null } };
  const store = { id: 7, live: true, method: 'command/execute', state, controller: new AbortController() };
  Object.assign(runtime, { namespace, features: new Set(['session_entries', 'session_append_receipts_v1']), active: new Map([[7, store]]) });
  const history = { entries: [], branch_ids: [], head: null, file: '/native/session.jsonl', header: {}, labels: {} };
  const descriptor = { owner, view_revision: 1, head: null, bytes: Buffer.byteLength(JSON.stringify(history)), entry_count: 0, branch_count: 0 };
  runtime.sessionTransport = new SessionTransport(runtime, profile, 1048576);
  const view = { descriptor, document: history, refs: 1, release: runtime.sessionTransport.reserve(descriptor) };
  runtime.sessionTransport.history(view);
  store.sessionView = view;
  const failures = [], calls = [];
  runtime.transport = {
    fail: error => failures.push(error),
    requestSync(method, params) {
      assert.equal(method, 'session/append_entry');
      assert.equal(params.parent_request_id, 7);
      assert.deepEqual(params.resource_owner, owner);
      assert.equal(params.session_leaf, undefined);
      calls.push(params);
      const id = `native-${calls.length}`;
      return mutate({ entry_id: id, head: id, successor: null,
        previous_revision: view.descriptor.view_revision, view_revision: view.descriptor.view_revision + 1,
        previous_head: view.descriptor.head,
        entry: { id, parent: view.descriptor.head, timestamp_unix_ms: 1700000000000,
          value: { type: 'config', model: null, reasoning: null, reasoning_mode: null },
          metadata: { extension_metadata: { [namespace]: { public: false,
            provenance: { extension: namespace, process_generation: owner.process_generation },
            value: { entry_type: params.entry_type, data: params.data } } } } } });
    },
  };
  return { runtime, store, state, view, failures, calls, context: createContext(runtime, store) };
}

test('grant-less routed durable appends immediately update the actual pinned context view', () => {
  const { runtime, store, state, view, context, failures } = fixture();
  assert.equal(runtime.appendEntry('first', { saved: 1 }, store), undefined);
  const first = context.sessionManager.getEntries()[0];
  assert.deepEqual(first, { id: 'native-1', parentId: null, timestamp: '2023-11-14T22:13:20.000Z',
    type: 'custom', customType: 'first', data: { saved: 1 } });
  assert.equal(context.sessionManager.getLeafId(), first.id);
  assert.equal(runtime.appendEntry('second', { saved: 2 }, store), undefined);
  assert.equal(context.sessionManager.getEntry('native-2').parentId, first.id);
  assert.equal(context.sessionManager.getBranch().length, 2);
  assert.equal(view.descriptor.view_revision, 3);
  assert.deepEqual(state.host.session_entries, []);
  assert.deepEqual(failures, []);
});

test('grant-less routed appends never accept an unbound legacy ACK or mint a successor grant', () => {
  for (const mutate of [value => ({ entry_id: value.entry_id }), value => ({ ...value, successor: {} })]) {
    const { runtime, store, view, failures } = fixture(mutate);
    assert.throws(() => runtime.appendEntry('private', {}, store), /outcome unknown/);
    assert.equal(view.document.entries.length, 0);
    assert.equal(failures.length, 1);
  }
});

test('legacy grant-less ACKs remain exact and do not acquire native receipt authority', () => {
  const { runtime, store, state, view } = fixture(value => ({ entry_id: value.entry_id }));
  runtime.features.delete('session_append_receipts_v1');
  assert.equal(runtime.appendEntry('legacy', {}, store), undefined);
  assert.equal(state.host.session_entries[0].id, 'native-1');
  assert.equal(view.document.entries.length, 0);
});

test('grant-less routed receipt revision, ancestry and quota failures cannot mutate the pinned view', () => {
  for (const mutate of [value => ({ ...value, previous_revision: 99 }),
    value => ({ ...value, previous_head: 'foreign' }),
    value => ({ ...value, entry: { ...value.entry, parent: 'foreign' } })]) {
    const { runtime, store, view } = fixture(mutate);
    assert.throws(() => runtime.appendEntry('private', {}, store), /outcome unknown/);
    assert.equal(view.document.entries.length, 0);
  }
  const { runtime, store, view } = fixture();
  runtime.sessionTransport.profile = { ...profile, view_entries: 0 };
  assert.throws(() => runtime.appendEntry('private', {}, store), /outcome unknown/);
  assert.equal(view.document.entries.length, 0);
});

test('grant-less routed appends retain cancellation, owner and live-parent fences', () => {
  for (const revoke of [({ store }) => store.controller.abort(),
    ({ store }) => { store.state.alive = false; }, ({ store }) => { store.live = false; }]) {
    const rig = fixture();
    revoke(rig);
    assert.throws(() => rig.runtime.appendEntry('private', {}, rig.store));
    assert.equal(rig.calls.length, 0);
    assert.equal(rig.view.document.entries.length, 0);
  }
});
