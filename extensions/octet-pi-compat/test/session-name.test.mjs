import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createHash } from 'node:crypto';
import { Runtime } from '../lib/runtime.mjs';
import { Transport } from '../lib/transport.mjs';
import { SessionTransport } from '../lib/session-transport.mjs';
import { createAPI } from '../lib/api.mjs';

const owner = { session_id: 'session-owner', extension_instance_id: 'instance', process_generation: 1 };
const profile = { profile: 'json-chunks.v1', chunk_bytes: 65536, snapshot_bytes: 268435456,
  generation_bytes: 536870912, owner_views: 64, transfers: 64, view_entries: 1048576,
  generation_entries: 2097152, projection_bytes: 67108864, projections: 64 };
const tick = () => new Promise(resolve => setImmediate(resolve));

// Real API, request correlation, owner checks and snapshot preparation; only the
// native wire peer is synthetic. No UI mount, provider, timer grace or compiler.
function fixture(t) {
  const frames = [];
  const transport = new Transport({ output: new EventEmitter(), write(line, done) {
    frames.push(JSON.parse(line)); done();
  } }, { local: true, onMessage() {}, onLost() {} });
  const runtime = new Runtime({ extensions: [] }, transport);
  runtime.features = new Set(['session_entries']);
  const store = { id: 1, method: 'command/execute', controller: new AbortController(), live: true, pending: new Set(), errors: [] };
  runtime.active.set(store.id, store);
  runtime.bind({ context: { resource_owner: owner, host: { session_name: 'Original' } } }, store);
  const pi = createAPI(runtime, 0);
  const name = () => runtime.scope.run(store, () => pi.getSessionName());
  const set = value => runtime.scope.run(store, () => pi.setSessionName(value));
  const reply = result => {
    const call = frames.findLast(frame => frame.method === 'session/set_name');
    assert.deepEqual(call.params.resource_owner, owner);
    assert.equal(call.params.parent_request_id, store.id);
    transport.response({ id: call.id, result });
  };
  t.after(() => { runtime.stopping = true; runtime.uninstallChildren(); transport.fail(new Error('test ended')); });
  assert.equal(runtime.ui.surfaces.size, 0);
  return { runtime, transport, store, frames, name, set, reply };
}

test('setter ACK installs the committed normalized name and explicit clear without a UI mount', async t => {
  const h = fixture(t);
  for (const [input, committed] of [['  Committed name  ', 'Committed name'], ['   ', null], ['Restored', 'Restored'], ['', null]]) {
    const pending = h.set(input);
    h.reply({ name: committed });
    await pending;
    assert.equal(h.name(), committed ?? undefined);
  }
});

test('earlier in-flight and late-starting snapshot preparations cannot undo the setter receipt', async t => {
  const h = fixture(t);
  h.runtime.sessionTransport = new SessionTransport(h.runtime, profile, 1048576);
  const bytes = Buffer.from(JSON.stringify({ entries: [], branch_ids: [], head: null, file: '/session.jsonl', header: {}, labels: {} }));
  const hash = value => createHash('sha256').update(value).digest('hex');
  async function finish(preparation, transferId) {
    const read = h.frames.findLast(frame => frame.method === 'session/snapshot/read' && frame.params.transfer_id === transferId);
    assert.ok(read);
    h.transport.response({ id: read.id, result: { transfer_id: transferId, offset: 0, data: bytes.toString('base64'), next_offset: bytes.length, eof: true } });
    await tick();
    const release = h.frames.findLast(frame => frame.method === 'session/snapshot/release' && frame.params.transfer_id === transferId);
    h.transport.response({ id: release.id, result: { released: true } });
    await preparation;
  }
  function prepare(revision) {
    const transferId = hash(`snapshot-${revision}`);
    const store = { id: 10 + revision, method: 'session/snapshot/prepare', controller: new AbortController(), live: true };
    h.runtime.active.set(store.id, store);
    const preparation = h.runtime.sessionTransport.prepare({ resource_owner: owner, host: { session_name: 'Original' }, snapshot: {
      transfer_id: transferId, kind: 'history', owner, view_revision: revision, head: null,
      bytes: bytes.length, sha256: hash(bytes), entry_count: 0, branch_count: 0, preparation: null,
    } }, store);
    preparation.catch(() => {});
    return { preparation, transferId };
  }
  const earlier = prepare(1); // Host snapshot started before the setter; its read is held.
  const pending = h.set('  Committed name  ');
  h.reply({ name: 'Committed name' });
  await pending;
  assert.equal(h.name(), 'Committed name', 'the ACK must not wait for a snapshot or shell pump');
  await finish(earlier.preparation, earlier.transferId);
  assert.equal(h.name(), 'Committed name');
  const late = prepare(2); // Old captured facts start arriving after the setter ACK.
  await finish(late.preparation, late.transferId);
  assert.equal(h.name(), 'Committed name', 'retained-facts merge precedence must remain unchanged');
});

for (const field of ['session_id', 'extension_instance_id', 'process_generation']) {
  test(`late setter receipt cannot cross ${field} replacement`, async t => {
    const h = fixture(t);
    const pending = h.set('  Old owner write  ');
    const rejected = assert.rejects(pending, /not_foreground_owner/);
    const replacement = { ...owner, [field]: field === 'process_generation' ? 2 : 'replacement' };
    const next = { id: 2, method: 'hook/run', controller: new AbortController(), live: true };
    h.runtime.bind({ hook: 'session_start', context: { resource_owner: replacement, host: { session_name: 'Replacement name' } } }, next);
    h.reply({ name: 'Old owner write' });
    await rejected;
    assert.equal(h.runtime.foreground.host.session_name, 'Replacement name');
    assert.throws(() => h.set('foreign write'), /not_foreground_owner/);
    assert.equal(h.frames.filter(frame => frame.method === 'session/set_name').length, 1);
  });
}

for (const receipt of [{}, { name: 4 }, { name: 'bad\u0000name' }]) {
  test(`malformed setter receipt is refused: ${JSON.stringify(receipt)}`, async t => {
    const h = fixture(t);
    const pending = h.set('Optimistic');
    h.reply(receipt);
    await assert.rejects(pending, /invalid_request/);
    assert.equal(h.name(), 'Original');
  });
}
