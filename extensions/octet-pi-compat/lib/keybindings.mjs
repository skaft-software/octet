// Pi components ask their module's keybinding manager at input time. Install a
// read-only dispatcher, never a mutable copy of one foreground owner's keys.
import { fileURLToPath } from 'node:url';
import { setKeybindings } from '../node_modules/@earendil-works/pi-tui/dist/keybindings.js';
import { plainJSON, strict } from './errors.mjs';
import { hostKeybindings } from './theme.mjs';

export async function installHostKeybindings(runtime) {
  const snapshot = () => {
    const store = runtime.scope.getStore();
    runtime.assertSessionOwner(store);
    return store.state.host.keybindings;
  };
  const keys = hostKeybindings(snapshot);
  const manager = strict({
    matches: (data, action) => keys.matches(data, action),
    getKeys: action => keys.getKeys(action),
    getResolvedBindings: () => plainJSON(snapshot(), 'host keybindings'),
    getEffectiveConfig: () => plainJSON(snapshot(), 'host keybindings'),
  }, 'host keybindings');
  setKeybindings(manager);
  // jiti's admitted component graph and native ESM must see the same dispatcher.
  const module = await runtime.jiti.import(fileURLToPath(new URL('../node_modules/@earendil-works/pi-tui/dist/keybindings.js', import.meta.url)));
  module.setKeybindings(manager);
}
