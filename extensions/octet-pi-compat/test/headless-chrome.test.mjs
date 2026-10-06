// Pi's print/headless UI context exists and its chrome setters do nothing
// there. Refusing them made factories that call `ctx.ui.setWidget`/`setTitle`
// unconditionally at session_start look unsupported (pi-plan-mode), while the
// interactive host still mounts every requested surface.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

function factory(t, source) {
  const directory = mkdtempSync(join(tmpdir(), 'pi-headless-chrome-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.mjs');
  writeFileSync(entry, source);
  return entry;
}

const bound = { binding: { session_id: 'host-issued-owner', extension_instance_id: 'host-issued-instance', process_generation: 2 } };
const issue = frame => frame.method === 'notification' && frame.params.title === '[Extension issues]';

test('headless chrome setters are inert, not unsupported, and never mount remote surfaces', async t => {
  const entry = factory(t, `export default pi => {
    pi.on('session_start', (_event, ctx) => {
      ctx.ui.setWidget('plan', ['line one']);
      ctx.ui.setStatus('plan', 'active');
      ctx.ui.setTitle('plan mode');
      ctx.ui.setWorkingMessage('planning');
      ctx.ui.setWorkingVisible(true);
      ctx.ui.setWorkingIndicator({ frames: ['-'], intervalMs: 100 });
      ctx.ui.setHiddenThinkingLabel('thinking');
      ctx.ui.setFooter(() => ({ render: () => [], invalidate() {} }));
      ctx.ui.setHeader(() => ({ render: () => [], invalidate() {} }));
      ctx.ui.setEditorComponent(() => ({ render: () => [], handleInput() {}, invalidate() {} }));
      const before = ctx.ui.getToolsExpanded();
      ctx.ui.setToolsExpanded(true);
      pi.appendProbe = before;
    });
    pi.registerCommand('expanded', { handler: (_args, ctx) => ctx.ui.notify(pi.appendProbe + ':' + ctx.ui.getToolsExpanded()) });
  };`);
  const peer = launch(t, [entry]);
  await peer.init(['session_entries']);
  const start = await peer.request('hook/run', { hook: 'session_start', payload: bound, context: peer.context() }).response;
  assert.ok(start.result, JSON.stringify(start));
  assert.equal(peer.seen.some(issue), false, JSON.stringify(peer.seen.filter(issue)));
  assert.equal(peer.seen.some(frame => frame.method === 'ui/open'), false, 'headless mount');
  assert.equal(peer.seen.some(frame => frame.method === 'ui/chrome'), false, 'headless chrome write');
  const command = peer.command('expanded');
  assert.ok((await command.response).result, 'headless command ran');
  const notice = await peer.wait(frame => frame.method === 'notification' && frame.params.message === 'false:true');
  assert.equal(notice.params.level, 'info');
  await peer.close();
});

test('headless dialogs still refuse: only presentation setters are inert', async t => {
  const entry = factory(t, `export default pi => {
    pi.registerCommand('dialog', { handler: async (_args, ctx) => { await ctx.ui.custom(() => ({ render: () => [], invalidate() {} })); } });
    pi.registerCommand('theme', { handler: (_args, ctx) => ctx.ui.getAllThemes() });
  };`);
  const peer = launch(t, [entry]);
  await peer.init(['session_entries']);
  const dialog = await peer.command('dialog').response;
  assert.equal(dialog.error?.code, -32601, JSON.stringify(dialog));
  assert.match(dialog.error.message, /unsupported_feature remote_ui/);
  const themes = await peer.command('theme').response;
  assert.equal(themes.error?.code, -32601, JSON.stringify(themes));
  assert.match(themes.error.message, /unsupported_feature ctx\.ui\.theme/);
  await peer.close();
});

test('an interactive host still mounts every requested surface', async t => {
  const entry = factory(t, `export default pi => {
    pi.on('session_start', (_event, ctx) => {
      ctx.ui.setWidget('plan', ['line one']);
      ctx.ui.setStatus('plan', 'active');
      ctx.ui.setFooter(() => ({ render: () => [], invalidate() {} }));
      ctx.ui.setHeader(() => ({ render: () => [], invalidate() {} }));
    });
  };`);
  const peer = launch(t, [entry]);
  await peer.init(['session_entries', 'remote_ui']);
  const start = await peer.request('hook/run', { hook: 'session_start', payload: bound, context: peer.context() }).response;
  assert.ok(start.result, JSON.stringify(start));
  assert.equal(peer.seen.some(issue), false, JSON.stringify(peer.seen.filter(issue)));
  const placements = peer.seen.filter(frame => frame.method === 'ui/open').map(frame => frame.params.placement);
  for (const placement of ['above_editor', 'footer', 'header']) assert.ok(placements.includes(placement), JSON.stringify(placements));
  await peer.close();
});
