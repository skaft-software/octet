import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
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

for (const entry of ['runner.mjs', 'configure.mjs']) test(`${entry} enables the compile cache before loading the adapter graph`, () => {
  const source = readFileSync(new URL(`../${entry}`, import.meta.url), 'utf8');
  const enabled = source.indexOf('enableModuleCompileCache();');
  assert.ok(enabled > source.indexOf("import './lib/runtime-prerequisites.mjs'"));
  const imports = [...source.matchAll(/await import\(['"]\.\/lib\/([^'"]+)['"]\)/g)];
  assert.ok(imports.length > 1);
  for (const imported of imports) if (imported[1] !== 'startup-cache.mjs') assert.ok(enabled < imported.index, `${imported[1]} loaded before cache activation`);
});

for (const mode of ['private', 'override', 'unusable']) test(`module compile cache activates before subsequent imports: ${mode}`, () => {
  const directory = join(process.env.OCTET_PI_COMPAT_CACHE, mode);
  const probe = join(process.env.OCTET_PI_COMPAT_CACHE, `probe-${mode}.mjs`);
  writeFileSync(probe, 'import module from "node:module"; export default module.getCompileCacheDir?.();');
  const cacheModule = new URL('../lib/startup-cache.mjs', import.meta.url).href;
  const env = { ...process.env, OCTET_PI_COMPAT_CACHE: directory };
  delete env.NODE_COMPILE_CACHE;
  delete env.NODE_DISABLE_COMPILE_CACHE;
  if (mode === 'override') env.NODE_COMPILE_CACHE = join(directory, 'explicit');
  if (mode === 'unusable') writeFileSync(directory, 'not a directory');
  const result = spawnSync(process.execPath, ['--input-type=module', '--eval', `
    import { enableModuleCompileCache } from ${JSON.stringify(cacheModule)};
    enableModuleCompileCache();
    const { default: directory } = await import(${JSON.stringify(pathToFileURL(probe).href)});
    console.log(JSON.stringify({ directory }));
  `], { env, encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  const observed = JSON.parse(result.stdout).directory;
  if (mode === 'unusable') assert.equal(observed, undefined);
  else {
    const expected = mode === 'override' ? env.NODE_COMPILE_CACHE : join(directory, 'node-compile-cache');
    assert.equal(observed, fileURLToPath(pathToFileURL(observed)));
    assert.ok(observed.startsWith(`${expected}/`), observed);
  }
});

test.after(() => rmSync(process.env.OCTET_PI_COMPAT_CACHE, { recursive: true, force: true }));
