// A hook whose history descriptor names the owner's current prepared view must
// reuse that parsed document instead of reading the whole history again.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { owner } from './helper.mjs';
import { SessionTransport } from '../lib/session-transport.mjs';
import { ownerKey } from '../lib/errors.mjs';

const profile = { profile: 'json-chunks.v1', chunk_bytes: 65536, snapshot_bytes: 268435456,
  generation_bytes: 536870912, owner_views: 64, transfers: 64, view_entries: 1048576,
  generation_entries: 2097152, projection_bytes: 67108864, projections: 64 };
const hash = value => createHash('sha256').update(value).digest('hex');
const message = (id, parent, text) => ({ id, parent, timestamp_unix_ms: 1,
  value: { type: 'message', User: { content: [{ Text: text }] } } });
function history(texts) {
  const entries = texts.map((text, index) => message(`e${index}`, index ? `e${index - 1}` : null, text));
  return { entries, branch_ids: entries.map(entry => entry.id), head: entries.at(-1)?.id ?? null,
    file: '/scratch/session.jsonl', header: {}, labels: {} };
}

function harness() {
  const buffers = new Map(), reads = new Map(), releases = [];
  const controller = new AbortController();
  const runtime = {
    namespace: 'octet-pi-compat',
    active: new Map([[1, { controller }]]),
    transport: {
      async request(method, params) {
        if (method === 'session/snapshot/read') {
          reads.set(params.transfer_id, (reads.get(params.transfer_id) ?? 0) + 1);
          const buffer = buffers.get(params.transfer_id), end = Math.min(buffer.length, params.offset + params.max_bytes);
          return { transfer_id: params.transfer_id, offset: params.offset,
            data: buffer.subarray(params.offset, end).toString('base64'), next_offset: end, eof: end === buffer.length };
        }
        if (method === 'session/snapshot/release') { releases.push(params.transfer_id); return { released: true }; }
        throw new Error(`unexpected ${method}`);
      },
    },
  };
  const store = { id: 1, controller, live: true, state: { alive: true, key: ownerKey(owner), owner, host: {} } };
  const transport = new SessionTransport(runtime, profile, 1048576);
  let serial = 0;
  const describe = (revision, value) => {
    const bytes = Buffer.from(JSON.stringify(value));
    const transfer_id = hash(`transfer:${serial++}`);
    buffers.set(transfer_id, bytes);
    return { transfer_id, kind: 'history', owner, view_revision: revision, head: value.head, bytes: bytes.length,
      sha256: hash(bytes), entry_count: value.entries.length, branch_count: value.branch_ids.length, preparation: null };
  };
  // Install a prepared current view the way prepare() does.
  const publish = async (barrier, d) => {
    const view = await transport.read(d, store);
    transport.history(view);
    barrier.view = view; barrier.resolve?.(); barrier.promise = undefined;
    return view;
  };
  const hook = d => ({ context: { resource_owner: owner }, session_snapshot: d });
  const fresh = () => ({ ...store, sessionView: undefined });
  return { transport, store, reads, releases, describe, publish, hook, fresh };
}

test('a hook naming the prepared revision reuses its parsed view without reading', async () => {
  const h = harness();
  const value = history(['one', 'two', 'three']);
  const prepared = await h.publish(h.transport.invalidate(owner, 1), h.describe(1, value));
  const bytesBefore = h.transport.bytes;

  const hookDescriptor = h.describe(1, value);
  const hookStore = h.fresh();
  await h.transport.hydrate(h.hook(hookDescriptor), hookStore);
  assert.equal(hookStore.sessionView, prepared, 'the prepared view is reused');
  assert.equal(h.reads.get(hookDescriptor.transfer_id), undefined, 'no chunk of the hook document is read');
  assert.ok(h.releases.includes(hookDescriptor.transfer_id), 'the hook transfer handle is released');
  assert.equal(h.transport.bytes, bytesBefore, 'no second copy is reserved');
  assert.equal(h.transport.host(hookStore).session_entries.length, 3);

  // The hook's lease outlives a newer publication until it settles.
  h.transport.invalidate(owner, 2);
  assert.notEqual(hookStore.sessionView.document, undefined);
  h.transport.drop(hookStore.sessionView);
  assert.equal(prepared.document, undefined, 'the last lease releases the document');
});

test('a different document at the same revision is read, never reused', async () => {
  const h = harness();
  await h.publish(h.transport.invalidate(owner, 1), h.describe(1, history(['one', 'two'])));
  const other = h.describe(1, history(['one', 'changed']));
  const hookStore = h.fresh();
  await h.transport.hydrate(h.hook(other), hookStore);
  assert.ok(h.reads.get(other.transfer_id) >= 1, 'a digest mismatch reads its own document');
  assert.equal(h.transport.host(hookStore).session_entries[1].value.User.content[0].Text, 'changed');
});

test('a hook arriving during that revision\'s preparation waits for it instead of reading twice', async () => {
  const h = harness();
  const value = history(['a', 'b']);
  const barrier = h.transport.invalidate(owner, 3);
  const prepareDescriptor = h.describe(3, value);
  const hookDescriptor = h.describe(3, value);
  const hookStore = h.fresh();
  const hydrated = h.transport.hydrate(h.hook(hookDescriptor), hookStore);
  await new Promise(resolve => setImmediate(resolve));
  const prepared = await h.publish(barrier, prepareDescriptor);
  await hydrated;
  assert.equal(hookStore.sessionView, prepared);
  assert.equal(h.reads.get(hookDescriptor.transfer_id), undefined);
});
