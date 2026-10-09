import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner, host } from './helper.mjs';

test('native-backed custom editor can read Pi getEffectiveConfig and keeps handling draft input', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-effective-keybindings-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'editor.ts');
  await writeFile(entry, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
import { getKeybindings } from '@earendil-works/pi-tui';
export default pi => pi.on('session_start', (_, ctx) => ctx.ui.setEditorComponent((tui, theme, kb) => {
  const effective = kb.getEffectiveConfig();
  if (JSON.stringify(effective) !== JSON.stringify(kb.getResolvedBindings())) throw new Error('effective and resolved bindings differ');
  if (JSON.stringify(effective) !== JSON.stringify(getKeybindings().getEffectiveConfig())) throw new Error('global effective bindings differ');
  ctx.ui.notify('effective:' + JSON.stringify(effective['app.exit']));
  return new CustomEditor(tui, theme, kb);
}));`);
  const peer = launch(t, [entry], { composer: 'draft', hold: ['composer/set'] });
  const effective = { ...host.keybindings, 'app.exit': ['ctrl+q'], 'tui.editor.cursorLeft': ['left'] };
  await peer.init();
  const start = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context({ keybindings: effective }) });
  await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'editor').catch(error => { throw new Error(`${error}; frames=${JSON.stringify(peer.seen)}`); });
  const note = await peer.wait(f => f.method === 'notification' && f.params.message.startsWith('effective:')).catch(error => { throw new Error(`${error}; frames=${JSON.stringify(peer.seen)}`); });
  assert.equal(note.params.message, 'effective:["ctrl+q"]');
  const id = peer.seen.find(f => f.method === 'ui/open' && f.params.placement === 'editor').params.surface_id;
  peer.editorKey(id, 'x');
  const checkpoint = await peer.wait(f => f.method === 'composer/set').catch(error => { throw new Error(`${error}; frames=${JSON.stringify(peer.seen)}`); });
  assert.equal(checkpoint.params.text, 'draftx');
  peer.send({ jsonrpc: '2.0', id: checkpoint.id, result: {
    input_revision: checkpoint.params.editor_checkpoint.input_revision,
    checkpoint_revision: checkpoint.params.editor_checkpoint.checkpoint_revision,
  } });
  assert.ok((await start.response).result);
  await peer.close();
});
