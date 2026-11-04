import test from 'node:test';
import assert from 'node:assert/strict';
import * as theme from '../lib/theme.mjs';
import { installHostKeybindings } from '../lib/keybindings.mjs';
import { getKeybindings } from '../node_modules/@earendil-works/pi-tui/dist/keybindings.js';

// pi-mcp-adapter 7.0.0 (panel-keys.ts) probes the optional `getUserBindings` member of the keybinding
// manager it is handed while constructing the /mcp panel. Reading it must report "absent" (the adapter's own
// structural type declares it optional) rather than throw: the throw rejected an unawaited ui.custom() promise,
// the runner's unhandledRejection handler failed the transport, and the whole host exited on /mcp.
const configuredSaveKeys = keybindings => keybindings?.getUserBindings?.()['mcp.panel.save'];

test('custom UI keybindings read the optional getUserBindings probe as absent', () => {
  const keys = theme.hostKeybindings(() => ({ 'tui.select.up': ['up'] }));
  assert.equal(keys.getUserBindings, undefined);
  assert.equal(configuredSaveKeys(keys), undefined);
  assert.equal(keys.matches('\x1b[A', 'tui.select.up'), true);
});

test('other unsupported keybinding members still refuse loudly and defaults are still not invented', () => {
  const keys = theme.hostKeybindings(() => ({ 'tui.select.up': ['up'] }));
  assert.throws(() => keys.getDefinition, /unsupported_feature keybindings\.getDefinition/);
  assert.throws(() => keys.getConflicts, /unsupported_feature keybindings\.getConflicts/);
  assert.throws(() => keys.setUserBindings, /unsupported_feature keybindings\.setUserBindings/);
  assert.throws(() => keys.matches('x', 'unknown.action'), /host binding not supplied/);
});

test('the pi-tui manager installed for getKeybindings() reads getUserBindings as absent', async () => {
  const snapshot = { 'tui.select.up': ['up'] };
  const runtime = {
    scope: { getStore: () => ({ state: { host: { keybindings: snapshot } } }) },
    assertSessionOwner() {},
    jiti: { import: specifier => import(specifier) },
  };
  await installHostKeybindings(runtime);
  const manager = getKeybindings();
  assert.equal(manager.getUserBindings, undefined);
  assert.equal(configuredSaveKeys(manager), undefined);
  assert.equal(manager.matches('\x1b[A', 'tui.select.up'), true);
  assert.throws(() => manager.getDefinition, /unsupported_feature host keybindings\.getDefinition/);
});
