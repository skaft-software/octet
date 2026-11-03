import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createJiti } from 'jiti';
import { NativeEsm, nativeEsmAvailable, nativeIneligibleReason } from '../lib/native-esm.mjs';
import { emulatedPiAliases, typeboxAliases, UNAVAILABLE_MODULE } from '../lib/pi-modules.mjs';
import { reviewedSourceHashes } from '../lib/runtime.mjs';

const aliases = { ...typeboxAliases(), ...emulatedPiAliases() };
const CODING = '@earendil-works/pi-coding-agent';

function project(files, { type = 'module' } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'octet-native-'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify(type ? { type } : {}));
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  return dir;
}
const setup = () => ({ native: new NativeEsm(aliases, UNAVAILABLE_MODULE), jiti: createJiti(import.meta.url, { alias: aliases, moduleCache: true, tryNative: false, fsCache: false }) });
const counter = name => { globalThis[name] = 0; return () => globalThis[name]; };

test('native hook availability honors the kill switch', () => {
  assert.equal(nativeEsmAvailable(), true);
  process.env.OCTET_PI_NATIVE_ESM = '0';
  try { assert.equal(nativeEsmAvailable(), false); } finally { delete process.env.OCTET_PI_NATIVE_ESM; }
});

test('a shim module is one instance across native and jiti importers; aliases resolve natively', async t => {
  const dir = project({
    'native.js': `import * as pi from '${CODING}'; import * as tb from 'typebox'; import { shared } from './helper.js';
export default function factory() {} export { pi, tb, shared };`,
    'helper.js': 'export const shared = { n: 0 };',
    'ts-factory.ts': `import * as pi from '${CODING}'; import { shared } from './helper.js';
const marker: number = 1; export default function factory() { return marker; } export { pi, shared };`,
  });
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const { native, jiti } = setup();
  const entry = join(dir, 'native.js');
  assert.equal(await native.attempt(entry, reviewedSourceHashes(entry)), true);
  const shim = await import(pathToFileURL(aliases[CODING]).href);
  const viaNative = await import(pathToFileURL(entry).href);
  assert.equal(viaNative.pi, shim, 'native importer sees the very namespace object of the shim');
  const typeboxUrl = pathToFileURL(aliases.typebox).href;
  assert.equal(viaNative.tb, await import(typeboxUrl));
  // jiti-transformed TypeScript importing the same shim and the same helper file.
  const viaJiti = await jiti.import(join(dir, 'ts-factory.ts'));
  assert.equal(viaJiti.pi.CustomEditor, shim.CustomEditor);
  assert.equal(viaJiti.pi.VERSION, shim.VERSION);
  assert.equal(viaJiti.shared, viaNative.shared, 'a relative helper is also a single instance');
  // jiti still returns the factory from the already-evaluated native module.
  const factory = await jiti.import(entry, { default: true });
  assert.equal(typeof factory, 'function');
});

test('CJS package, unreviewed relative file and unemulated Pi module fall back before any evaluation', async t => {
  const runs = counter('__nativeRuns');
  const cjs = project({ 'index.js': `import * as pi from '${CODING}'; globalThis.__nativeRuns++; export default () => pi;` }, { type: null });
  const escape = project({ 'index.js': `import * as pi from '${CODING}'; import './later.js'; globalThis.__nativeRuns++; export default () => pi;`, 'later.js': 'export {};' });
  const unavailable = project({ 'index.js': `import '@earendil-works/pi-ai/oauth'; globalThis.__nativeRuns++; export default 1;` });
  const missing = project({ 'index.js': `import { noSuchExport } from '${CODING}'; globalThis.__nativeRuns++; export default noSuchExport;` });
  const subpath = project({ 'index.js': `import '${CODING}/internal'; globalThis.__nativeRuns++; export default 1;` });
  for (const dir of [cjs, escape, unavailable, missing, subpath]) t.after(() => rmSync(dir, { recursive: true, force: true }));
  const { native } = setup();
  assert.match(nativeIneligibleReason(join(cjs, 'index.js'), reviewedSourceHashes(join(cjs, 'index.js'))), /type:module/);
  // The escape closure omits ./later.js, as if the review's static scan had missed it.
  const entry = join(escape, 'index.js'), hashes = reviewedSourceHashes(entry);
  delete hashes[join(escape, 'later.js')];
  assert.equal(await native.attempt(entry, hashes), false);
  for (const dir of [unavailable, missing, subpath]) {
    const file = join(dir, 'index.js');
    assert.equal(await native.attempt(file, reviewedSourceHashes(file)), false, dir);
  }
  assert.equal(await native.attempt(join(cjs, 'index.js'), reviewedSourceHashes(join(cjs, 'index.js'))), false);
  assert.equal(runs(), 0, 'no declined factory evaluated');
});

test('a closure file whose bytes differ from the reviewed hash is refused, not run or retried', async t => {
  const runs = counter('__nativeIntegrity');
  const dir = project({ 'index.js': `import * as pi from '${CODING}'; import './dep.js'; export default () => pi;`, 'dep.js': 'globalThis.__nativeIntegrity++; export {};' });
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const entry = join(dir, 'index.js'), hashes = reviewedSourceHashes(entry);
  hashes[join(dir, 'dep.js')] = '0'.repeat(64);
  const { native } = setup();
  await assert.rejects(() => native.attempt(entry, hashes), /reviewed factory source changed/);
  assert.equal(runs(), 0);
});

test('CJS-style wrapper names jiti provides are defined only when used and not declared', async t => {
  const dir = project({
    'index.js': `export default function factory() {}
export const where = typeof __dirname === 'undefined' ? null : __dirname;
export const file = __filename; export const req = typeof require === 'function';
export const own = (() => { const module = 1; return module; })();`,
    'owned.js': 'const require = () => 7; export const r = require();',
  });
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const { native } = setup();
  const entry = join(dir, 'index.js');
  assert.equal(await native.attempt(entry, reviewedSourceHashes(entry)), true);
  const ns = await import(pathToFileURL(entry).href);
  assert.equal(ns.where, dir);
  assert.equal(ns.file, entry);
  assert.equal(ns.req, true);
  assert.equal(ns.own, 1);
  const owned = join(dir, 'owned.js');
  assert.equal(await native.attempt(owned, reviewedSourceHashes(owned)), true);
  assert.equal((await import(pathToFileURL(owned).href)).r, 7);
});

test('a live scope keeps resolving aliases for later dynamic imports from the closure', async t => {
  const dir = project({ 'index.js': `export default () => {}; export const later = () => import('${CODING}');` });
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const { native } = setup();
  const entry = join(dir, 'index.js');
  assert.equal(await native.attempt(entry, reviewedSourceHashes(entry)), true);
  const { later } = await import(pathToFileURL(entry).href);
  assert.equal(await later(), await import(pathToFileURL(aliases[CODING]).href));
});

test('a declined probe does not strip a live scope of the files they share', async t => {
  const dir = project({
    'util.js': `export const later = () => import('${CODING}');`,
    'a.js': "import { later } from './util.js'; export default () => {}; export { later };",
    'b.js': "import './util.js'; import 'cjsdep'; export default () => {};",
  });
  mkdirSync(join(dir, 'node_modules', 'cjsdep'), { recursive: true });
  writeFileSync(join(dir, 'node_modules', 'cjsdep', 'package.json'), '{"name":"cjsdep","main":"index.js"}');
  writeFileSync(join(dir, 'node_modules', 'cjsdep', 'index.js'), 'module.exports = 1;');
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const { native } = setup();
  const a = join(dir, 'a.js'), b = join(dir, 'b.js');
  assert.equal(await native.attempt(a, reviewedSourceHashes(a)), true);
  assert.equal(await native.attempt(b, reviewedSourceHashes(b)), false);
  const { later } = await import(pathToFileURL(a).href);
  assert.equal(await later(), await import(pathToFileURL(aliases[CODING]).href));
});
