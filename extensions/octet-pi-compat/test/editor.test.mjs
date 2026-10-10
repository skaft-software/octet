import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';
import { setTimeout as delay } from 'node:timers/promises';

const frame = text => f => f.method === 'ui/frame' && f.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '').includes(text);
const factory = '(tui,theme,kb) => new (class extends CustomEditor { handleInput(data) { return super.handleInput(data); } })(tui,theme,kb)';
async function fixture(t, body = '', make = factory) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-editor-fence-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'editor.ts');
  await writeFile(path, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => {
  const factory = ${make};
  pi.on('session_start', (_,ctx) => { ctx.ui.setEditorComponent(factory); });
  ${body}
};`);
  return path;
}
function ack(peer, request) {
  const { input_revision, checkpoint_revision } = request.params.editor_checkpoint;
  peer.send({ jsonrpc: '2.0', id: request.id, result: { input_revision, checkpoint_revision } });
}
function checkpoint(request, surface, input, revision) {
  assert.deepEqual(request.params.resource_owner, owner);
  assert.deepEqual(request.params.editor_checkpoint, { surface_id: surface,
    mount_id: `host-editor-${surface}`, input_revision: input, checkpoint_revision: revision });
}

test('editor frames await their captured checkpoint, never a newer draft on an older ACK; late echoes cannot replace it', async t => {
  const entry = await fixture(t);
  const peer = launch(t, [entry], { composer: 'seed', hold: ['composer/set'] });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  await peer.wait(frame('seed'));
  peer.editorKey(id, 'u');
  const first = await peer.wait(f => f.method === 'composer/set');
  checkpoint(first, id, 1, 1); assert.equal(first.params.text, 'seedu');
  await delay(30); // Permit the frame task to capture its held barrier.
  peer.editorKey(id, 'l');
  peer.notify('ui/editor-state', { text: 'seedu', revision: 1, focused: true });
  await delay(30);
  assert.equal(peer.seen.some(frame('seedu')), false, 'no draft frame before its ACK');
  ack(peer, first);
  await peer.wait(frame('seedu'));
  const second = await peer.wait(f => f.method === 'composer/set');
  checkpoint(second, id, 2, 2); assert.equal(second.params.text, 'seedul');
  assert.equal(peer.seen.some(frame('seedul')), false, 'older ACK must not release newly rendered text');
  ack(peer, second); await peer.wait(frame('seedul'));
  // The queue has drained. A delayed, unfenced echo is still not authoritative.
  peer.notify('ui/editor-state', { text: 'seedu', revision: 2, focused: true });
  peer.editorKey(id, 't');
  const third = await peer.wait(f => f.method === 'composer/set');
  checkpoint(third, id, 3, 3); assert.equal(third.params.text, 'seedult');
  ack(peer, third); await peer.wait(frame('seedult'));
  peer.notify('ui/closed', { surface_id: id, reason: 'host rescue' });
  await peer.close();
});

for (const replace of [false, true]) for (const reject of [false, true]) test(`${replace ? 'replacing' : 'clearing'} an editor ${reject ? 'reports checkpoint refusal' : 'checkpoints the complete draft'} before restoring the host`, async t => {
  const entry = await fixture(t, `pi.registerCommand('restore', { handler: async (_,ctx) => {
    ctx.ui.notify('restore requested');
    await ctx.ui.setEditorComponent(${replace ? 'factory' : 'undefined'});
  } });`);
  const hold = ['composer/set'];
  const peer = launch(t, [entry], { composer: 'seed', hold });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  await peer.wait(frame('seed'));
  peer.editorKey(id, 'u');
  const write = await peer.wait(f => f.method === 'composer/set');
  assert.equal(write.params.text, 'seedu');
  peer.editorKey(id, 'l');
  const restore = peer.command('restore');
  await peer.wait(f => f.method === 'composer/get' && f.params.parent_request_id === restore.id);
  await peer.wait(f => f.method === 'notification' && f.params.message === 'restore requested');
  await delay(30);
  assert.equal(peer.seen.some(f => f.method === 'ui/close'), false, 'close must not race the checkpoint');
  peer.editorKey(id, 'x'); // Retiring editor admits no new input.
  if (replace) hold.push('composer/get');
  if (reject) peer.send({ jsonrpc: '2.0', id: write.id, error: { code: -32002, message: 'checkpoint refused' } });
  else {
    ack(peer, write);
    const last = await peer.wait(f => f.method === 'composer/set');
    checkpoint(last, id, 2, 2); assert.equal(last.params.text, 'seedul');
    assert.equal(peer.seen.some(f => f.method === 'ui/close'), false);
    ack(peer, last);
  }
  await peer.wait(f => f.method === 'ui/close');
  if (replace && !reject) {
    const next = await peer.wait(f => f.method === 'ui/open');
    assert.notEqual(next.params.surface_id, id);
    const seed = await peer.wait(f => f.method === 'composer/get' && f.params.parent_request_id === restore.id);
    peer.send({ jsonrpc: '2.0', id: seed.id, result: { text: 'seedul' } });
    await peer.wait(f => frame('seedul')(f) && f.params.surface_id === next.params.surface_id);
  }
  const response = await restore.response;
  if (reject) assert.match(response.error?.message, /checkpoint refused/);
  else assert.ok(response.result, JSON.stringify(response));
  await peer.close();
});

test('old hosts without editor_mount_id refuse explicitly and close the unusable mount', async t => {
  const peer = launch(t, [await fixture(t)], { editorFence: false });
  await peer.init();
  const response = await peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() }).response;
  assert.match(response.error?.message, /unsupported_feature editor checkpoint.*editor_mount_id/);
  await peer.wait(f => f.method === 'ui/close');
  assert.equal(peer.seen.some(f => f.method === 'ui/frame'), false);
  await peer.close();
});

for (const result of [{}, { input_revision: 0, checkpoint_revision: 1 }, { input_revision: 1, checkpoint_revision: 2 }]) {
  test(`mismatched checkpoint ACK ${JSON.stringify(result)} never releases a frame`, async t => {
    const peer = launch(t, [await fixture(t)], { composer: 'seed', hold: ['composer/set'] });
    await peer.init(); await peer.start();
    const open = await peer.wait(f => f.method === 'ui/open');
    peer.editorKey(open.params.surface_id, 'u');
    const write = await peer.wait(f => f.method === 'composer/set');
    peer.send({ jsonrpc: '2.0', id: write.id, result });
    await peer.wait(f => f.method === 'ui/close');
    assert.equal(peer.seen.some(frame('seedu')), false);
    await peer.waitStderr(/editor checkpoint acknowledgement mismatch/);
    assert.match(peer.stderr(), /editor checkpoint acknowledgement mismatch/);
    await peer.close();
  });
}

test('completed events checkpoint final text once, including no-op/release; only host acceptance clears the slot', async t => {
  const make = `tui => ({ text: '', render() { return [this.text]; }, invalidate() {},
    setText(text) { this.text = text; this.onChange?.(text); }, getText() { return this.text; },
    handleInput(data) {
      if (data === 'u') { this.setText('intermediate'); this.setText('complete'); }
      if (data === '\\r') { this.setText(''); this.onSubmit?.('complete'); }
    }
  })`;
  const peer = launch(t, [await fixture(t, '', make)], { composer: 'seed', hold: ['composer/set'] });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  for (const [index, key, kind, text] of [[1, 'u', 'press', 'complete'], [2, 'u', 'release', 'complete'], [3, 'ArrowLeft', 'press', 'complete']]) {
    peer.editorKey(id, key, kind);
    const write = await peer.wait(f => f.method === 'composer/set');
    checkpoint(write, id, index, index); assert.equal(write.params.text, text);
    assert.equal(peer.seen.some(f => f.method === 'composer/submit'), false);
    assert.equal(peer.seen.some(f => f.method === 'session/send_user_message'), false);
    ack(peer, write);
  }
  peer.notify('ui/editor-text', {surface_id: id, text: ''});
  const cleared = await peer.wait(f => f.method === 'composer/set');
  checkpoint(cleared, id, 3, 4); assert.equal(cleared.params.text, ''); ack(peer, cleared);
  assert.equal(peer.seen.some(f => f.method === 'composer/submit' || f.method === 'session/send_user_message'), false);
  assert.equal(peer.seen.some(f => f.method === 'composer/set' && f.params.text === 'intermediate'), false);
  await peer.close();
});

for (const editor_input of [undefined, { mount_id: 'foreign', input_revision: 1 }, { mount_id: 'host-editor-pi-1', input_revision: 2 }]) {
  test(`missing/foreign/gapped editor input ${JSON.stringify(editor_input)} cannot acknowledge a draft`, async t => {
    const peer = launch(t, [await fixture(t)], { composer: 'seed' });
    await peer.init(); await peer.start();
    const open = await peer.wait(f => f.method === 'ui/open');
    peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'u', kind: 'press', modifiers: [], ...(editor_input && { editor_input }) });
    await peer.wait(f => f.method === 'ui/close');
    assert.equal(peer.seen.some(f => f.method === 'composer/set'), false);
    await peer.close();
  });
}

test('observed rescue remains immediate while a checkpoint is held; queued work and frames stay retired', async t => {
  const entry = await fixture(t, `pi.registerCommand('native', { handler: (_,ctx) => { ctx.ui.setEditorText('native edit'); } });`);
  const peer = launch(t, [entry], { composer: 'seed', hold: ['composer/set'] });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  peer.editorKey(id, 'u');
  const stale = await peer.wait(f => f.method === 'composer/set');
  checkpoint(stale, id, 1, 1);
  peer.editorKey(id, 'l');
  peer.notify('ui/closed', { surface_id: id, reason: 'host rescue' });
  const call = peer.command('native');
  const native = await peer.wait(f => f.method === 'composer/set');
  assert.equal(native.params.text, 'native edit'); assert.equal(native.params.editor_checkpoint, undefined);
  peer.send({ jsonrpc: '2.0', id: native.id, result: {} });
  assert.ok((await call.response).result);
  // A real host rejects the retired mount. Even a late matching ACK cannot
  // publish its captured frame or send the queued second edit after closure.
  ack(peer, stale);
  await peer.wait(f => f.method === 'notification' && /editor mount retired/.test(f.params.message));
  assert.equal(peer.seen.some(frame('seedu')), false);
  assert.equal(peer.seen.some(f => f.method === 'composer/set' && f.params.text === 'seedul'), false);
  await peer.close();
});

test('async editor changes reuse the completed input revision with a new checkpoint revision', async t => {
  const make = `(tui,theme,kb) => {
    const c = new CustomEditor(tui,theme,kb), input = c.handleInput.bind(c);
    c.handleInput = data => { input(data); setTimeout(() => { c.setText(c.getText() + '!'); tui.requestRender(); }, 0); };
    return c;
  }`;
  const peer = launch(t, [await fixture(t, '', make)], { composer: 'seed', hold: ['composer/set'] });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  peer.editorKey(id, 'u');
  const first = await peer.wait(f => f.method === 'composer/set');
  checkpoint(first, id, 1, 1); assert.equal(first.params.text, 'seedu');
  ack(peer, first);
  const timer = await peer.wait(f => f.method === 'composer/set');
  checkpoint(timer, id, 1, 2); assert.equal(timer.params.text, 'seedu!');
  assert.equal(peer.seen.some(frame('seedu!')), false);
  ack(peer, timer); await peer.wait(frame('seedu!')); await peer.close();
});

test('a failing input handler cannot checkpoint or display its intermediate onChange', async t => {
  const make = `(tui,theme,kb) => {
    const c = new CustomEditor(tui,theme,kb);
    c.handleInput = () => { c.setText('partial'); tui.requestRender(); throw new Error('input failed'); };
    return c;
  }`;
  const peer = launch(t, [await fixture(t, '', make)], { composer: 'seed' });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open');
  peer.editorKey(open.params.surface_id, 'u');
  await peer.wait(f => f.method === 'ui/close');
  assert.equal(peer.seen.some(f => f.method === 'composer/set'), false);
  assert.equal(peer.seen.some(frame('partial')), false);
  await peer.close();
});

test('normalized seed is checkpointed before the first frame or successful mount response', async t => {
  const peer = launch(t, [await fixture(t)], { composer: '\tseed', hold: ['composer/set'] });
  await peer.init();
  const start = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
  const open = await peer.wait(f => f.method === 'ui/open');
  const write = await peer.wait(f => f.method === 'composer/set');
  checkpoint(write, open.params.surface_id, 0, 1); assert.equal(write.params.text, '    seed');
  assert.equal(peer.seen.some(f => f.method === 'ui/frame' || f.id === start.id && !f.method), false);
  ack(peer, write); assert.ok((await start.response).result); await peer.wait(frame('    seed'));
  await peer.close();
});

test('cross-factory setEditorText/paste use the actual editor cursor and wait for fenced ACKs', async t => {
  const entry = await fixture(t);
  const commands = join(tmpdir(), `octet-editor-writer-${process.pid}-${Date.now()}.ts`);
  t.after(() => rm(commands, { force: true }));
  await writeFile(commands, `export default pi => {
    pi.registerCommand('set', { handler: async (_,ctx) => { await ctx.ui.setEditorText('AB'); ctx.ui.notify(ctx.ui.getEditorText()); } });
    pi.registerCommand('paste', { handler: async (_,ctx) => { await ctx.ui.pasteToEditor('X'); ctx.ui.notify(ctx.ui.getEditorText()); } });
  };`);
  const peer = launch(t, [entry, commands], { composer: 'seed', hold: ['composer/set'] });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  const set = peer.command('set');
  const first = await peer.wait(f => f.method === 'composer/set');
  checkpoint(first, id, 0, 1); assert.equal(first.params.text, 'AB');
  assert.equal(peer.seen.some(f => f.id === set.id && !f.method), false);
  ack(peer, first); assert.ok((await set.response).result); await peer.wait(frame('AB'));
  peer.editorKey(id, 'ArrowLeft');
  const move = await peer.wait(f => f.method === 'composer/set');
  checkpoint(move, id, 1, 2); ack(peer, move);
  const paste = peer.command('paste');
  const inserted = await peer.wait(f => f.method === 'composer/set');
  checkpoint(inserted, id, 1, 3); assert.equal(inserted.params.text, 'AXB');
  assert.equal(peer.seen.some(f => f.method === 'composer/insert'), false);
  ack(peer, inserted); assert.ok((await paste.response).result);
  await peer.wait(f => f.method === 'notification' && f.params.message === 'AXB');
  await peer.wait(frame('AXB')); await peer.close();
});

// cwd-history reads the editor in session_start, before any editor-state
// snapshot. Pi's editor is empty then; a composer host must not refuse it.
test('getEditorText during session_start reads an empty composer before the first snapshot', async t => {
  const entry = join(tmpdir(), `octet-editor-start-${process.pid}-${Date.now()}.ts`);
  t.after(() => rm(entry, { force: true }));
  await writeFile(entry, `export default pi => pi.on('session_start', (_e, ctx) => { ctx.ui.notify('editor:[' + ctx.ui.getEditorText() + ']'); });`);
  const peer = launch(t, [entry]);
  await peer.init(); await peer.start();
  const notice = await peer.wait(f => f.method === 'notification' && /editor:/.test(f.params.message ?? f.params.title ?? ''));
  assert.match(notice.params.message ?? notice.params.title, /editor:\[\]/);
  await peer.close();
});

// pi-permission-system branches on event.reason; Pi's first session_start
// in a process is "startup" when the host gives no other reason.
test('session_start carries Pi\'s startup reason', async t => {
  const entry = join(tmpdir(), `octet-session-reason-${process.pid}-${Date.now()}.ts`);
  t.after(() => rm(entry, { force: true }));
  await writeFile(entry, `export default pi => pi.on('session_start', (event, ctx) => { ctx.ui.notify('reason:' + event.reason); });`);
  const peer = launch(t, [entry]);
  await peer.init(); await peer.start();
  const notice = await peer.wait(f => f.method === 'notification' && /reason:/.test(f.params.message ?? ''));
  assert.equal(notice.params.message, 'reason:startup');
  await peer.close();
});

test('CustomEditor retains its draft until a native host text decision', async t => {
  const peer = launch(t, [await fixture(t)], { composer: '/model' });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  await peer.wait(frame('/model'));
  // Native refusal leaves the editor untouched; an unchanged echo is not a reset.
  peer.notify('ui/editor-text', {surface_id: id, text: '/model'});
  const unchanged = await peer.wait(f => f.method === 'composer/set');
  assert.equal(unchanged.params.text, '/model');
  peer.notify('ui/editor-text', {surface_id: id, text: ''});
  const cleared = await peer.wait(f => f.method === 'composer/set');
  assert.equal(cleared.params.text, '');
  assert.equal(peer.seen.some(f => f.method === 'composer/submit' || f.method === 'session/send_user_message'), false);
  await peer.close();
});

for (const echo of [false, true]) test(`rejected genuine Pi Editor keeps paste payloads and undo (${echo ? 'unchanged host echo' : 'no host write'})`, async t => {
  const entry = await fixture(t, `let editor;
    pi.registerTool({name: 'state', description: 'inspect editor', parameters: {type: 'object'},
      execute: async () => ({content: [{type: 'text', text: JSON.stringify({
        text: editor.getText(), expanded: editor.getExpandedText(), pastes: [...editor.pastes], undo: editor.undoStack,
      })}]})});
    pi.registerCommand('paste', {handler: () => editor.handleInput('\x1b[200~' + 'paste-line\\n'.repeat(50) + '\x1b[201~')});`,
    '(tui,theme,kb) => editor = new CustomEditor(tui,theme,kb)');
  const peer = launch(t, [entry], { composer: 'prefix' });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  assert.ok((await peer.command('paste').response).result);
  const state = async () => {
    const reply = await peer.request('tool/call', {name: 'state', arguments: {}, context: peer.context()}).response;
    return JSON.parse(reply.result.content[0].text);
  };
  const before = await state();
  assert.ok(before.pastes.length > 0); assert.notEqual(before.text, before.expanded);
  if (echo) {
    peer.notify('ui/editor-text', {surface_id: id, text: before.expanded});
    await peer.wait(f => f.method === 'composer/set' && f.params.text === before.expanded);
  }
  assert.deepEqual(await state(), before, 'native refusal never resets draft/pastes/undo');
  // A subsequent fenced no-op keeps the same genuine component state.
  peer.editorKey(id, 'ArrowLeft', 'release');
  await peer.wait(f => f.method === 'composer/set' && f.params.editor_checkpoint.input_revision === 1);
  assert.deepEqual(await state(), before);
  assert.equal(peer.seen.some(f => f.method === 'ui/close'), false);
  peer.editorKey(id, '-', 'press', ['control']); // Pi's default undo binding.
  await peer.wait(f => f.method === 'composer/set' && f.params.editor_checkpoint.input_revision === 2);
  assert.equal((await state()).text, 'prefix', 'undo restores the pre-paste draft');
  await peer.close();
});

for (const accepted of [false, true]) test(`input received after native ${accepted ? 'acceptance' : 'refusal'} is delivered once`, async t => {
  const peer = launch(t, [await fixture(t)], { composer: 'draft' });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  if (accepted) {
    peer.notify('ui/editor-text', {surface_id: id, text: ''});
    await peer.wait(f => f.method === 'composer/set' && f.params.text === '');
  }
  peer.editorKey(id, 'x'); peer.editorKey(id, 'y');
  const last = await peer.wait(f => f.method === 'composer/set' && f.params.editor_checkpoint.input_revision === 2);
  assert.equal(last.params.text, accepted ? 'xy' : 'draftxy');
  await peer.wait(frame(accepted ? 'xy' : 'draftxy'));
  await peer.close();
});

test('late host clear after retirement cannot mutate the editor or run its callback', async t => {
  const peer = launch(t, [await fixture(t, '', `(tui,theme,kb) => {
    const c = new CustomEditor(tui,theme,kb);
    c.onSubmit = () => { throw new Error('retired submit callback ran'); }; return c;
  }`)], {composer: 'draft'});
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  peer.notify('ui/closed', {surface_id: id, reason: 'owner retired'});
  peer.notify('ui/editor-text', {surface_id: id, text: ''});
  await peer.close();
  assert.equal(peer.seen.some(f => f.method === 'composer/set' && f.params.text === ''), false);
  assert.doesNotMatch(peer.stderr(), /retired submit callback ran/);
});

test('host-resolved editing overrides reach the same slot; no adapter submit path exists', async t => {
  const peer = launch(t, [await fixture(t)], {composer: 'draft'});
  await peer.init();
  await peer.start({keybindings: {...(await import('./helper.mjs')).host.keybindings,
    'tui.editor.cursorLeft': ['ctrl+x'], 'tui.input.submit': ['ctrl+y']}});
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  peer.editorKey(id, 'x', 'press', ['control']);
  await peer.wait(f => f.method === 'composer/set' && f.params.editor_checkpoint.input_revision === 1);
  peer.editorKey(id, '!');
  const edited = await peer.wait(f => f.method === 'composer/set' && f.params.editor_checkpoint.input_revision === 2);
  assert.equal(edited.params.text, 'draf!t');
  assert.equal(peer.seen.some(f => f.method === 'composer/submit'), false);
  await peer.close();
});

test('Ctrl+G is delivered and checkpointed in the composer before native clear and slash input', async t => {
  const make = `(tui,theme,kb) => {
    const c = new CustomEditor(tui,theme,kb), input = c.handleInput.bind(c);
    c.handleInput = data => { if (data === '\\x07') c.setText(c.getText() + 'G'); else input(data); };
    return c;
  }`;
  const peer = launch(t, [await fixture(t, '', make)], {composer: 'draft', hold: ['composer/set']});
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'), id = open.params.surface_id;
  peer.editorKey(id, 'g', 'press', ['control']);
  const controlG = await peer.wait(f => f.method === 'composer/set');
  checkpoint(controlG, id, 1, 1); assert.equal(controlG.params.text, 'draftG'); ack(peer, controlG);
  peer.notify('ui/editor-text', {surface_id: id, text: ''});
  const clear = await peer.wait(f => f.method === 'composer/set');
  checkpoint(clear, id, 1, 2); assert.equal(clear.params.text, ''); ack(peer, clear);
  peer.editorKey(id, '/');
  const slash = await peer.wait(f => f.method === 'composer/set');
  checkpoint(slash, id, 2, 3); assert.equal(slash.params.text, '/'); ack(peer, slash);
  await peer.wait(frame('/'));
  assert.equal(peer.seen.some(f => f.method === 'ui/close'), false);
  await peer.close();
});
