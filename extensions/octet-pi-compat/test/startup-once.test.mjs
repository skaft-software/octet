import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, utimesSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { reviewedSourceHashes } from '../lib/runtime.mjs';
import { SourceFingerprints } from '../lib/source-fingerprints.mjs';
import { warmupWorkerCount } from '../lib/transform-warmup.mjs';

const past = new Date(Date.now() - 3600_000);
function tree() {
  const dir = mkdtempSync(join(tmpdir(), 'octet-once-'));
  writeFileSync(join(dir, 'a.ts'), "import './b';\nexport default 1;\n");
  writeFileSync(join(dir, 'b.ts'), 'export const b = 1;\n');
  for (const f of ['a.ts', 'b.ts']) utimesSync(join(dir, f), past, past);
  return dir;
}

test('fingerprint cache reuses unchanged digests and re-hashes a same-size rewrite with restored mtime', t => {
  const cache = mkdtempSync(join(tmpdir(), 'octet-fp-'));
  const previous = process.env.OCTET_PI_COMPAT_CACHE;
  process.env.OCTET_PI_COMPAT_CACHE = cache;
  t.after(() => { previous === undefined ? delete process.env.OCTET_PI_COMPAT_CACHE : process.env.OCTET_PI_COMPAT_CACHE = previous; });
  const dir = tree();
  const first = new SourceFingerprints({ racyNs: 0n });
  const plain = reviewedSourceHashes(join(dir, 'a.ts'));
  assert.deepEqual(reviewedSourceHashes(join(dir, 'a.ts'), { fingerprints: first }), plain);
  first.flush();
  const second = new SourceFingerprints();
  assert.equal(second.files.size, 2);
  assert.deepEqual(reviewedSourceHashes(join(dir, 'a.ts'), { fingerprints: second }), plain);
  // Same size, mtime restored: ctime still moves, so the file is re-hashed.
  writeFileSync(join(dir, 'b.ts'), 'export const b = 2;\n');
  utimesSync(join(dir, 'b.ts'), past, past);
  const third = new SourceFingerprints({ racyNs: 0n });
  const changed = reviewedSourceHashes(join(dir, 'a.ts'), { fingerprints: third });
  assert.notDeepEqual(changed, plain);
  assert.deepEqual(changed, reviewedSourceHashes(join(dir, 'a.ts')));
  rmSync(dir, { recursive: true, force: true });
});

test('a corrupt fingerprint cache is ignored', t => {
  const cache = mkdtempSync(join(tmpdir(), 'octet-fp-'));
  writeFileSync(join(cache, 'source-fingerprints-v1.json'), '{not json');
  const previous = process.env.OCTET_PI_COMPAT_CACHE;
  process.env.OCTET_PI_COMPAT_CACHE = cache;
  t.after(() => { previous === undefined ? delete process.env.OCTET_PI_COMPAT_CACHE : process.env.OCTET_PI_COMPAT_CACHE = previous; });
  assert.equal(new SourceFingerprints().files.size, 0);
});

test('warm-up workers leave cores for the host', () => {
  assert.equal(warmupWorkerCount(1000, undefined, 4), 2);
  assert.equal(warmupWorkerCount(1000, undefined, 2), 1);
  assert.equal(warmupWorkerCount(1000, undefined, 1), 1);
  assert.equal(warmupWorkerCount(1000, undefined, 64), 6);
});

test('persisted jiti resolutions are reused and dropped when a shadowing file appears', () => {
  const dir = mkdtempSync(join(tmpdir(), 'octet-res-')), cache = mkdtempSync(join(tmpdir(), 'octet-resc-'));
  mkdirSync(join(dir, 'node_modules', 'dep'), { recursive: true });
  writeFileSync(join(dir, 'node_modules', 'dep', 'package.json'), '{"name":"dep","main":"./i.js"}');
  writeFileSync(join(dir, 'node_modules', 'dep', 'i.js'), 'module.exports = "js";');
  writeFileSync(join(dir, 'x.ts'), "import d from 'dep'; import {y} from './y'; export default d + y;\n");
  writeFileSync(join(dir, 'y.ts'), 'export const y = "-ts";');
  const script = join(dir, 'run.mjs');
  writeFileSync(script, `import { createJiti, flushResolveCache } from ${JSON.stringify(new URL('../lib/jiti-resolve-cache.mjs', import.meta.url).href)};
const jiti = createJiti(import.meta.url, { moduleCache: false, tryNative: false });
console.log(await jiti.import(${JSON.stringify(join(dir, 'x.ts'))}, { default: true })); flushResolveCache();`);
  const run = () => spawnSync(process.execPath, [script], { env: { ...process.env, OCTET_PI_COMPAT_CACHE: cache }, encoding: 'utf8' });
  assert.equal(run().stdout.trim(), 'js-ts');
  assert.ok(JSON.parse(readFileSync(join(cache, 'jiti-resolve-v1.json'), 'utf8')).entries);
  assert.equal(run().stdout.trim(), 'js-ts');
  // A new earlier candidate changes the parent directory: the cached entry must not win.
  writeFileSync(join(dir, 'y.js'), 'export const y = "-js";');
  assert.equal(run().stdout.trim(), 'js-js');
  rmSync(dir, { recursive: true, force: true });
});
