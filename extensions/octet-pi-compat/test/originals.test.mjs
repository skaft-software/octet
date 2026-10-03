// Real acceptance targets are loaded unchanged from user-provided paths. No GPL
// game code/WAD or third-party extension implementation is copied into octet.
import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { launch, owner, host } from './helper.mjs';
// Never discover and execute factories from a user's HOME or a developer's /tmp.
// Setting these paths explicitly authorizes the corresponding reviewed probe.
const doom = process.env.PI_DOOM_PATH || '';
const wad = process.env.PI_DOOM_WAD || '';
const footer = process.env.PI_FOOTER_PATH || '';
const draw = process.env.PI_DRAW_PATH || '';
const rainbow = process.env.PI_RAINBOW_PATH || '';
const frame = text => f => f.method === 'ui/frame' && f.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '').includes(text);

test('unchanged Doom WASM: animated RGB snapshots, input/release, resize, pause and persistent resume', { skip: !existsSync(doom) || !existsSync(wad) ? 'set PI_DOOM_PATH and PI_DOOM_WAD to the built upstream package' : false }, async t => {
  const peer = launch(t, [doom]); await peer.init();
  const call = peer.command('doom', [wad]); const open = await peer.wait(f => f.method === 'ui/open');
  assert.ok((await call.response).result);
  const first = await peer.wait(frame('DOOM |'));
  assert.match(first.params.lines[0], /\x1b\[38;2;\d+;\d+;\d+m/);
  await peer.wait(f => f.method === 'ui/frame' && f.params.revision > first.params.revision + 1);
  for (const kind of ['press', 'repeat', 'release']) peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'w', kind, modifiers: [] });
  peer.notify('ui/resize', { surface_id: open.params.surface_id, columns: 60, rows: 22 });
  await peer.wait(f => f.method === 'ui/frame' && f.params.columns === 60 && f.params.rows === 22);
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'q', kind: 'press', modifiers: [] }); await peer.wait(f => f.method === 'ui/close');
  const resume = peer.command('doom', [wad]); await peer.wait(f => f.method === 'notification' && f.params.message === 'Resuming DOOM...'); await resume.response;
  const reopened = await peer.wait(f => f.method === 'ui/open');
  peer.notify('ui/closed', { surface_id: reopened.params.surface_id, reason: 'rescue' }); await peer.close();
});
test('unchanged powerline-footer 0.5.4: real factory, owner-retained getters and host-state replacement', { skip: existsSync(footer) ? false : 'set PI_FOOTER_PATH to the unchanged installed extension' }, async t => {
  const peer = launch(t, [footer], { columns: 140 }); await peer.init();
  const facts = { context_usage: { tokens: 8000, contextWindow: 32768, percent: 24.4 }, session_entries: [{ type: 'message', message: { role: 'assistant', usage: { cost: { total: 0.125 } } } }], using_oauth: false };
  await peer.start(facts);
  const open = await peer.wait(f => f.method === 'ui/open'); assert.equal(open.params.placement, 'footer');
  const first = await peer.wait(frame('Test Model')); assert.match(first.params.lines[0], /8K\/32K/); assert.match(first.params.lines[0], /\$0\.125/);
  peer.notify('context/updated', { resource_owner: owner, host: { ...host, ...facts, model_view: { ...host.model_view, name: 'Updated Model' }, session_name: 'Updated Session' } });
  const updated = await peer.wait(frame('Updated Model')); assert.match(updated.params.lines[0], /\[Updated Session\]/);
  peer.notify('ui/resize', { surface_id: open.params.surface_id, columns: 80, rows: 24 }); await peer.wait(f => f.method === 'ui/frame' && f.params.columns === 80);
  await peer.close();
});
test('unchanged Ben Vinegar drawing: reviewed mouse intent, SGR drag/release, real export to host composer', { skip: existsSync(draw) ? false : 'set PI_DRAW_PATH to unchanged pi-stuff draw.ts' }, async t => {
  const peer = launch(t, [draw], { columns: 80, rows: 24, composer: 'draft' }); await peer.init(); await peer.start();
  const call = peer.command('draw'); const open = await peer.wait(f => f.method === 'ui/open'); assert.equal(open.params.mouse_capture, true); await call.response;
  await peer.wait(frame('/draw'));
  const mouse = (kind, x, y) => peer.notify('ui/mouse', { surface_id: open.params.surface_id, kind, button: 'left', x, y, modifiers: [], wheel_delta: 0 });
  mouse('press', 2, 5); mouse('drag', 6, 5); mouse('release', 6, 5);
  await peer.wait(frame('#####'));
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'Enter', kind: 'press', modifiers: [] });
  await peer.wait(f => f.method === 'ui/close');
  const insert = await peer.wait(f => f.method === 'composer/insert');
  assert.match(insert.params.text, /^\n```text\n/); assert.match(insert.params.text, /#####/); assert.deepEqual(insert.params.resource_owner, owner);
  await peer.wait(f => f.method === 'notification' && f.params.message === 'Inserted drawing into editor.');
  assert.doesNotMatch(peer.stderr(), /\x1b\[\?100[026][hl]/); await peer.close();
});
test('unchanged Doom and footer coexist in one Node process with independent retained components', { skip: !existsSync(doom) || !existsSync(wad) || !existsSync(footer) ? 'original Doom/footer assets unavailable' : false }, async t => {
  const peer = launch(t, [doom, footer], { columns: 100 }); await peer.init(); await peer.start();
  const footerOpen = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'footer'); await peer.wait(frame('Test Model'));
  const command = peer.command('doom', [wad]); const gameOpen = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'fullscreen'); await command.response;
  await peer.wait(frame('DOOM |'));
  peer.notify('ui/resize', { surface_id: footerOpen.params.surface_id, columns: 90, rows: 24 });
  await peer.wait(f => frame('Test Model')(f) && f.params.surface_id === footerOpen.params.surface_id && f.params.columns === 90);
  peer.notify('ui/closed', { surface_id: gameOpen.params.surface_id, reason: 'rescue' });
  peer.notify('context/updated', { resource_owner: owner, host: { ...host, session_name: 'Still alive' } }); await peer.wait(frame('[Still alive]'));
  await peer.close();
});

test('unchanged rainbow CustomEditor: actual editor input, animated RGB frames and draft mirroring', { skip: existsSync(rainbow) ? false : 'set PI_RAINBOW_PATH to unchanged rainbow-editor.ts' }, async t => {
  const peer = launch(t, [rainbow], { composer: '' }); await peer.init(); await peer.start();
  const open = await peer.wait(f => f.method === 'ui/open'); assert.equal(open.params.placement, 'editor');
  peer.notify('ui/editor-state', { text: '', revision: 1, focused: false });
  for (const key of 'ultrathink') peer.notify('ui/key', { surface_id: open.params.surface_id, key, kind: 'press', modifiers: [] });
  const first = await peer.wait(f => f.method === 'ui/frame' && /\x1b\[38;2;/.test(f.params.lines.join('')));
  const later = await peer.wait(f => f.method === 'ui/frame' && f.params.revision > first.params.revision + 1 && f.params.lines.join('') !== first.params.lines.join(''));
  assert.ok(later.params.lines.length > 0);
  const set = await peer.wait(f => f.method === 'composer/set' && f.params.text === 'ultrathink'); assert.deepEqual(set.params.resource_owner, owner);
  peer.notify('ui/resize', { surface_id: open.params.surface_id, columns: 60, rows: 20 }); await peer.wait(f => f.method === 'ui/frame' && f.params.columns === 60);
  peer.notify('ui/closed', { surface_id: open.params.surface_id, reason: 'rescue' }); await peer.close();
});
