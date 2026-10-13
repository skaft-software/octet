import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

async function mounting(t, hold = ['composer/get', 'composer/set']) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-editor-mount-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'editor.ts');
  await writeFile(entry, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => {
  const calls = [];
  pi.on('session_start', (_, ctx) => { ctx.ui.setEditorComponent((tui, theme, kb) => {
    const c = new CustomEditor(tui, theme, kb), input = c.handleInput.bind(c);
    c.handleInput = data => { calls.push(data); input(data); }; return c;
  }); });
  pi.registerTool({ name: 'mount_probe', description: 'Explicit component input observation barrier', parameters: { type: 'object' },
    async execute(_id, args, _signal, _update, ctx) {
      if (args.native) await ctx.ui.setEditorText('native edit');
      return { content: [{ type: 'text', text: JSON.stringify(calls) }] };
    }
  });
};`);
  const peer = launch(t, [entry], { hold }); await peer.init();
  const start = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
  const open = await peer.wait(f => f.method === 'ui/open');
  return { peer, start, open, id: open.params.surface_id };
}
async function probe(peer, args = {}) {
  const reply = await peer.request('tool/call', { name: 'mount_probe', arguments: args, context: peer.context() }).response;
  assert.ok(reply.result, JSON.stringify(reply)); return JSON.parse(reply.result.content[0].text);
}
function ack(peer, request) {
  const { input_revision, checkpoint_revision } = request.params.editor_checkpoint;
  peer.send({ jsonrpc: '2.0', id: request.id, result: { input_revision, checkpoint_revision } });
}
function checkpoint(request, surface, input, revision) {
  assert.deepEqual(request.params.resource_owner, owner);
  assert.deepEqual(request.params.editor_checkpoint, { surface_id: surface, mount_id: `host-editor-${surface}`, input_revision: input, checkpoint_revision: revision });
}
const frame = text => f => f.method === 'ui/frame' && f.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '').includes(text);
const noFrame = peer => assert.equal(peer.seen.some(f => f.method === 'ui/frame'), false, 'first component frame needs the matching final checkpoint');

// No time/grace-period barriers: the seed/checkpoint RPCs are held explicitly,
// and a real tool call observes all preceding input notifications in the process.
test('mounting editor: held seed read defers first delivery in order and first frame awaits matching checkpoints', async t => {
  const { peer, start, id } = await mounting(t);
  const seed = await peer.wait(f => f.method === 'composer/get');
  for (const key of ['u', 'ArrowLeft', 'l']) peer.editorKey(id, key);
  assert.deepEqual(await probe(peer), []);
  assert.equal(peer.seen.some(f => f.method === 'ui/close' || f.method === 'notification'), false);
  noFrame(peer);
  peer.send({ jsonrpc: '2.0', id: seed.id, result: { text: 'seed😀' } });
  for (const [i, text] of ['seed😀u', 'seed😀u', 'seed😀lu'].entries()) {
    const write = await peer.wait(f => f.method === 'composer/set');
    checkpoint(write, id, i + 1, i + 1); assert.equal(write.params.text, text);
    assert.deepEqual(await probe(peer), ['u', '\x1b[D', 'l']); noFrame(peer); ack(peer, write);
  }
  assert.ok((await start.response).result); await peer.wait(frame('seed😀lu'));
  assert.deepEqual(await probe(peer), ['u', '\x1b[D', 'l'], 'each host event is delivered once');
  await peer.close();
});

test('mounting editor: input also waits through the normalized-seed checkpoint', async t => {
  const { peer, start, id } = await mounting(t);
  const seed = await peer.wait(f => f.method === 'composer/get');
  peer.editorKey(id, 'u'); peer.send({ jsonrpc: '2.0', id: seed.id, result: { text: '\tseed' } });
  const normalized = await peer.wait(f => f.method === 'composer/set');
  checkpoint(normalized, id, 0, 1); assert.equal(normalized.params.text, '    seed');
  peer.editorKey(id, 'l'); assert.deepEqual(await probe(peer), []); noFrame(peer); ack(peer, normalized);
  for (const [i, text] of ['    seedu', '    seedul'].entries()) {
    const write = await peer.wait(f => f.method === 'composer/set');
    checkpoint(write, id, i + 1, i + 2); assert.equal(write.params.text, text); noFrame(peer); ack(peer, write);
  }
  assert.ok((await start.response).result); await peer.wait(frame('    seedul')); await peer.close();
});

for (const foreign of [false, true]) test(`mounting editor: pre-open-reply input ${foreign ? 'cannot choose its own mount' : 'waits for host mount identity and seed'}`, async t => {
  const { peer, start, open, id } = await mounting(t, ['ui/open', 'composer/get', 'composer/set']);
  peer.notify('ui/key', { surface_id: id, key: 'u', kind: 'press', modifiers: [],
    editor_input: { mount_id: foreign ? 'foreign' : `host-editor-${id}`, input_revision: 1 } });
  assert.deepEqual(await probe(peer), []); noFrame(peer);
  peer.send({ jsonrpc: '2.0', id: open.id, result: { columns: 80, rows: 24, editor_mount_id: `host-editor-${id}` } });
  if (foreign) {
    assert.match((await start.response).error?.message, /editor input mount\/revision mismatch/);
    await peer.wait(f => f.method === 'ui/close');
    assert.equal(peer.seen.some(f => f.method === 'composer/get' || f.method === 'composer/set'), false);
  } else {
    const seed = await peer.wait(f => f.method === 'composer/get');
    peer.send({ jsonrpc: '2.0', id: seed.id, result: { text: 'seed' } });
    const write = await peer.wait(f => f.method === 'composer/set');
    checkpoint(write, id, 1, 1); assert.equal(write.params.text, 'seedu'); noFrame(peer); ack(peer, write);
    assert.ok((await start.response).result); await peer.wait(frame('seedu'));
  }
  await peer.close();
});

for (const editor_input of [undefined, { mount_id: 'foreign', input_revision: 1 }, { mount_id: 'host-editor-pi-1', input_revision: 2 }]) {
  test(`mounting editor: missing/foreign/gapped fence ${JSON.stringify(editor_input)} still refuses`, async t => {
    const { peer, start, id } = await mounting(t);
    const seed = await peer.wait(f => f.method === 'composer/get');
    peer.notify('ui/key', { surface_id: id, key: 'u', kind: 'press', modifiers: [], ...(editor_input && { editor_input }) });
    await peer.wait(f => f.method === 'ui/close');
    await peer.wait(f => f.method === 'notification' && /editor input/.test(f.params.message));
    assert.deepEqual(await probe(peer), []);
    peer.send({ jsonrpc: '2.0', id: seed.id, result: { text: 'seed' } }); await start.response;
    assert.equal(peer.seen.some(f => f.method === 'composer/set'), false); noFrame(peer); await peer.close();
  });
}

for (const count of [128, 129]) test(`mounting editor: initial input queue ${count === 128 ? 'admits exactly 128' : 'refuses overflow without delivery or invented ACK'}`, async t => {
  const { peer, start, id } = await mounting(t, ['composer/get']);
  const seed = await peer.wait(f => f.method === 'composer/get');
  for (let i = 0; i < count; i++) peer.editorKey(id, 'x');
  assert.deepEqual(await probe(peer), []); noFrame(peer);
  if (count > 128) {
    await peer.wait(f => f.method === 'notification' && /bounds_exceeded editor initial input queue/.test(f.params.message));
    await peer.wait(f => f.method === 'ui/close');
  } else assert.equal(peer.seen.some(f => f.method === 'ui/close'), false);
  peer.send({ jsonrpc: '2.0', id: seed.id, result: { text: 'seed' } }); assert.ok((await start.response).result);
  if (count === 128) {
    const last = await peer.wait(f => f.method === 'composer/set' && f.params.editor_checkpoint.input_revision === 128);
    checkpoint(last, id, 128, 128); assert.equal(last.params.text, 'seed' + 'x'.repeat(128));
    assert.equal((await probe(peer)).length, 128); await peer.wait(f => f.method === 'ui/frame');
  } else {
    assert.deepEqual(await probe(peer), []); noFrame(peer); assert.equal(peer.seen.some(f => f.method === 'composer/set'), false);
  }
  await peer.close();
});

test('mounting editor: rescue discards undispatched input immediately; a late seed cannot overwrite a newer native edit', async t => {
  const { peer, start, id } = await mounting(t, ['composer/get']);
  const seed = await peer.wait(f => f.method === 'composer/get');
  peer.editorKey(id, 'u'); peer.editorKey(id, 'l'); peer.notify('ui/closed', { surface_id: id, reason: 'host rescue' });
  assert.deepEqual(await probe(peer, { native: true }), []);
  const native = await peer.wait(f => f.method === 'composer/set');
  assert.equal(native.params.text, 'native edit'); assert.equal(native.params.editor_checkpoint, undefined);
  peer.send({ jsonrpc: '2.0', id: seed.id, result: { text: 'stale seed' } }); assert.ok((await start.response).result);
  assert.deepEqual(await probe(peer), []); noFrame(peer);
  assert.equal(peer.seen.filter(f => f.method === 'composer/set').length, 1);
  assert.equal(peer.seen.some(f => f.method === 'ui/close'), false); await peer.close();
});
