import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

// pnpm-style layout: the adapter's node_modules is a symlink into a store.
// Alias targets must be real paths, or Node's own (realpath) resolution of the
// same module yields a second instance.
test('typebox aliases resolve through symlinked node_modules to real paths', async () => {
  const root = mkdtempSync(join(tmpdir(), 'octet-pi-symlink-'));
  try {
    const store = join(root, 'store');
    mkdirSync(store);
    cpSync(new URL('../node_modules/typebox/', import.meta.url), join(store, 'typebox'), { recursive: true });
    const adapter = join(root, 'adapter');
    mkdirSync(adapter);
    cpSync(new URL('../lib/', import.meta.url), join(adapter, 'lib'), { recursive: true });
    cpSync(new URL('../shims/', import.meta.url), join(adapter, 'shims'), { recursive: true });
    symlinkSync(store, join(adapter, 'node_modules'));
    const { typeboxAliases, emulatedPiAliases } = await import(pathToFileURL(join(adapter, 'lib', 'pi-modules.mjs')).href);
    const aliases = typeboxAliases();
    assert.ok(Object.keys(aliases).length > 0);
    for (const target of [...Object.values(aliases), ...Object.values(emulatedPiAliases())]) {
      assert.equal(target, realpathSync(target), `${target} is not a real path`);
    }
    assert.ok(aliases.typebox.startsWith(realpathSync(store)), aliases.typebox);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
