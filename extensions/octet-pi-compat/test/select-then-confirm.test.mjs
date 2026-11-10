import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, root } from './helper.mjs';

const fixture = join(root, 'test/fixtures/select-then-confirm.ts');
const key = (peer, open, name) => peer.notify('ui/key', { surface_id: open.params.surface_id, key: name, kind: 'press', modifiers: [] });

// Regression: a command that awaits ctx.ui.select(...) and then ctx.ui.confirm(...)
// (Pi's examples/extensions/commands.ts) failed with "not_foreground_owner
// confirmation/request requires a live request". The first dialog's surface detaches the
// command from its request; the second dialog must stay on the remote surface.
test('confirm after select in one command opens a second dialog instead of failing', async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const command = peer.command('pick-then-confirm');
  const first = await peer.wait(f => f.method === 'ui/open'); await command.response;
  key(peer, first, 'Enter');
  const second = await peer.wait(f => f.method === 'ui/open' && f.params.surface_id !== first.params.surface_id);
  key(peer, second, 'Enter');
  const done = await peer.wait(f => f.method === 'notification' && /^(result:|Pi compatibility error)/.test(f.params.message));
  assert.equal(done.params.message, 'result:one:true');
});

test('input after select in one command opens a second dialog instead of failing', async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const command = peer.command('pick-then-input');
  const first = await peer.wait(f => f.method === 'ui/open'); await command.response;
  key(peer, first, 'Enter');
  const second = await peer.wait(f => f.method === 'ui/open' && f.params.surface_id !== first.params.surface_id);
  key(peer, second, 'Enter');
  const done = await peer.wait(f => f.method === 'notification' && /^(typed:|Pi compatibility error)/.test(f.params.message));
  assert.equal(done.params.message, 'typed:one:');
});
