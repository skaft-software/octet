import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const moduleUrl = new URL('../lib/jiti-resolve-cache.mjs', import.meta.url).href;
const aliasesUrl = new URL('../lib/pi-modules.mjs', import.meta.url).href;

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), 'octet-resolve-key-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const env = { ...process.env, OCTET_PI_COMPAT_CACHE: join(dir, 'cache') };
  delete env.OCTET_PI_JITI_RESOLVE_CACHE;
  const run = script => {
    const result = spawnSync(process.execPath, ['--input-type=module', '--eval', script], { env, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(result.stdout);
  };
  return { dir, run, cacheFile: join(env.OCTET_PI_COMPAT_CACHE, 'jiti-resolve-v2.json') };
}

test('resolution keys retain the complete ordered alias identity without repeating the table', t => {
  const { run } = fixture(t);
  const result = run(`
    import { resolveCache } from ${JSON.stringify(moduleUrl)};
    import { emulatedPiAliases, typeboxAliases } from ${JSON.stringify(aliasesUrl)};
    const alias = { ...typeboxAliases(), ...emulatedPiAliases() };
    const cache = resolveCache(), ctx = { alias, url: import.meta.url };
    const keys = Array.from({length: 100}, (_, i) => cache.key(ctx, './helper-' + i));
    console.log(JSON.stringify({ keys, table: JSON.stringify(alias),
      equivalent: cache.key({ ...ctx, alias: { ...alias } }, './helper-0'),
      changedTarget: cache.key({ ...ctx, alias: { ...alias, typebox: '/different/typebox.mjs' } }, './helper-0'),
      changedOrder: cache.key({ ...ctx, alias: Object.fromEntries(Object.entries(alias).reverse()) }, './helper-0'),
    }));
  `);
  assert.ok(result.table.length > 1024, 'use the real adapter alias table');
  for (const key of result.keys) assert.match(JSON.parse(key)[0], /^[a-f0-9]{64}$/);
  assert.ok(result.keys.reduce((sum, key) => sum + Buffer.byteLength(key), 0) < 30000,
    '100 import keys must not duplicate the multi-KiB table');
  assert.equal(result.equivalent, result.keys[0]);
  assert.notEqual(result.changedTarget, result.keys[0]);
  assert.notEqual(result.changedOrder, result.keys[0]);
});

test('cold and warm resolution keep different alias targets separate and corrupt caches fall back', t => {
  const { dir, run, cacheFile } = fixture(t);
  const entry = join(dir, 'factory.ts');
  const targets = ['shim', 'installed'].map(name => {
    const target = join(dir, `${name}.cjs`);
    writeFileSync(target, `module.exports = ${JSON.stringify(name)};`);
    return target;
  });
  writeFileSync(entry, "import helper from 'virtual-helper'; export default helper;");
  const script = `
    import { createJiti, flushResolveCache } from ${JSON.stringify(moduleUrl)};
    const values = [];
    for (const target of ${JSON.stringify(targets)}) {
      const jiti = createJiti(import.meta.url, { alias: { 'virtual-helper': target }, moduleCache: false, tryNative: false });
      values.push(await jiti.import(${JSON.stringify(entry)}, { default: true }));
    }
    flushResolveCache(); console.log(JSON.stringify(values));
  `;
  assert.deepEqual(run(script), ['shim', 'installed']);
  const cold = readFileSync(cacheFile, 'utf8');
  assert.equal(JSON.parse(cold).schema, 'octet.pi-jiti-resolve/v2');
  assert.deepEqual(run(script), ['shim', 'installed']);
  assert.equal(readFileSync(cacheFile, 'utf8'), cold, 'warm hits need no rewritten cache');
  writeFileSync(cacheFile, '{broken');
  assert.deepEqual(run(script), ['shim', 'installed']);
});
