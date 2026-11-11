import test from 'node:test';
import assert from 'node:assert/strict';
import { renderFakeCursor } from '../shims/tui.mjs';
import { safeLines } from '../lib/remote-ui.mjs';

// Pi 1.5.0 pi-tui dist/tui.js:76 (renderFakeCursor) and :1089-1092 (resolveFakeCursors): overlay-test.ts
// draws its input cursor with renderFakeCursor, which Pi paints as reverse video.
test('pi-tui renderFakeCursor is exported and a frame carries it as reverse video', () => {
  assert.equal(typeof renderFakeCursor, 'function');
  assert.equal(renderFakeCursor('x'), '\x1b_pi:fc\x07x\x1b_pi:/fc\x07');
  assert.deepEqual(safeLines([`before${renderFakeCursor('x')}after`]), ['before\x1b[7mx\x1b[27mafter']);
  assert.deepEqual(safeLines(['plain']), ['plain']);
});
