// `/usage` of @narumitw/pi-usage 0.69.0 opens its menu through @narumitw/pi-tui-kit, whose
// `componentKeyAvailable()` reads `tui.mode`, `tui.onDebug` and the terminal key-protocol flags of
// the TUI handed to a `ctx.ui.custom()` factory. Pi 1.4.0 has all of them; octet's remote TUI refused
// the first read, so the command failed with `unsupported_feature tui.mode` and never opened.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

const plain = frame => frame.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '');

function factory(t, source) {
  const directory = mkdtempSync(join(tmpdir(), 'pi-tui-members-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.mjs');
  writeFileSync(entry, source);
  return entry;
}

// The same reads pi-tui-kit makes while it builds the hint line of its "actions" menu, then the rows Pi
// shows for `/usage` on a provider without usage reporting (S08 checkpoint 09 on Pi 1.4.0).
const usageProbe = `export default pi => pi.registerCommand('usage', { async handler(_args, ctx) {
  const probe = {};
  const result = await ctx.ui.custom((tui, _theme, _keybindings, done) => {
    probe.mode = tui.mode;
    probe.onDebug = tui.onDebug;
    probe.kitty = tui.terminal.kittyProtocolActive;
    probe.modifyOtherKeys = tui.terminal.modifyOtherKeysActive;
    probe.rows = tui.terminal.rows;
    probe.mouseWrites = [];
    const reserved = tui.mode === 'fullscreen' ? ['up', 'down'] : [];
    if (tui.onDebug) reserved.push('shift+ctrl+d');
    const hint = ['up', 'down'].filter(key => !reserved.includes(key)).length === 2 ? '↑/↓ navigate' : 'navigate';
    return {
      render: () => ['Provider usage', 'mock · Current', 'Unsupported: Usage reporting is not supported for mock.', '',
        '→ Refresh current usage', '  Settings', '  View another configured provider…', '  View all configured providers…', '  Close',
        hint + ' • enter select • esc close'],
      invalidate() {},
      handleInput(data) { if (data === '\\r') done('selected'); },
    };
  });
  ctx.ui.notify('result:' + result + ' probe:' + JSON.stringify(probe));
} });
`;

test('a custom TUI reads Pi 1.4.0 mode, onDebug and terminal key-protocol flags without an unsupported_feature refusal', async t => {
  const entry = factory(t, usageProbe);
  const peer = launch(t, [entry], { columns: 100, rows: 30 });
  await peer.init(['remote_ui']);
  await peer.start({ mode: 'tui', has_ui: true });
  t.after(() => { if (!t.passed) t.diagnostic(JSON.stringify({ seen: peer.seen, stderr: peer.stderr() })); });

  const command = peer.command('usage', [], { mode: 'tui', has_ui: true });
  const open = await peer.wait(frame => frame.method === 'ui/open');
  const frame = await peer.wait(candidate => candidate.method === 'ui/frame' && candidate.params.surface_id === open.params.surface_id);
  assert.deepEqual(frame.params.lines.map(line => line.replace(/\x1b\[[0-9;]*m/g, '')), [
    'Provider usage', 'mock · Current', 'Unsupported: Usage reporting is not supported for mock.', '',
    '→ Refresh current usage', '  Settings', '  View another configured provider…', '  View all configured providers…', '  Close',
    '↑/↓ navigate • enter select • esc close',
  ]);

  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'Enter', kind: 'press', modifiers: [] });
  await peer.wait(candidate => candidate.method === 'ui/close');
  const response = await command.response;
  assert.equal(response.error, undefined, JSON.stringify(response));
  const done = await peer.wait(candidate => candidate.method === 'notification' && /^result:selected probe:/.test(candidate.params.message));
  const probe = JSON.parse(done.params.message.replace(/^result:selected probe:/, ''));
  // Regular mode: octet's Rust frontend owns scrollback and has no Pi alt-screen transcript keys, so no
  // `tui.altScreen.*` action is reserved. Pi's optional `onDebug` is absent; the key-protocol flags are the
  // adapter's own (it never negotiates Kitty or modifyOtherKeys; the host normalizes keys).
  assert.equal(probe.mode, 'regular');
  assert.equal(probe.onDebug, undefined);
  assert.equal(probe.kitty, false);
  assert.equal(probe.modifyOtherKeys, false);
  assert.equal(probe.rows, 30);
  assert.doesNotMatch(peer.stderr(), /unsupported_feature/);
  await peer.close();
});

test('TUI members octet cannot honor still refuse loudly while the Pi 1.4.0 reads above do not', async t => {
  const entry = factory(t, `export default pi => pi.registerCommand('refuse', { async handler(_args, ctx) {
    const outcome = {};
    await ctx.ui.custom((tui, _theme, _keybindings, done) => {
      for (const name of ['getShowHardwareCursor', 'fullRedraws']) {
        try { void tui[name]; outcome[name] = 'read'; } catch (error) { outcome[name] = /unsupported_feature tui\\.\\w+/.test(error.message) ? 'refused' : error.message; }
      }
      try { tui.terminal.start(); outcome.start = 'started'; } catch (error) { outcome.start = /unsupported_feature tui\\.terminal\\.start/.test(error.message) ? 'refused' : error.message; }
      done('closed');
      return { render: () => ['x'], invalidate() {} };
    });
    ctx.ui.notify('outcome:' + JSON.stringify(outcome));
  } });`);
  const peer = launch(t, [entry], { columns: 80, rows: 24 });
  await peer.init(['remote_ui']);
  await peer.start({ mode: 'tui', has_ui: true });
  t.after(() => { if (!t.passed) t.diagnostic(JSON.stringify({ seen: peer.seen, stderr: peer.stderr() })); });
  const command = peer.command('refuse', [], { mode: 'tui', has_ui: true });
  const note = await peer.wait(candidate => candidate.method === 'notification' && /^outcome:/.test(candidate.params.message));
  assert.deepEqual(JSON.parse(note.params.message.slice('outcome:'.length)), { getShowHardwareCursor: 'refused', fullRedraws: 'refused', start: 'refused' });
  assert.equal((await command.response).error, undefined);
  await peer.close();
});

// A real-package acceptance run, kept out of the default suite like the other original-package checks.
// Install `@narumitw/pi-usage@0.69.0` with `npm install --ignore-scripts --legacy-peer-deps` into a scratch
// directory and set PI_USAGE_PATH to its `dist/index.ts`; the package's own pi-tui-kit builds the menu.
const realEntry = process.env.PI_USAGE_PATH;
test('original @narumitw/pi-usage /usage opens the Provider usage menu with Pi rows', { skip: !realEntry && 'set reviewed PI_USAGE_PATH', timeout: 30000 }, async t => {
  const peer = launch(t, [realEntry], { columns: 100, rows: 30 });
  await peer.init(['remote_ui', 'session_entries', 'pipeline_hooks_v1', 'resource_paths_v1', 'lifecycle_events', 'lifecycle_events_v2', 'message_injection', 'shortcuts', 'composer', 'active_tools']);
  await peer.start({ mode: 'tui', has_ui: true });
  t.after(() => { if (!t.passed) t.diagnostic(JSON.stringify({ seen: peer.seen, stderr: peer.stderr() })); });
  const command = peer.command('usage', [], { mode: 'tui', has_ui: true });
  command.response.catch(() => {});
  // The package first mounts a "Checking current usage…" loader surface, then the menu on a second one.
  const frame = await peer.wait(candidate => candidate.method === 'ui/frame' && plain(candidate).includes('Provider usage'), 15000);
  const text = plain(frame);
  for (const row of ['Provider usage', 'Refresh current usage', 'Settings', 'View another configured provider…', 'View all configured providers…', 'Close', '↑/↓ navigate • enter select • esc close']) {
    assert.ok(text.includes(row), `${row} in ${text}`);
  }
  peer.notify('ui/key', { surface_id: frame.params.surface_id, key: 'Escape', kind: 'press', modifiers: [] });
  await peer.wait(candidate => candidate.method === 'ui/close' && candidate.params.surface_id === frame.params.surface_id);
  assert.equal((await command.response).error, undefined);
});
