// Pi 1.5.0 pi-tui dist/tui.js:71-72 (markers), :76-78 (renderFakeCursor), :1089-1092 (resolveFakeCursors).
// A fake cursor is the text under the cursor wrapped in zero-width APC markers. Pi draws it as reverse
// video unless its hardware cursor is shown; octet has no component-owned hardware cursor, so the
// reverse-video form is the one a frame carries.
export const FAKE_CURSOR_START = '\x1b_pi:fc\x07';
export const FAKE_CURSOR_END = '\x1b_pi:/fc\x07';
export const renderFakeCursor = text => `${FAKE_CURSOR_START}${text}${FAKE_CURSOR_END}`;
export const resolveFakeCursors = line => line.includes(FAKE_CURSOR_START)
  ? line.replaceAll(FAKE_CURSOR_START, '\x1b[7m').replaceAll(FAKE_CURSOR_END, '\x1b[27m') : line;
