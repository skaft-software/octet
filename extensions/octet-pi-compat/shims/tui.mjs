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
export { renderFakeCursor } from '../lib/fake-cursor.mjs';
import { unsupported } from '../lib/errors.mjs';
export class TUI { constructor() { unsupported('new Pi TUI', 'use the host-supplied remote TUI facade'); } }
export class ProcessTerminal { constructor() { unsupported('ProcessTerminal', 'octet always owns the terminal'); } }
export class Image { constructor() { unsupported('terminal image component', 'remote_ui admits printable text and safe SGR, not image escape protocols'); } }

export { MouseRegion } from '../node_modules/@earendil-works/pi-tui/dist/components/mouse-region.js';
export { ScrollView } from '../node_modules/@earendil-works/pi-tui/dist/components/scroll-view.js';
export { HStack } from '../node_modules/@earendil-works/pi-tui/dist/components/h-stack.js';
export { VStack } from '../node_modules/@earendil-works/pi-tui/dist/components/v-stack.js';
export { backgroundAnsi, colorToHex, colorToOkhsl, colorToOklch, colorToRgb, foregroundAnsi, indexedColor, mixColors, okhslColor, oklchColor, parseColor, rgbColor, styleText, styleTextWithAnsi } from '../node_modules/@earendil-works/pi-tui/dist/colors.js';
export { oklabToOkhslLightness } from '../node_modules/@earendil-works/pi-tui/dist/oklab.js';
export { renderLatex } from '../node_modules/@earendil-works/pi-tui/dist/latex.js';
export { compositeTuiLine, isViewportTUI } from '../node_modules/@earendil-works/pi-tui/dist/tui.js';
export { parseTerminalColorSchemeReport } from '../node_modules/@earendil-works/pi-tui/dist/terminal-colors.js';
export { allocateImageId, calculateImageRows, deleteAllKittyImages, deleteKittyImage, encodeITerm2, encodeKitty, getCellDimensions, getGifDimensions, getImageDimensions, getJpegDimensions, getPngDimensions, getWebpDimensions, hyperlink, imageFallback, resetCapabilitiesCache, setCapabilities, setCapabilityOverrides, setCellDimensions } from '../node_modules/@earendil-works/pi-tui/dist/terminal-image.js';
export { setImageTranscoder } from '../node_modules/@earendil-works/pi-tui/dist/components/image.js';
export { getOsc8LinkAtColumn } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';
export { isAppleTerminalSession } from '../node_modules/@earendil-works/pi-tui/dist/terminal.js';
export { StdinBuffer } from '../node_modules/@earendil-works/pi-tui/dist/stdin-buffer.js';
export { Marked } from '../node_modules/@earendil-works/pi-tui/dist/index.js';
import { refused } from './local-pi.mjs';
export const CombinedAutocompleteProvider = refused('pi-tui.CombinedAutocompleteProvider');
export const TuiAltScreen = refused('pi-tui.TuiAltScreen');
export const TuiMainScreen = refused('pi-tui.TuiMainScreen');
export const getNativeClipboard = refused('pi-tui.getNativeClipboard');
export const detectCapabilities = refused('pi-tui.detectCapabilities');
export const getCapabilities = refused('pi-tui.getCapabilities');
export const getTerminalColorMode = refused('pi-tui.getTerminalColorMode');
export const renderImage = refused('pi-tui.renderImage');
