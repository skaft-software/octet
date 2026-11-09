import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';
import { Runtime } from '../lib/runtime.mjs';
import { createContext } from '../lib/api.mjs';

test('optional SessionManager probes in footer and widget renderers do not fail startup or retire the editor', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-session-manager-ui-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'surfaces.ts');
  await writeFile(entry, `import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => {
  pi.on('session_start', (_, ctx) => {
    ctx.ui.setEditorComponent((tui, theme, keys) => new CustomEditor(tui, theme, keys));
    const count = () => typeof ctx.sessionManager.getEntryCount === 'function'
      ? ctx.sessionManager.getEntryCount() : ctx.sessionManager.getEntries().length;
    ctx.ui.setFooter(() => ({ render: () => ['footer entries ' + count()], invalidate() {} }));
    ctx.ui.setWidget('status', () => ({ render: () => ['widget entries ' + count()], invalidate() {} }));
  });
};`);
  const peer = launch(t, [entry], { composer: 'retained draft' });
  await peer.init();
  await peer.start({ session_entries: [], session_branch: [], session_leaf_id: null });
  const opens = peer.seen.filter(f => f.method === 'ui/open');
  assert.deepEqual(opens.map(f => f.params.placement).sort(), ['above_editor', 'editor', 'footer']);
  for (const [placement, text] of [['editor', 'retained draft'], ['footer', 'footer entries 0'], ['above_editor', 'widget entries 0']]) {
    const surface = opens.find(f => f.params.placement === placement).params.surface_id;
    await peer.wait(f => f.method === 'ui/frame' && f.params.surface_id === surface && f.params.lines.join('\n').includes(text));
  }
  assert.equal(peer.seen.some(f => f.method === 'ui/close'), false);
  assert.equal(peer.stderr(), '');
  await peer.close();
});

test('optional SessionManager absence preserves real getter validation and retired-owner fences', () => {
  const runtime = new Runtime({ extensions: [] }, { notify: async () => {} });
  runtime.uninstallChildren();
  const store = { id: 1, method: 'hook/run', controller: new AbortController(), pending: new Set(), errors: [], live: true };
  runtime.bind({ context: { workspace: '/workspace', resource_owner: owner, host: {} } }, store, { foreground: true });
  const manager = createContext(runtime, store).sessionManager;
  assert.equal(typeof manager.getEntryCount, 'undefined');
  assert.equal(typeof manager.futureOptionalMethod, 'undefined');
  // No publication yet is Pi's own empty session, not a compatibility refusal.
  assert.deepEqual(manager.getEntries(), []);
  assert.deepEqual(manager.getBranch(), []);
  store.state.host.session_entries = [];
  store.state.host.session_branch = [];
  assert.deepEqual(manager.getEntries(), []);
  store.state.host.session_entries = null;
  assert.throws(() => manager.getEntries(), /native session view unavailable/);
  store.state.host.session_entries = [];
  store.state.alive = false;
  assert.throws(() => manager.getEntries(), /not_foreground_owner/);
});
