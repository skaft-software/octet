import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, truncateSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { configureMirror, preparePiMirror } from '../configure.mjs';
import { REVIEWED_SOURCE_LIMITS, reviewedSourceHashes } from '../lib/runtime.mjs';

function temporary(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'octet-source-review-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}
function put(path, content) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
  return path;
}
function sourceChain(root, count) {
  const files = [];
  for (let index = 0; index < count; index++) {
    const path = join(root, `part-${String(index).padStart(3, '0')}.mjs`);
    files.push(put(path, index + 1 < count ? `import './part-${String(index + 1).padStart(3, '0')}.mjs';\n` : 'export {};\n'));
  }
  return files[0];
}

test('reviewed source hashes read bounded bundled sources above the former 1 MiB limit and track changes', t => {
  const root = temporary(t), entry = put(join(root, 'entry.mjs'), `import './bundle.mjs';\n`);
  const bundle = Buffer.alloc(1024 * 1024 + 128, 0x20);
  bundle.write('export const bundled = true;\n');
  const path = put(join(root, 'bundle.mjs'), bundle);
  const before = reviewedSourceHashes(entry);
  assert.equal(before[realpathSync(path)], createHash('sha256').update(bundle).digest('hex'));
  assert.ok(bundle.byteLength > 1024 * 1024);
  put(path, Buffer.concat([bundle, Buffer.from('\n// changed\n')]));
  assert.notEqual(reviewedSourceHashes(entry)[realpathSync(path)], before[realpathSync(path)]);
});

test('reviewed source closure accepts 256 files and explicitly refuses the 257th', t => {
  assert.equal(REVIEWED_SOURCE_LIMITS.files, 256);
  const atLimit = sourceChain(temporary(t), REVIEWED_SOURCE_LIMITS.files);
  assert.equal(Object.keys(reviewedSourceHashes(atLimit)).length, 256);
  const overLimit = sourceChain(temporary(t), REVIEWED_SOURCE_LIMITS.files + 1);
  assert.throws(() => reviewedSourceHashes(overLimit), /reviewed factory source closure exceeds 256 files/);
});

test('reviewed source file and closure-byte limits reject before unbounded reads', t => {
  assert.equal(REVIEWED_SOURCE_LIMITS.fileBytes, 8 * 1024 * 1024);
  assert.equal(REVIEWED_SOURCE_LIMITS.bytes, 8 * 1024 * 1024);

  const exactRoot = temporary(t), exact = put(join(exactRoot, 'exact.mjs'), Buffer.alloc(REVIEWED_SOURCE_LIMITS.fileBytes, 0x20));
  assert.ok(reviewedSourceHashes(exact)[realpathSync(exact)]);

  const oversizedRoot = temporary(t), oversized = put(join(oversizedRoot, 'oversized.mjs'), '');
  truncateSync(oversized, REVIEWED_SOURCE_LIMITS.fileBytes + 1);
  assert.throws(() => reviewedSourceHashes(oversized), /reviewed factory source file exceeds 8388608 bytes/);

  const exactClosureRoot = temporary(t), exactEntrySource = `import './part.mjs';\n`;
  const exactEntry = put(join(exactClosureRoot, 'entry.mjs'), exactEntrySource);
  put(join(exactClosureRoot, 'part.mjs'), Buffer.alloc(REVIEWED_SOURCE_LIMITS.bytes - Buffer.byteLength(exactEntrySource), 0x20));
  assert.equal(Object.keys(reviewedSourceHashes(exactEntry)).length, 2);

  const closureRoot = temporary(t), entry = put(join(closureRoot, 'entry.mjs'), `import './first.mjs';\n`);
  const megabytes = 4 * 1024 * 1024;
  const first = Buffer.alloc(megabytes, 0x20); first.write(`import './second.mjs';\n`);
  put(join(closureRoot, 'first.mjs'), first);
  put(join(closureRoot, 'second.mjs'), Buffer.alloc(megabytes, 0x20));
  assert.throws(() => reviewedSourceHashes(entry), /reviewed factory source closure exceeds 8388608 bytes/);
});

test('reviewed source closure refuses symlinked entrypoints and relative-import candidates', t => {
  const root = temporary(t), outside = put(join(root, 'outside.mjs'), 'export {};\n');
  const targetDir = join(root, 'target'); put(join(targetDir, 'inside.mjs'), 'export {};\n');
  try {
    symlinkSync(outside, join(root, 'linked.mjs'), process.platform === 'win32' ? 'file' : undefined);
    symlinkSync(targetDir, join(root, 'linked-dir'), process.platform === 'win32' ? 'junction' : undefined);
  } catch (error) {
    if (process.platform === 'win32' && ['EPERM', 'EACCES', 'ENOSYS'].includes(error.code)) return t.skip(`Windows symlink creation unavailable: ${error.code}`);
    throw error;
  }
  const linkedEntry = join(root, 'linked.mjs');
  assert.throws(() => reviewedSourceHashes(linkedEntry), /reviewed factory source traverses a symlink/);

  const leafImport = put(join(root, 'leaf-import.mjs'), `import './linked.mjs';\n`);
  assert.throws(() => reviewedSourceHashes(leafImport), /reviewed factory source traverses a symlink/);
  const directoryImport = put(join(root, 'directory-import.mjs'), `import './linked-dir/inside.mjs';\n`);
  assert.throws(() => reviewedSourceHashes(directoryImport), /reviewed factory source traverses a symlink/);
});

test('Pi mirror preparation/configuration reviews a >1 MiB bundle and >64-file closure without bypassing source hashes', async t => {
  const cwd = temporary(t), agent = join(cwd, 'agent'), output = join(cwd, 'extensions/octet-pi-compat');
  const entry = put(join(agent, 'extensions/entry.mjs'), `import './bundle.mjs';\nimport './chain/part-000.mjs';\nexport default () => {};\n`);
  put(join(agent, 'settings.json'), JSON.stringify({ extensions: [entry] }));
  const bundle = Buffer.alloc(1024 * 1024 + 128, 0x20); bundle.write('export const bundled = true;\n');
  put(join(agent, 'extensions/bundle.mjs'), bundle);
  sourceChain(join(agent, 'extensions/chain'), 64);
  const env = { ...process.env, OCTET_PI_AGENT_DIR: agent };
  const prepared = preparePiMirror({ output, cwd, env });
  assert.equal(prepared.extensions.length, 1);
  assert.equal(prepared.extensions[0].reviewable, true);
  assert.equal(prepared.extensions[0].sources.length, 66);
  const configured = await configureMirror({ output, cwd, env, reviewed: true,
    expectedFingerprint: prepared.fingerprint, log() {} });
  assert.equal(Object.keys(configured.config.entrypoint_sources[entry]).length, 66);
  assert.ok(configured.config.entrypoint_sources[entry][realpathSync(join(agent, 'extensions/bundle.mjs'))]);
});
