// Selected MIT Pi library modules only. Never expose its ProcessTerminal or
// differential TUI renderer: the Rust frontend owns terminal IO and focus.
export { Box } from '../node_modules/@earendil-works/pi-tui/dist/components/box.js';
export { Text } from '../node_modules/@earendil-works/pi-tui/dist/components/text.js';
export { TruncatedText } from '../node_modules/@earendil-works/pi-tui/dist/components/truncated-text.js';
export { Spacer } from '../node_modules/@earendil-works/pi-tui/dist/components/spacer.js';
export { Input } from '../node_modules/@earendil-works/pi-tui/dist/components/input.js';
export { Editor } from '../lib/editor.mjs';
export { SelectList } from '../node_modules/@earendil-works/pi-tui/dist/components/select-list.js';
export { SettingsList } from '../node_modules/@earendil-works/pi-tui/dist/components/settings-list.js';
export { Markdown } from '../node_modules/@earendil-works/pi-tui/dist/components/markdown.js';
export { Loader } from '../node_modules/@earendil-works/pi-tui/dist/components/loader.js';
export { CancellableLoader } from '../node_modules/@earendil-works/pi-tui/dist/components/cancellable-loader.js';
export { fuzzyFilter, fuzzyMatch } from '../node_modules/@earendil-works/pi-tui/dist/fuzzy.js';
export { Key, matchesKey, parseKey, isKeyRelease, isKeyRepeat, decodeKittyPrintable, isKittyProtocolActive, setKittyProtocolActive } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';
export { KeybindingsManager, getKeybindings, setKeybindings, TUI_KEYBINDINGS } from '../node_modules/@earendil-works/pi-tui/dist/keybindings.js';
export { visibleWidth, truncateToWidth, sliceByColumn, stripTerminalSequences, wrapTextWithAnsi } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';
// These component-only exports do not start or instantiate the Pi terminal.
export { Container, CURSOR_MARKER, isFocusable } from '../node_modules/@earendil-works/pi-tui/dist/tui.js';
import { unsupported } from '../lib/errors.mjs';
export class TUI { constructor() { unsupported('new Pi TUI', 'use the host-supplied remote TUI facade'); } }
export class ProcessTerminal { constructor() { unsupported('ProcessTerminal', 'octet always owns the terminal'); } }
export class Image { constructor() { unsupported('terminal image component', 'remote_ui admits printable text and safe SGR, not image escape protocols'); } }
