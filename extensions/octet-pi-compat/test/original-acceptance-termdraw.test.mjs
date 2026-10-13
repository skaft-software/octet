// Real unchanged termDRAW factory + Bun island; synthetic host, NOT native/PTY.
import test from 'node:test';
import assert from 'node:assert/strict';
import { launch, owner } from './helper.mjs';
const entry = process.env.PI_TERMDRAW_PATH;
const plain = f => f.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '');
test('original termDRAW constructor capture, real island mouse drawing, save and composer insertion', { skip: !entry && 'set reviewed PI_TERMDRAW_PATH', timeout: 20000 }, async t => {
  const peer = launch(t, [entry], { composer: 'existing draft', columns: 80, rows: 40 });
  await peer.init();
  t.after(() => { if (!t.passed) t.diagnostic(JSON.stringify({ frames: peer.seen, stderr: peer.stderr() })); });
  const command = peer.command('termdraw');
  command.response.catch(() => {}); // Teardown may reject after an earlier assertion; awaited below.
  // Construction needs real geometry, then capture is re-admitted before paint.
  const initial = await peer.wait(f => f.method === 'ui/open');
  assert.equal(initial.params.mouse_capture, false);
  const open = await peer.wait(f => f.method === 'ui/open' && f.params.mouse_capture === true);
  assert.equal(peer.seen.some(f => f.method === 'ui/frame'), false, 'no frame before capture admission');
  assert.ok((await command.response).result);
  // The island can emit its one-shot ready event before the original subscribes.
  // Readiness is the actual canvas, not the potentially stale status footer.
  const canvas = await peer.wait(f => f.method === 'ui/frame' && plain(f).includes('termDRAW!'));
  assert.equal(plain(canvas).split('\n')[8].slice(20, 27), '       ');
  const mouse = (kind, x, y) => peer.notify('ui/mouse', { surface_id: open.params.surface_id, kind, button: 'left', x, y, modifiers: [], wheel_delta: 0 });
  mouse('press', 20, 8); mouse('drag', 26, 8); mouse('release', 26, 8);
  // An observable painted stroke, not mere registration/readiness.
  // The unchanged default LINE/smooth tool draws horizontal box glyphs, not '#'.
  await peer.wait(f => f.method === 'ui/frame' && plain(f).split('\n')[8].slice(20, 27) === '●─────●');
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'Enter', kind: 'press', modifiers: [] });
  await peer.wait(f => f.method === 'ui/close');
  const insert = await peer.wait(f => f.method === 'composer/insert');
  assert.match(insert.params.text, /^\n```text\n/);
  assert.match(insert.params.text, /───────/);
  assert.deepEqual(insert.params.resource_owner, owner);
  await peer.wait(f => f.method === 'notification' && f.params.message === 'Inserted drawing into editor.');
  assert.deepEqual(peer.seen.filter(f => f.method === 'ui/open').map(f => f.params.mouse_capture), [false, true]);
  await peer.close();
});
