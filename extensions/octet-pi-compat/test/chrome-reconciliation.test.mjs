import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

for (const action of ['omit', 'clear', 'mount']) test(`startup chrome reconciliation includes ${action} slots`, async t => {
  const directory = mkdtempSync(join(tmpdir(), 'pi-chrome-reconcile-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.mjs');
  writeFileSync(entry, `export default pi => pi.on('session_start', (_event, ctx) => {
    ${action === 'clear' ? 'ctx.ui.setFooter(undefined);' : action === 'mount' ? "ctx.ui.setFooter(() => ({render: () => ['live footer'], invalidate() {}}));" : ''}
  });`);
  const h = launch(t, [entry]);
  await h.init();
  const start = h.request('hook/run', {hook: 'session_start', payload: {binding: owner}, context: h.context()});
  const reconciled = await h.wait(f => f.method === 'ui/chrome' && f.params.chrome.kind === 'reconciled', 1500);
  assert.deepEqual(reconciled.params.resource_owner, owner);
  if (action === 'mount') assert.ok(h.seen.some(f => f.method === 'ui/frame'), 'first frame precedes reconciliation');
  else assert.ok(!h.seen.some(f => f.method === 'ui/open' || f.method === 'ui/close'), 'no synthetic slot required');
  assert.ok((await start.response).result);
  await h.close();
});
