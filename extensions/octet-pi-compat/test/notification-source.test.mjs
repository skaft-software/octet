import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createContext, extensionDisplayName } from '../lib/api.mjs';

test('the Pi UI adapter sends the originating extension label with negotiated notifications', async () => {
  const frames = [];
  const runtime = {
    features: new Set(['notification_source_v1']),
    extensionNames: new Map([[7, 'Termdraw']]),
    assertOwner() {}, track: promise => promise,
    transport: { notify: async (method, params) => frames.push({ method, params }) },
  };
  const store = { factory: 7, controller: new AbortController(), state: {} };
  await createContext(runtime, store).ui.notify('Inserted drawing into editor.');
  assert.deepEqual(frames, [{ method: 'notification', params: {
    level: 'info', message: 'Inserted drawing into editor.', source: 'Termdraw',
  } }]);
});

test('extension notifications use a declared display name or humanize the package name', () => {
  const root = mkdtempSync(join(tmpdir(), 'pi-notification-name-'));
  try {
    const pkg = join(root, 'node_modules', '@termdraw', 'pi');
    mkdirSync(pkg, { recursive: true });
    writeFileSync(join(pkg, 'package.json'), JSON.stringify({ name: '@termdraw/pi' }));
    const entry = join(pkg, 'index.js');
    writeFileSync(entry, '');
    assert.equal(extensionDisplayName(entry), 'Termdraw');
    writeFileSync(join(pkg, 'package.json'), JSON.stringify({ name: '@termdraw/pi', pi: { displayName: 'Termdraw Pro' } }));
    assert.equal(extensionDisplayName(entry), 'Termdraw Pro');
  } finally { rmSync(root, { recursive: true, force: true }); }
});
