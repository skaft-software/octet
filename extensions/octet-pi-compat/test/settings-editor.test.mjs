import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

// Settings-only subclasses are genuine Pi editors too. Never probe a factory
// on a fake TUI, discard its instance, or substitute a host history/menu.
async function extension(t, body) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-settings-editor-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'editor.ts');
  await writeFile(path, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
${body}
export default pi => pi.on('session_start', (_, ctx) => {
  let constructions = 0;
  const factory = (tui, theme, kb) => {
    ctx.ui.notify(JSON.stringify({ factory: ++constructions, columns: tui.terminal.columns, rows: tui.terminal.rows,
      realTheme: typeof theme.borderColor === 'function' && typeof theme.selectList.selectedText === 'function', realKeys: typeof kb.getKeys === 'function' }));
    const editor = make(tui, theme, kb);
    editor.addToHistory('older prompt'); editor.addToHistory('newer prompt');
    return editor;
  };
  ctx.ui.setEditorComponent(factory);
  ctx.ui.notify('editor:' + (ctx.ui.getEditorComponent() === factory ? 'original' : 'wrong'));
});`);
  return path;
}
const frame = text => f => f.method === 'ui/frame' && f.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '').includes(text);

test('settings-only Pi editor mounts the original factory once with real admitted geometry/theme/keybindings', async t => {
  const entry = await extension(t, `class HistoryEditor extends CustomEditor {
  locked = false;
  lockBorderColor() { this.locked = true; }
}
const make = (tui, theme, kb) => { const e = new HistoryEditor(tui, theme, kb); e.borderColor = s => s; e.lockBorderColor(); return e; };`);
  const peer = launch(t, [entry], { columns: 97, rows: 31 });
  await peer.init(); await peer.start();
  await peer.wait(f => f.method === 'notification' && f.params.message === 'editor:original');
  await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'editor');
  const factory = await peer.wait(f => f.method === 'notification' && f.params.message.startsWith('{"factory":'));
  assert.deepEqual(JSON.parse(factory.params.message), { factory: 1, columns: 97, rows: 31, realTheme: true, realKeys: true });
  assert.equal(peer.seen.filter(f => f.method === 'notification' && f.params.message.startsWith('{"factory":')).length, 1);
  await peer.close();
});

test('settings-only history remains on the actual editor and is recalled newest-first', async t => {
  const entry = await extension(t, 'const make = (tui, theme, kb) => new CustomEditor(tui, theme, kb);');
  const peer = launch(t, [entry], { composer: '' });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'editor');
  peer.editorKey(open.params.surface_id, 'ArrowUp');
  await peer.wait(frame('newer prompt'));
  peer.editorKey(open.params.surface_id, 'ArrowUp');
  await peer.wait(frame('older prompt'));
  assert.equal(peer.seen.some(f => f.method === 'composer/history'), false, 'no surrogate native editor history transfer');
  await peer.close();
});

test('a refused actual editor mount cannot settle the opening hook successfully or invoke its factory', async t => {
  const entry = await extension(t, 'const make = (tui, theme, kb) => new CustomEditor(tui, theme, kb);');
  const peer = launch(t, [entry], { hold: ['ui/open'] });
  await peer.init();
  const start = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
  const open = await peer.wait(f => f.method === 'ui/open');
  peer.send({ jsonrpc: '2.0', id: open.id, error: { code: -32002, message: 'not_foreground_owner editor owner retired' } });
  assert.equal((await start.response).error?.code, -32002);
  assert.equal(peer.seen.some(f => f.method === 'notification' && f.params.message.startsWith('{"factory":')), false);
  await peer.close();
});

test('an editor that overrides input still mounts the original Pi editor exactly once', async t => {
  const entry = await extension(t, `class VimEditor extends CustomEditor {
  handleInput(data) { return super.handleInput(data); }
}
const make = (tui, theme, kb) => new VimEditor(tui, theme, kb);`);
  const peer = launch(t, [entry]);
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'editor');
  assert.equal(open.params.placement, 'editor');
  assert.equal(peer.seen.filter(f => f.method === 'notification' && f.params.message.startsWith('{"factory":')).length, 1);
  await peer.close();
});
