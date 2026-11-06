import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assertRuntimePrerequisites } from '../lib/runtime-prerequisites.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const entrypoints = ['runner.mjs', 'configure.mjs', 'setup.mjs', 'auto-configure.mjs'];
function temporary(t) {
  const dir = mkdtempSync(join(tmpdir(), 'octet-pi-prerequisite-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}
// This exercises startup ordering on the current Node, not qualification of a
// different Node binary. Only Node's own trusted version fact is substituted.
function versionPreload(node) {
  return `data:text/javascript,${encodeURIComponent(`Object.defineProperty(process.versions, 'node', { value: ${JSON.stringify(node)} });`)}`;
}

test('Node minimum matches the package engine contract', () => {
  assert.equal(JSON.parse(readFileSync(join(root, 'package.json'))).engines.node, '>=22.19.0');
  for (const node of ['18.20.8', '20.19.5', '21.7.3', '22.0.0', '22.18.0', '22.18.9']) {
    assert.throws(() => assertRuntimePrerequisites({ node }), error => {
      assert.match(error.message, /Node 22\.19\.0 or newer is required/);
      assert.ok(error.message.includes(`found Node ${node}`));
      assert.match(error.message, /Upgrade Node.*PATH/);
      return true;
    });
  }
  for (const node of ['22.19.0', '22.19.1', '22.20.0', '23.0.0', '24.0.0']) {
    assert.doesNotThrow(() => assertRuntimePrerequisites({ node }));
  }
});

test('Bun is intentionally not judged by its reported Node compatibility version', () => {
  assert.doesNotThrow(() => assertRuntimePrerequisites({ bun: '1.2.0', node: '22.18.0' }));
  assert.doesNotThrow(() => assertRuntimePrerequisites({ bun: '1.2.0', node: '20.0.0' }));
});

for (const entry of entrypoints) test(`${entry} rejects old Node before linking adapter dependencies or doing setup`, t => {
  const dir = temporary(t);
  mkdirSync(join(dir, 'lib'));
  for (const name of entrypoints) copyFileSync(join(root, name), join(dir, name));
  copyFileSync(join(root, 'lib/runtime-prerequisites.mjs'), join(dir, 'lib/runtime-prerequisites.mjs'));
  // No adapter dependencies exist here. An eager static dependency graph would
  // fail module resolution before the actionable prerequisite diagnostic.
  for (const node of ['20.19.5', '22.18.0']) {
    const result = spawnSync(process.execPath, ['--import', versionPreload(node), join(dir, entry)],
      { encoding: 'utf8', timeout: 5000, env: { ...process.env, PATH: dir } });
    assert.ifError(result.error);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /Node 22\.19\.0 or newer is required/);
    assert.ok(result.stderr.includes(`found Node ${node}`));
    assert.doesNotMatch(result.stderr, /ERR_MODULE_NOT_FOUND|ENOENT/);
    assert.equal(result.stdout, '');
  }
});

test('runner refuses 22.18 before a factory import, but 22.19 imports it', t => {
  const dir = temporary(t), marker = join(dir, 'factory-imported'), factory = join(dir, 'factory.mjs');
  writeFileSync(factory, `import {writeFileSync} from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, 'yes'); export default () => {};`);
  const run = node => spawnSync(process.execPath,
    ['--import', versionPreload(node), join(root, 'runner.mjs'), '--inspect', factory],
    { encoding: 'utf8', timeout: 10000, env: { ...process.env, OCTET_PI_COMPAT_CACHE: join(dir, 'cache') } });
  const refused = run('22.18.0');
  assert.ifError(refused.error);
  assert.equal(refused.status, 1);
  assert.match(refused.stderr, /Node 22\.19\.0 or newer is required/);
  assert.equal(refused.stdout, '');
  assert.equal(existsSync(marker), false);
  const accepted = run('22.19.0');
  assert.ifError(accepted.error);
  assert.equal(accepted.status, 0, accepted.stderr);
  assert.equal(readFileSync(marker, 'utf8'), 'yes');
  assert.ok(JSON.parse(accepted.stdout).result);
});
