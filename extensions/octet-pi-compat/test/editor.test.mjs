import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

const frame = text => f => f.method === 'ui/frame' && f.params.lines.join('\n').includes(text);
test('custom editor preserves newer local input across delayed host echoes and rescue', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-editor-race-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'editor.ts');
  await writeFile(entry, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => { pi.on('session_start', (_,ctx) => {
  ctx.ui.setEditorComponent((tui,theme,kb) => new CustomEditor(tui,theme,kb));
}); };`);
  const peer = launch(t, [entry], { composer: 'seed', hold: ['composer/set'] });
  await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open');
  await peer.wait(frame('seed'));
  const key = key => peer.notify('ui/key', { surface_id: open.params.surface_id, key, kind: 'press', modifiers: [] });
  key('u');
  const first = await peer.wait(f => f.method === 'composer/set');
  assert.equal(first.params.text, 'seedu');
  key('l'); await peer.wait(frame('seedul'));
  // The host has only committed the first key. Its notification must not
  // overwrite the newer local draft or itself generate another write.
  peer.notify('ui/editor-state', { text: 'seedu', revision: 1, focused: true });
  key('t'); await peer.wait(frame('seedult'));
  peer.send({ jsonrpc: '2.0', id: first.id, result: {} });
  let accepted = 'seedu';
  for (let i = 0; i < 2; i++) {
    const update = await peer.wait(f => f.method === 'composer/set');
    accepted = update.params.text;
    peer.send({ jsonrpc: '2.0', id: update.id, result: {} });
  }
  assert.equal(accepted, 'seedult');
  peer.notify('ui/closed', { surface_id: open.params.surface_id, reason: 'host rescue' });
  await peer.close();
});
