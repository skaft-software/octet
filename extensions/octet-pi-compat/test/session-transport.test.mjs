// Routed session-repair.v2 transport against the real adapter process.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';

const profile = { profile: 'json-chunks.v1', chunk_bytes: 65536, snapshot_bytes: 268435456,
  generation_bytes: 536870912, owner_views: 64, transfers: 64, view_entries: 1048576,
  generation_entries: 2097152, projection_bytes: 67108864, projections: 64 };
const features = ['session_entries', 'session_owner_routes_v1', 'session_snapshot_transport_v1'];
const limits = { max_concurrent_requests: 8, max_message_bytes: 1048576 };
const offer = { limits, session_snapshot_transport_v1: profile };
const hash = value => createHash('sha256').update(value).digest('hex');
const grant = (head, revision) => ({ grant_id: hash(`grant:${revision}`), activation_epoch: revision,
  operation_id: `provider-context:${revision}`, owner, expected_head: head });
const message = (id, parent, text) => ({ id, parent, timestamp_unix_ms: 1,
  value: { type: 'message', User: { content: [{ Text: text }] } } });

function fixture(directory, script) {
  const path = join(directory, 'factory.mjs');
  writeFileSync(path, script);
  return path;
}

const countingFactory = `export default function (pi) {
  pi.on('context', (event, ctx) => {
    const entries = ctx.sessionManager.getEntries();
    const branch = ctx.sessionManager.getBranch();
    if (entries.length !== branch.length) throw Object.assign(new Error('incomplete branch'), { code: -32602 });
    return { messages: [{ role: 'user', content: 'count:' + entries.length }] };
  });
}`;

function history(entries) {
  return { entries, branch_ids: entries.map(entry => entry.id), head: entries.at(-1)?.id ?? null,
    file: '/scratch/session.jsonl', header: {}, labels: {} };
}

// Serve one descriptor's chunks and release exactly like a native parent would.
// A refused document is never released by the adapter: parent settlement owns it.
async function serve(peer, buffers, transferId, { release = true } = {}) {
  for (;;) {
    const frame = await peer.wait(f => f.method === 'session/snapshot/read' && f.params.transfer_id === transferId, 4000);
    const buffer = buffers.get(transferId);
    const { offset } = frame.params;
    const end = Math.min(buffer.length, offset + frame.params.max_bytes);
    peer.send({ jsonrpc: '2.0', id: frame.id, result: { transfer_id: transferId, offset,
      data: buffer.subarray(offset, end).toString('base64'), next_offset: end, eof: end === buffer.length } });
    if (end < buffer.length) continue;
    if (!release) return;
    const released = await peer.wait(f => f.method === 'session/snapshot/release' && f.params.transfer_id === transferId, 4000);
    peer.send({ jsonrpc: '2.0', id: released.id, result: { released: true } });
    return;
  }
}

// Serve every descriptor of one revision in order; early rejections stay
// available through the hook reply instead of becoming unhandled.
function serveAll(peer, buffers, ids, options) {
  const served = (async () => { for (const id of ids) await serve(peer, buffers, id, options); })();
  served.catch(() => {});
  return served;
}

function descriptors(entries, revision) {
  const snapshot = history(entries);
  const preparation = { activation_epoch: revision, operation_id: `provider-context:${revision}`,
    tool_generation: revision, head: snapshot.head };
  const document = (kind, value, extra = {}) => {
    const bytes = Buffer.from(JSON.stringify(value));
    return { descriptor: { transfer_id: hash(`${kind}:${revision}:${bytes.length}`), kind, owner,
      view_revision: revision, head: snapshot.head, bytes: bytes.length, sha256: hash(bytes),
      entry_count: kind === 'invocation' ? 0 : value.entries.length,
      branch_count: kind === 'invocation' ? 0 : value.branch_ids.length,
      preparation: kind === 'invocation' ? preparation : preparation, ...extra }, bytes };
  };
  const snapshotDescriptor = document('history', snapshot);
  const payload = { request: { system: 'routed system', messages: [{ User: { content: [{ Text: 'seed' }] } }], tools: [] },
    preparation: { resource_owner: owner.session_id, session_id: 'actual-session', head: preparation.head, tool_generation: revision } };
  const invocation = document('invocation', payload);
  return { snapshot: snapshotDescriptor, invocation, preparation };
}

function contextParams(peer, parts, revision) {
  const preparation = parts.preparation;
  return { hook: 'provider_context', session_leaf: grant(preparation.head, revision),
    context: peer.context({ session_name: 'routed', session_leaf_id: preparation.head, session_entries: [], session_branch: [] }),
    session_snapshot: parts.snapshot.descriptor, session_payload: parts.invocation.descriptor };
}

test('routed hook hydrates the complete document and refuses a cancelled transfer', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'session-transport-'));
  const peer = launch(t, [fixture(directory, countingFactory)]);
  const initialized = await peer.init(features, offer);
  assert.deepEqual(initialized.protocol.session_snapshot_transport_v1, profile);
  assert.equal(initialized.protocol.limits.max_message_bytes, 1048576);
  for (const feature of features) assert.ok(initialized.protocol.features.includes(feature), feature);
  await peer.start();

  // A complete history far past the legacy 16 KiB private-entry bound.
  const entries = Array.from({ length: 40 }, (_, index) => message(`e${index}`, index ? `e${index - 1}` : null, `${index}:${'x'.repeat(7996)}`));
  const parts = descriptors(entries, 1);
  const buffers = new Map([[parts.snapshot.descriptor.transfer_id, parts.snapshot.bytes],
    [parts.invocation.descriptor.transfer_id, parts.invocation.bytes]]);
  const served = serveAll(peer, buffers, [parts.snapshot.descriptor.transfer_id, parts.invocation.descriptor.transfer_id]);
  const reply = await peer.request('hook/run', contextParams(peer, parts, 1)).response;
  assert.ok(reply.result, JSON.stringify(reply));
  await served;
  assert.deepEqual(reply.result.provider_context.messages, [{ User: { content: [{ Text: 'count:40' }] } }]);

  // Mid-transfer cancellation: one chunk arrives, the parent is cancelled.
  const cancelled = descriptors(entries, 2);
  const pending = peer.request('hook/run', contextParams(peer, cancelled, 2));
  const first = await peer.wait(f => f.method === 'session/snapshot/read' && f.params.transfer_id === cancelled.snapshot.descriptor.transfer_id, 4000);
  peer.send({ jsonrpc: '2.0', id: first.id, result: { transfer_id: first.params.transfer_id, offset: 0,
    data: cancelled.snapshot.bytes.subarray(0, first.params.max_bytes).toString('base64'),
    next_offset: first.params.max_bytes, eof: false } });
  peer.notify('$/cancelRequest', { id: pending.id, reason: 'mid-transfer cancel' });
  const refused = await pending.response;
  assert.equal(refused.error?.code, -32800, JSON.stringify(refused));

  // The next complete revision still hydrates exactly; nothing partial survived.
  const fresh = descriptors(entries, 3);
  const freshBuffers = new Map([[fresh.snapshot.descriptor.transfer_id, fresh.snapshot.bytes],
    [fresh.invocation.descriptor.transfer_id, fresh.invocation.bytes]]);
  const servedAgain = serveAll(peer, freshBuffers, [fresh.snapshot.descriptor.transfer_id, fresh.invocation.descriptor.transfer_id]);
  const replyAgain = await peer.request('hook/run', contextParams(peer, fresh, 3)).response;
  assert.ok(replyAgain.result, JSON.stringify(replyAgain));
  await servedAgain;
  assert.deepEqual(replyAgain.result.provider_context.messages, [{ User: { content: [{ Text: 'count:40' }] } }]);
  await peer.close();
});

test('routed hydration refuses a document whose identity does not match', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'session-transport-'));
  const peer = launch(t, [fixture(directory, countingFactory)]);
  await peer.init(features, offer);
  await peer.start();
  const entries = [message('e0', null, 'only')];
  const parts = descriptors(entries, 1);
  parts.snapshot.descriptor.sha256 = hash('not the document');
  const buffers = new Map([[parts.snapshot.descriptor.transfer_id, parts.snapshot.bytes],
    [parts.invocation.descriptor.transfer_id, parts.invocation.bytes]]);
  // The adapter reads the history document and stops at the digest mismatch;
  // it never reaches the invocation descriptor or a release.
  const served = serveAll(peer, buffers, [parts.snapshot.descriptor.transfer_id], { release: false });
  const reply = await peer.request('hook/run', contextParams(peer, parts, 1)).response;
  await served;
  assert.equal(reply.error?.code, -32602, JSON.stringify(reply));
  assert.match(reply.error.message, /digest|session document/i);
  assert.ok(!peer.seen.some(frame => frame.method === 'session/snapshot/release'
    && frame.params.transfer_id === parts.snapshot.descriptor.transfer_id),
  'a refused document is never released as usable');
  assert.ok(!peer.seen.some(frame => frame.method === 'session/snapshot/read'
    && frame.params.transfer_id === parts.invocation.descriptor.transfer_id),
  'a refused history document never reaches its invocation payload');
  // A mismatched digest never becomes a readable view.
  const retry = descriptors(entries, 2);
  const retryBuffers = new Map([[retry.snapshot.descriptor.transfer_id, retry.snapshot.bytes],
    [retry.invocation.descriptor.transfer_id, retry.invocation.bytes]]);
  const servedRetry = serveAll(peer, retryBuffers, [retry.snapshot.descriptor.transfer_id, retry.invocation.descriptor.transfer_id]);
  const replyRetry = await peer.request('hook/run', contextParams(peer, retry, 2)).response;
  assert.ok(replyRetry.result, JSON.stringify(replyRetry));
  await servedRetry;
  assert.deepEqual(replyRetry.result.provider_context.messages, [{ User: { content: [{ Text: 'count:1' }] } }]);
  await peer.close();
});

test('routed transport requires the paired offer and exact profile', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'session-transport-'));
  for (const [protocol, message] of [
    [{ limits }, /session transport profile must be an object/],
    [{ limits, session_snapshot_transport_v1: { ...profile, chunk_bytes: 65537 } }, /session transport profile/],
    [{ limits, session_snapshot_transport_v1: { ...profile, profile: 'json-chunks.v2' } }, /session transport profile/],
  ]) {
    const peer = launch(t, [fixture(directory, countingFactory)]);
    await assert.rejects(peer.init(features, protocol), message);
    await peer.close();
  }
  // Below the paired concurrency bound the adapter refuses instead of
  // accepting an unreachable transport.
  const peer = launch(t, [fixture(directory, countingFactory)]);
  await assert.rejects(peer.init(features, { limits: { max_concurrent_requests: 1, max_message_bytes: 1048576 }, session_snapshot_transport_v1: profile }), /paired session transport/);
  await peer.close();
});


test('a live editor mount keeps its invocation view across newer publication and uses the published view after settlement', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'session-editor-view-'));
  const entry = fixture(directory, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => pi.on('session_start', (_, ctx) => {
  ctx.ui.setEditorComponent((tui, theme, keys) => new class extends CustomEditor {
    render(width) { return [...super.render(width), 'view:' + ctx.sessionManager.getBranch().length]; }
  }(tui, theme, keys));
});`);
  const peer = launch(t, [entry], {hold: ['ui/open']});
  await peer.init([...features, 'remote_ui', 'composer'], offer);
  const snapshot = (entries, revision) => {
    const bytes = Buffer.from(JSON.stringify(history(entries)));
    return {bytes, descriptor: {transfer_id: hash(`editor:${revision}`), kind: 'history', owner,
      view_revision: revision, head: entries.at(-1)?.id ?? null, bytes: bytes.length,
      sha256: hash(bytes), entry_count: entries.length, branch_count: entries.length, preparation: null}};
  };
  const initial = snapshot([message('old', null, 'initial')], 1);
  const start = peer.request('hook/run', {hook: 'session_start', payload: {binding: owner},
    context: peer.context({session_view_revision: 1, session_entries: null, session_branch: null}),
    session_snapshot: initial.descriptor});
  await serve(peer, new Map([[initial.descriptor.transfer_id, initial.bytes]]), initial.descriptor.transfer_id);
  const open = await peer.wait(f => f.method === 'ui/open');
  peer.notify('context/updated', {resource_owner: owner, host: {session_view_revision: 2,
    session_entries: null, session_branch: null}});
  peer.send({jsonrpc: '2.0', id: open.id, result: {columns: 80, rows: 24, editor_mount_id: 'view-editor'}});
  const response = await start.response;
  assert.ok(response.result, JSON.stringify(response));
  await peer.wait(f => f.method === 'ui/frame' && f.params.lines.includes('view:1'));

  // A resize paints after the originating invocation has settled, while the
  // newest owner publication is still being transferred. It must not retire
  // the editor just because its retained callback cannot yet read history.
  peer.notify('ui/resize', {surface_id: open.params.surface_id, columns: 81, rows: 24});
  const next = snapshot([message('a', null, 'new'), message('b', 'a', 'newer')], 2);
  const publication = peer.request('session/snapshot/prepare', {resource_owner: owner,
    snapshot: next.descriptor, host: {session_view_revision: 2}});
  await serve(peer, new Map([[next.descriptor.transfer_id, next.bytes]]), next.descriptor.transfer_id);
  assert.ok((await publication.response).result);
  await peer.wait(f => f.method === 'ui/frame' && f.params.lines.includes('view:2'));
  await peer.close();
});
