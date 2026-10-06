import test from 'node:test';
import assert from 'node:assert/strict';
import * as theme from '../lib/theme.mjs';

test('custom UI keybindings project native action IDs and overrides without defaults', () => {
  let snapshot = { 'app.tools.expand': ['ctrl+x'], 'tui.select.confirm': [], 'tui.select.cancel': ['alt+q'] };
  const keys = theme.hostKeybindings(() => snapshot);
  assert.equal(keys.matches('\x18', 'app.tools.expand'), true);
  assert.equal(keys.matches('\x0f', 'app.tools.expand'), false);
  assert.equal(keys.matches('\r', 'tui.select.confirm'), false);
  assert.deepEqual(keys.getKeys('tui.select.confirm'), []);
  const copy = keys.getKeys('app.tools.expand'); copy.push('ctrl+o');
  assert.deepEqual(keys.getKeys('app.tools.expand'), ['ctrl+x']);
  snapshot = { 'app.tools.expand': ['ctrl+o'] };
  assert.equal(keys.matches('\x0f', 'app.tools.expand'), true);
  assert.throws(() => keys.matches('x', 'unknown.action'), /host binding not supplied/);
});

test('missing native snapshot refuses instead of inventing defaults', () => {
  const keys = theme.hostKeybindings(() => undefined);
  assert.throws(() => keys.matches('\r', 'tui.select.confirm'), /host binding not supplied/);
});
