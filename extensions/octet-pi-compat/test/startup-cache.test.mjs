import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// The cache module reads its directory from the environment on each call, so a
// test can point it at a scratch directory without touching the user cache.
process.env.OCTET_PI_COMPAT_CACHE = mkdtempSync(join(tmpdir(), 'octet-pi-cache-'));
const { startupCacheDirectory, jitiCacheOptions } = await import('../lib/startup-cache.mjs');

test('the startup cache directory is owner-only and reused', () => {
  const directory = startupCacheDirectory();
  assert.equal(directory, process.env.OCTET_PI_COMPAT_CACHE);
  const mode = statSync(directory).mode & 0o777;
  assert.equal(mode, 0o700, `cache directory mode ${mode.toString(8)}`);
  assert.equal(startupCacheDirectory(), directory);
});

test('jiti keeps its content-hashed cache inside the private directory', () => {
  assert.deepEqual(jitiCacheOptions(), { fsCache: join(process.env.OCTET_PI_COMPAT_CACHE, 'jiti') });
});

test('an unwritable cache directory degrades to jiti defaults', () => {
  const previous = process.env.OCTET_PI_COMPAT_CACHE;
  try {
    // A path under a file can never be created, so the options fall back to
    // jiti's own defaults instead of failing startup.
    const file = join(process.env.OCTET_PI_COMPAT_CACHE, 'not-a-directory');
    rmSync(file, { recursive: true, force: true });
    writeFileSync(file, 'x');
    process.env.OCTET_PI_COMPAT_CACHE = join(file, 'nested');
    assert.equal(startupCacheDirectory(), undefined);
    assert.deepEqual(jitiCacheOptions(), {});
  } finally {
    process.env.OCTET_PI_COMPAT_CACHE = previous;
  }
});

test.after(() => rmSync(process.env.OCTET_PI_COMPAT_CACHE, { recursive: true, force: true }));
