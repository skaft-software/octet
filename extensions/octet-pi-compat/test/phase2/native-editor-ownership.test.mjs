import assert from 'node:assert/strict';
import test from 'node:test';
import { Editor } from '../../shims/tui.mjs';
import { CustomEditor } from '../../lib/custom-editor.mjs';
import { Editor as PiEditor } from '../../node_modules/@earendil-works/pi-tui/dist/components/editor.js';

// This is intentionally red until phase 2 is implemented. The 193 upstream
// cases can pass against the original Pi engine; that is not native parity.
// Check the actual exports used by extension factories, including CustomEditor,
// rather than a test-only replacement or a claimed feature flag.
test('phase-2 Editor exports must not use or inherit Pi’s JavaScript editor engine', () => {
  assert.notEqual(Editor, PiEditor,
    'native-backed facade missing: the adapter still exports Pi Editor directly');
  assert.equal(PiEditor.prototype.isPrototypeOf(Editor.prototype), false,
    'a subclass of Pi Editor still owns editing in JavaScript');
  assert.equal(PiEditor.prototype.isPrototypeOf(CustomEditor.prototype), false,
    'CustomEditor must use the same native-backed facade');
  assert.equal(Object.getPrototypeOf(CustomEditor.prototype), Editor.prototype,
    'CustomEditor and pi-tui Editor must share one native facade');
});
