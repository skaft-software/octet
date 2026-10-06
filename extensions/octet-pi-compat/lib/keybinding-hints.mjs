// Pi 1.0.2 `modes/interactive/components/keybinding-hints.ts`, unchanged
// semantics, against the pinned pi-tui release this adapter also ships. On the
// installed-Pi route the shared pi-tui keybinding manager is the host-supplied
// dispatcher, so these helpers report the host's real bindings there too.
import { getKeybindings } from '../node_modules/@earendil-works/pi-tui/dist/keybindings.js';
import { theme } from './theme.mjs';

function formatKeyPart(part, options) {
  const displayPart = process.platform === 'darwin' && part.toLowerCase() === 'alt' ? 'option' : part;
  return options.capitalize ? displayPart.charAt(0).toUpperCase() + displayPart.slice(1) : displayPart;
}
export function formatKeyText(key, options = {}) {
  return key.split('/').map(k => k.split('+').map(part => formatKeyPart(part, options)).join('+')).join('/');
}
function formatKeys(keys, options) {
  return keys.length === 0 ? '' : formatKeyText(keys.join('/'), options);
}
export function keyText(keybinding) { return formatKeys(getKeybindings().getKeys(keybinding)); }
export function keyHint(binding, description) {
  return theme.fg('dim', keyText(binding)) + theme.fg('muted', ` ${description}`);
}
export function rawKeyHint(key, description) {
  return theme.fg('dim', formatKeyText(key)) + theme.fg('muted', ` ${description}`);
}
