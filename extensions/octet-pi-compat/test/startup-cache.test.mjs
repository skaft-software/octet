import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, chownSync, existsSync, mkdirSync, utimesSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// The cache module reads its directory from the environment on each call, so a
// test can point it at a scratch directory without touching the user cache.
process.env.OCTET_PI_COMPAT_CACHE = mkdtempSync(join(tmpdir(), 'octet-pi-cache-'));
const { startupCacheDirectory, jitiCacheOptions, writePersisted } = await import('../lib/startup-cache.mjs');

test('default caches stay outside the watched extension tree', () => {
  const root = mkdtempSync(join(tmpdir(), 'octet-cache-location-'));
  const extension = join(root, 'extensions', 'octet-pi-compat'), cacheHome = join(root, 'cache');
  const env = { ...process.env, OCTET_EXTENSION_DIR: extension, XDG_CACHE_HOME: cacheHome };
  delete env.OCTET_PI_COMPAT_CACHE;
  try {
    const result = spawnSync(process.execPath, ['--input-type=module', '--eval', `
      import { startupCacheDirectory, writePersisted } from ${JSON.stringify(new URL('../lib/startup-cache.mjs', import.meta.url).href)};
      console.log(startupCacheDirectory());
      writePersisted('probe.json', { schema: 'probe' });
    `], { env, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), join(cacheHome, 'octet-pi-compat'));
    assert.equal(existsSync(extension), false, 'cache writes must not trigger an extension-generation reload');
    assert.equal(existsSync(join(cacheHome, 'octet-pi-compat', 'probe.json')), true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('review capture pins the runtime to the same cache outside its output bundle', () => {
  const root = mkdtempSync(join(tmpdir(), 'octet-capture-cache-'));
  const output = join(root, 'extensions', 'octet-pi-compat'), cacheHome = join(root, 'cache');
  const entry = join(root, 'extension.ts');
  writeFileSync(entry, 'export default pi => pi.on("session_start", () => {});');
  const env = { ...process.env, XDG_CACHE_HOME: cacheHome };
  delete env.OCTET_PI_COMPAT_CACHE;
  delete env.OCTET_EXTENSION_DIR;
  try {
    const result = spawnSync(process.execPath, [fileURLToPath(new URL('../configure.mjs', import.meta.url)),
      '--reviewed', '--output', output, entry], { env, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.ok(readFileSync(join(output, 'extension.toml'), 'utf8').includes(`OCTET_PI_COMPAT_CACHE = ${JSON.stringify(join(cacheHome, 'octet-pi-compat'))}`));
    assert.equal(existsSync(join(output, '.cache')), false);
    assert.ok(readdirSync(join(cacheHome, 'octet-pi-compat', 'jiti')).length);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('the startup cache directory is owner-only and reused', () => {
  const directory = startupCacheDirectory();
  assert.equal(directory, process.env.OCTET_PI_COMPAT_CACHE);
  const mode = statSync(directory).mode & 0o777;
  assert.equal(mode, 0o700, `cache directory mode ${mode.toString(8)}`);
  assert.equal(startupCacheDirectory(), directory);
});

test('temporaries abandoned by killed processes are swept, fresh ones are left alone', () => {
  const directory = startupCacheDirectory();
  const stale = join(directory, 'sweep.json.99999.tmp'), fresh = join(directory, 'sweep.json.99998.tmp'), other = join(directory, 'other.json.99999.tmp');
  for (const file of [stale, fresh, other]) writeFileSync(file, 'x');
  const old = new Date(Date.now() - 3600_000);
  utimesSync(stale, old, old);
  utimesSync(other, old, old);
  try {
    assert.equal(writePersisted('sweep.json', { value: 1 }), true);
    assert.equal(existsSync(stale), false);
    assert.equal(existsSync(fresh), true);
    assert.equal(existsSync(other), true); // another document's temporary is not this write's business
  } finally { for (const file of [stale, fresh, other, join(directory, 'sweep.json')]) rmSync(file, { force: true }); }
});

test('a pre-existing group/world-writable directory is tightened, not trusted', () => {
  const previous = process.env.OCTET_PI_COMPAT_CACHE;
  const loose = mkdtempSync(join(tmpdir(), 'octet-pi-loose-'));
  try {
    chmodSync(loose, 0o777);
    process.env.OCTET_PI_COMPAT_CACHE = loose;
    assert.equal(startupCacheDirectory(), loose);
    assert.equal(statSync(loose).mode & 0o777, 0o700);
  } finally { process.env.OCTET_PI_COMPAT_CACHE = previous; rmSync(loose, { recursive: true, force: true }); }
});

test('a directory owned by another user is never used', { skip: process.getuid?.() !== 0 }, () => {
  const previous = process.env.OCTET_PI_COMPAT_CACHE;
  const foreign = mkdtempSync(join(tmpdir(), 'octet-pi-foreign-'));
  try {
    chownSync(foreign, 65534, 65534);
    process.env.OCTET_PI_COMPAT_CACHE = foreign;
    assert.equal(startupCacheDirectory(), undefined);
    assert.deepEqual(jitiCacheOptions(), {});
  } finally { process.env.OCTET_PI_COMPAT_CACHE = previous; rmSync(foreign, { recursive: true, force: true }); }
});

test('a failed persisted write leaves no temporary file behind', () => {
  const directory = startupCacheDirectory();
  mkdirSync(join(directory, 'blocked.json')); // renaming a file over a directory fails after the write
  try {
    assert.equal(writePersisted('blocked.json', { value: 1 }), false);
    assert.deepEqual(readdirSync(directory).filter(name => name.endsWith('.tmp')), []);
  } finally { rmSync(join(directory, 'blocked.json'), { recursive: true }); }
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
