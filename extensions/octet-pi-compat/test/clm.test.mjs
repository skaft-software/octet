import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { root } from './helper.mjs';

// Explicitly supplied, unchanged b84a9d7c upstream source only. A passing
// refusal regression is NOT any of CLM's seven native qualification gates.
const entry = process.env.PI_CLM_PATH;
test('unchanged pinned pi-clm 1.0.0 still refuses unbound per-model turn events after native context/pipeline/session declarations', {
  skip: !entry && 'set PI_CLM_PATH to reviewed pi-clm b84a9d7c root index.ts',
}, t => {
  const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');
  assert.equal(hash(entry), 'd5e9d73034eb87e28dc9909a2d8ac28b85a1969fd234c7a8f3eddecb71e3fde6');
  assert.equal(hash(join(dirname(entry), 'src/index.ts')), 'a6d1c464bebdcc927a0eb6d0bb1ff319c3e5cdf5bf844e03b599ec91169f7170');
  assert.equal(JSON.parse(readFileSync(join(dirname(entry), 'package.json'), 'utf8')).version, '1.0.0');
  const home = mkdtempSync(join(tmpdir(), 'octet-clm-refusal-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const result = spawnSync(process.execPath, [join(root, 'runner.mjs'), '--inspect', entry], {
    encoding: 'utf8', timeout: 10000, maxBuffer: 1048576, cwd: home,
    env: { HOME: home, USERPROFILE: home, TMPDIR: home, PI_OFFLINE: '1' },
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /unsupported_feature event turn_start/);
});
