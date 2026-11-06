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

test('headless custom() resolves undefined like Pi instead of waiting for a surface', async t => {
  const entry = factory(t, `export default pi => {
    pi.registerCommand('dialog', { handler: async (_args, ctx) => {
      const value = await ctx.ui.custom(() => ({ render: () => [], invalidate() {} }));
      ctx.ui.notify('custom:' + String(value));
    } });
    pi.registerCommand('confirm', { handler: async (_args, ctx) => {
      ctx.ui.notify('confirm:' + String(await ctx.ui.confirm('t', 'm')));
    } });
  };`);
  const peer = launch(t, [entry]);
  await peer.init(['session_entries']);
  // Pi's noOpUIContext.custom() resolves undefined, confirm() resolves false:
  // a print-mode hook must never wait for a foreground surface, or the whole
  // hook round trip times out and the tool call fails.
  await peer.command('dialog').response;
  await peer.command('confirm').response;
  assert.equal(peer.seen.some(frame => frame.method === 'ui/open'), false, 'headless mount');
  const values = peer.seen.filter(frame => frame.method === 'notification').map(frame => frame.params.message);
  assert.deepEqual(values, ['custom:undefined', 'confirm:false']);
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
