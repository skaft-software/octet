import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { launch, root, owner } from './helper.mjs';

// Actual factory + process + Runtime + Transport. A TEST-ONLY runner stops after
// the opening hook dispatch/flush, BEFORE Runtime.receive writes its terminal
// reply. No timer, fabricated parent ID, altered production API or fake ACK.
async function boundary(t, editor = true) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-editor-lifetime-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'editor.ts'), runner = join(dir, 'barrier.mjs');
  await writeFile(entry, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => {
  let opening;
  pi.on('session_start', (_, ctx) => { if (opening) return; opening = ctx; if (${editor}) ctx.ui.setEditorComponent((tui, theme, kb) => new (class extends CustomEditor { handleInput(data) { return super.handleInput(data); } })(tui, theme, kb)); });
  pi.registerTool({ name: 'retained_probe', description: 'Exercise the actual retained context', parameters: { type: 'object' },
    async execute(_id, args, _signal, _update, ctx) {
      if (args.mode === 'plain') await opening.ui.setEditorText('plain set');
      if (args.mode === 'confirm') await opening.ui.confirm('retained?', 'must require a live request');
      if (args.mode === 'stale') await opening.ui.setEditorText('stale overwrite');
      if (args.mode === 'native') await ctx.ui.setEditorText('native edit');
      return { content: [{ type: 'text', text: 'probe complete' }] };
    }
  });
};`);
  await writeFile(runner, `import { Runtime } from ${JSON.stringify(pathToFileURL(join(root, 'lib/runtime.mjs')).href)};
const dispatch = Runtime.prototype.dispatch, receive = Runtime.prototype.receive;
const release = Promise.withResolvers(), settled = Promise.withResolvers(); let openingId;
Runtime.prototype.dispatch = async function(message, store) {
  if (message.method === 'test/release-hook') { release.resolve(); return {}; }
  if (message.method === 'test/settled') { await settled.promise; return {}; }
  const result = await dispatch.call(this, message, store);
  if (message.method === 'hook/run' && message.params.hook === 'session_start') {
    openingId = message.id;
    await this.transport.notify('notification', { level: 'info', message: 'test: opening hook dispatch complete' });
    await release.promise;
  }
  return result;
};
Runtime.prototype.receive = async function(message) {
  await receive.call(this, message);
  if (message.id === openingId) settled.resolve();
};
await import(${JSON.stringify(pathToFileURL(join(root, 'runner.mjs')).href)});`);
  const peer = launch(t, [entry], { runner, composer: 'seed', hold: ['composer/set'] }); await peer.init();
  const start = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
  await peer.wait(f => f.method === 'notification' && f.params.message === 'test: opening hook dispatch complete');
  if (!editor) return { peer, start };
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  peer.editorKey(id, 'u');
  const write = await peer.wait(f => f.method === 'composer/set');
  assert.equal(write.params.parent_request_id, start.id); assert.deepEqual(write.params.resource_owner, owner);
  assert.deepEqual(write.params.editor_checkpoint, { surface_id: id, mount_id: `host-editor-${id}`, input_revision: 1, checkpoint_revision: 1 });
  assert.equal(write.params.text, 'seedu');
  assert.equal(peer.seen.some(f => f.id === start.id && !f.method), false, 'checkpoint is actually sent before opening hook reply');
  return { peer, start, id, write };
}
async function settle(peer, start, code) {
  assert.ok((await peer.request('test/release-hook', {}).response).result);
  const response = await start.response;
  if (code === undefined) assert.ok(response.result, JSON.stringify(response));
  else assert.equal(response.error.code, code);
  assert.ok((await peer.request('test/settled', {}).response).result);
}
function ack(peer, request) {
  const { input_revision, checkpoint_revision } = request.params.editor_checkpoint;
  peer.send({ jsonrpc: '2.0', id: request.id, result: { input_revision, checkpoint_revision } });
}
const frame = text => f => f.method === 'ui/frame' && f.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '').includes(text);
const cancelled = id => f => f.method === '$/cancelRequest' && f.params.id === id;
const probe = (peer, mode, context = peer.context()) => peer.request('tool/call', { name: 'retained_probe', arguments: { mode }, context });

test('retained editor: withheld checkpoint ACK survives the opening hook reply, preserving FIFO and exact frame barriers', async t => {
  const { peer, start, id, write } = await boundary(t);
  peer.editorKey(id, 'l');
  await settle(peer, start);
  assert.equal(peer.seen.some(cancelled(write.id)), false, 'normal parent settlement must not cancel owner-retained work');
  assert.equal(peer.seen.some(f => f.method === 'ui/close'), false);
  assert.equal(peer.seen.some(frame('seedu')), false);
  ack(peer, write);
  const second = await peer.wait(f => f.method === 'composer/set');
  assert.equal(second.params.text, 'seedul'); assert.equal(second.params.parent_request_id, start.id);
  assert.deepEqual(second.params.editor_checkpoint, { surface_id: id, mount_id: `host-editor-${id}`, input_revision: 2, checkpoint_revision: 2 });
  assert.equal(peer.seen.some(frame('seedul')), false); ack(peer, second); await peer.wait(frame('seedul'));
  assert.match((await probe(peer, 'confirm').response).error?.message, /confirmation\/request requires a live request/);
  assert.equal(peer.seen.some(f => f.method === 'confirmation/request'), false);
  assert.equal(peer.seen.some(f => f.method === 'notification' && /parent settled|request cancelled/.test(f.params.message)), false);
  await peer.close();
});

test('retained context: generic composer/set without checkpoint still cancels on normal parent settlement', async t => {
  // A plain composer write is legitimate without an editor mount; do not use
  // an already-forbidden plain write into a mounted editor as the negative.
  const { peer, start } = await boundary(t, false);
  const plain = probe(peer, 'plain'); plain.response.catch(() => {});
  const write = await peer.wait(f => f.method === 'composer/set');
  assert.equal(write.params.text, 'plain set'); assert.equal(write.params.editor_checkpoint, undefined);
  assert.equal(write.params.parent_request_id, start.id); assert.deepEqual(write.params.resource_owner, owner);
  assert.equal(peer.seen.some(f => f.id === start.id && !f.method), false);
  await settle(peer, start);
  assert.equal(peer.seen.some(cancelled(write.id)), true, 'normal parent settlement must still cancel generic owner-scoped composer writes');
  assert.match((await plain.response).error?.message, /parent settled/);
  peer.send({ jsonrpc: '2.0', id: write.id, result: {} });
  assert.ok((await probe(peer).response).result);
  assert.equal(peer.seen.filter(cancelled(write.id)).length, 1);
  assert.equal(peer.seen.filter(f => f.id === plain.id && !f.method).length, 1);
  await peer.close();
});

for (const after of [false, true]) test(`retained editor: genuine opening-request cancellation ${after ? 'after' : 'before'} reply still aborts held work once`, async t => {
  const { peer, start, id, write } = await boundary(t);
  if (after) await settle(peer, start);
  peer.notify('$/cancelRequest', { id: start.id });
  await peer.wait(cancelled(write.id)); await peer.wait(f => f.method === 'ui/close');
  if (!after) await settle(peer, start, -32800);
  ack(peer, write); peer.editorKey(id, 'x');
  assert.ok((await probe(peer).response).result);
  assert.equal(peer.seen.filter(cancelled(write.id)).length, 1);
  assert.equal(peer.seen.filter(f => f.method === 'composer/set').length, 1);
  assert.equal(peer.seen.some(frame('seedu')), false); await peer.close();
});

test('retained editor: explicit child cancellation still refuses the checkpoint and ignores its late ACK', async t => {
  const { peer, start, id, write } = await boundary(t); await settle(peer, start);
  peer.notify('$/cancelRequest', { id: write.id });
  await peer.wait(cancelled(write.id)); await peer.wait(f => f.method === 'ui/close');
  ack(peer, write); peer.editorKey(id, 'x'); assert.ok((await probe(peer).response).result);
  assert.equal(peer.seen.filter(cancelled(write.id)).length, 1);
  assert.equal(peer.seen.some(frame('seedu')), false); await peer.close();
});

for (const retire of ['session_end', 'replacement']) test(`retained editor: ${retire} fences late ACKs, queued writes and captured contexts`, async t => {
  const { peer, start, id, write } = await boundary(t); peer.editorKey(id, 'l'); await settle(peer, start);
  let context = peer.context();
  if (retire === 'session_end') {
    const ended = await peer.request('hook/run', { hook: 'session_end', payload: { binding: owner, reason: 'shutdown' }, context }).response;
    assert.ok(ended.result, JSON.stringify(ended));
  } else {
    context = { ...context, resource_owner: { ...owner, session_id: 'host-issued-next-owner' } };
    // A tool call cannot authorize foreground replacement; native start does.
    assert.ok((await peer.request('hook/run', { hook: 'session_start', payload: { binding: context.resource_owner }, context }).response).result);
    const native = probe(peer, 'native', context);
    const next = await peer.wait(f => f.method === 'composer/set');
    assert.equal(next.params.text, 'native edit'); assert.equal(next.params.editor_checkpoint, undefined);
    peer.send({ jsonrpc: '2.0', id: next.id, result: {} }); assert.ok((await native.response).result);
  }
  ack(peer, write); peer.editorKey(id, 'x');
  await peer.wait(f => f.method === 'notification' && /editor mount retired/.test(f.params.message));
  assert.match((await probe(peer, 'stale', context).response).error?.message, /not_foreground_owner/);
  assert.equal(peer.seen.some(frame('seedu')), false);
  assert.equal(peer.seen.some(f => f.method === 'composer/set' && f.params.text === 'seedul'), false);
  assert.equal(peer.seen.some(f => f.method === 'composer/set' && f.params.text === 'stale overwrite'), false);
  await peer.close();
});

test('retained editor: reusing a settled numeric ID cannot revive its captured context for nonretained methods', async t => {
  const { peer, start, write } = await boundary(t); await settle(peer, start); ack(peer, write); await peer.wait(frame('seedu'));
  peer.send({ jsonrpc: '2.0', id: start.id, method: 'tool/call', params: { name: 'retained_probe', arguments: { mode: 'confirm' }, context: peer.context() } });
  // This explicit service barrier observes the attempted call even on a broken
  // implementation that would wait indefinitely for a confirmation response.
  await peer.request('test/settled', {}).response;
  assert.equal(peer.seen.some(f => f.method === 'confirmation/request'), false);
  const refused = await peer.wait(f => f.id === start.id && !f.method);
  assert.match(refused.error?.message, /confirmation\/request requires a live request/); await peer.close();
});
