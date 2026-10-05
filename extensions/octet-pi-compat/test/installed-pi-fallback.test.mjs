// Opt-in installed-Pi fallback (path B). Fixture installs are generated in a
// temporary directory (node_modules/ is gitignored) and selected with
// OCTET_PI_AGENT_DIR; one test exercises the user's real managed Pi 1.0.x.
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createInterface } from 'node:readline';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { root, inspect, host } from './helper.mjs';

const fixture = name => join(root, 'test/fixtures/installed-pi', name);
const runner = join(root, 'runner.mjs');
const piVersion = readFileSync(join(root, 'lib/public-helpers.mjs'), 'utf8').match(/export const VERSION = '([^']+)'/)[1];

function run(extensions, { mode, env = {} } = {}) {
  const result = spawnSync(process.execPath, [runner, ...(mode ? ['--pi-runtime', mode] : []), '--inspect', ...extensions],
    { encoding: 'utf8', timeout: 20000, maxBuffer: 1048576, env: { ...process.env, ...env } });
  const fallback = result.stderr.split('\n').find(line => line.startsWith('[pi-compat fallback] '));
  return { ...result, metadata: result.status === 0 ? JSON.parse(result.stdout).result : undefined,
    fallback: fallback && JSON.parse(fallback.slice('[pi-compat fallback] '.length)) };
}
const describe = (metadata, name) => metadata.commands.find(c => c.name === name)?.description;

// Fake managed install: <agent>/install/current-version + releases/<v>/node_modules/@earendil-works/*.
function fakeInstall(t, { version = '1.0.7', packageVersion = version, omit } = {}) {
  const agent = mkdtempSync(join(tmpdir(), 'octet-installed-pi-'));
  t.after(() => rmSync(agent, { recursive: true, force: true }));
  mkdirSync(join(agent, 'install'), { recursive: true });
  writeFileSync(join(agent, 'install', 'current-version'), `${version}\n`);
  const modules = {
    'pi-coding-agent': `export const parseSkillBlock = text => ({ fixture: text });
export const VERSION = 'real-fixture';
export const truncateToVisualLines = text => text;
export class ProjectTrustStore { constructor() { throw new Error('real ProjectTrustStore must never run'); } }
export const fixtureOnlyHelper = () => 'real unclassified helper ran';`,
    'pi-ai': `export const createProvider = () => { throw new Error('real createProvider must never run'); };`,
    'pi-tui': `export const visibleWidth = () => -1;`,
  };
  for (const [name, source] of Object.entries(modules)) {
    if (name === omit) continue;
    const dir = join(agent, 'install', 'releases', version, 'node_modules', '@earendil-works', name);
    mkdirSync(join(dir, 'dist'), { recursive: true });
    writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: `@earendil-works/${name}`, version: packageVersion, type: 'module',
      exports: { '.': { import: './dist/index.js' } } }));
    writeFileSync(join(dir, 'dist', 'index.js'), source + '\n');
  }
  return { OCTET_PI_AGENT_DIR: agent };
}

test('path A reports a structured fallback-eligible error for a Pi export the shims lack', () => {
  const entry = fixture('missing-export.ts');
  const result = run([entry]);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /^\[pi-compat startup\] pi_compat_fallback_eligible .*octet shims lack or refuse @earendil-works\/pi-coding-agent\.parseSkillBlock/m);
  assert.equal(result.fallback.fallback_eligible, true);
  assert.equal(result.fallback.entrypoint, entry);
  assert.equal(result.fallback.entrypoint_sha256, createHash('sha256').update(readFileSync(entry)).digest('hex'));
  assert.deepEqual(result.fallback.missing_exports, [{ specifier: '@earendil-works/pi-coding-agent', name: 'parseSkillBlock', file: entry }]);
  assert.match(result.fallback.original_error, /parseSkillBlock\) is not a function/);
  assert.equal(result.fallback.supported_installed_pi, '1.0.x');
});

test('path A classifies a gap in a relative helper module, including legacy package names', () => {
  const result = run([fixture('helper-gap.ts')]);
  assert.equal(result.status, 1);
  assert.deepEqual(result.fallback.missing_exports.map(g => [g.specifier, g.name, g.file]),
    [['@mariozechner/pi-coding-agent', 'truncateToVisualLines', fixture('helper-gap-lib.ts')]]);
});

test('path A failures unrelated to Pi exports keep their original error and no fallback data', () => {
  const result = run([fixture('broken.ts')]);
  assert.equal(result.status, 1);
  assert.equal(result.stderr.trim(), `[pi-compat startup] factory module failed on purpose ${piVersion.length}`);
  assert.equal(result.fallback, undefined);
});

test('mode not selected: path A output is identical to explicit shims mode and never touches the Pi install', t => {
  const env = fakeInstall(t, { version: '9.9.9' }); // would fail loudly if path B were consulted
  const core = join(root, 'test/fixtures/core.ts');
  const implicit = run([core], { env }), explicit = run([core], { mode: 'shims', env });
  assert.equal(implicit.status, 0, implicit.stderr);
  assert.equal(implicit.stdout, explicit.stdout);
  assert.equal(implicit.stderr, explicit.stderr);
  assert.deepEqual(implicit.metadata, inspect([core]));
  const config = join(mkdtempSync(join(tmpdir(), 'octet-bridge-')), 'bridge.json');
  t.after(() => rmSync(join(config, '..'), { recursive: true, force: true }));
  writeFileSync(config, JSON.stringify({ extensions: [core] }));
  const fromConfig = spawnSync(process.execPath, [runner, '--config', config, '--inspect'], { encoding: 'utf8', timeout: 20000, env: { ...process.env, ...env } });
  assert.equal(fromConfig.stdout, implicit.stdout);
});

test('installed mode loads the gap from installed Pi while shim exports keep winning', t => {
  const env = fakeInstall(t);
  const result = run([fixture('missing-export.ts')], { mode: 'installed', env });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(describe(result.metadata, 'installed-probe'), `parsed={"fixture":"plain text"} version=${piVersion}`);
  const legacy = run([fixture('helper-gap.ts')], { mode: 'installed', env });
  assert.equal(legacy.status, 0, legacy.stderr);
  assert.equal(describe(legacy.metadata, 'helper'), 'string');
});

test('installed mode via bridge.json pi_runtime field', t => {
  const env = fakeInstall(t);
  const dir = mkdtempSync(join(tmpdir(), 'octet-bridge-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, 'bridge.json'), JSON.stringify({ extensions: [fixture('missing-export.ts')], pi_runtime: 'installed' }));
  const result = spawnSync(process.execPath, [runner, '--config', join(dir, 'bridge.json'), '--inspect'], { encoding: 'utf8', timeout: 20000, env: { ...process.env, ...env } });
  assert.equal(result.status, 0, result.stderr);
  assert.match(JSON.parse(result.stdout).result.commands[0].description, /^parsed=\{"fixture":"plain text"\}/);
});

test('installed mode refuses real Pi names that would take over octet-owned side effects', t => {
  const env = fakeInstall(t);
  const result = run([fixture('denied.ts')], { mode: 'installed', env });
  assert.equal(result.status, 0, result.stderr);
  assert.match(describe(result.metadata, 'trust'), /^unsupported_feature pi-coding-agent\.ProjectTrustStore: Pi config, trust, package and theme state/);
  assert.match(describe(result.metadata, 'provider'), /^unsupported_feature pi-ai\.createProvider: Pi provider runtimes would make model calls/);
  const unclassified = run([fixture('unclassified.ts')], { mode: 'installed', env });
  assert.equal(unclassified.status, 0, unclassified.stderr);
  assert.match(describe(unclassified.metadata, 'unclassified'), /^unsupported_feature pi-coding-agent\.fixtureOnlyHelper: not classified for the installed-Pi fallback/);
});

test('installed mode fails loudly for a missing, unsupported, or inconsistent Pi install', t => {
  const entry = [fixture('missing-export.ts')];
  const cases = [
    [{ OCTET_PI_AGENT_DIR: join(tmpdir(), 'octet-no-such-pi-agent') }, /could not read managed Pi version from .*current-version \(ENOENT\)/],
    [fakeInstall(t, { version: '1.1.0' }), /installed Pi 1\.1\.0 is outside the supported range 1\.0\.x/],
    [fakeInstall(t, { version: '0.99.2' }), /installed Pi 0\.99\.2 is outside the supported range 1\.0\.x/],
    [fakeInstall(t, { version: '1.0.7', packageVersion: '1.0.6' }), /reports @earendil-works\/pi-coding-agent@1\.0\.6, expected 1\.0\.7/],
    [fakeInstall(t, { omit: 'pi-tui' }), /@earendil-works\/pi-tui is missing or unreadable in Pi 1\.0\.7/],
  ];
  for (const [env, message] of cases) {
    const result = run(entry, { mode: 'installed', env });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /^\[pi-compat startup\] installed_pi_unavailable /);
    assert.match(result.stderr, message);
    assert.match(result.stderr, /no silent fallback/);
    assert.equal(result.stdout, '');
  }
  const invalid = run(entry, { mode: 'vendored' });
  assert.equal(invalid.status, 1);
  assert.match(invalid.stderr, /pi_runtime must be "shims" or "installed"/);
});

test('host initialize receives the fallback data on the JSON-RPC error', async t => {
  const entry = fixture('missing-export.ts');
  const child = spawn(process.execPath, [runner, entry], { stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => child.kill());
  const frame = new Promise(resolve => createInterface({ input: child.stdout }).on('line', line => resolve(JSON.parse(line))));
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { api_version: '0.4', workspace: root, host,
    contributes: {}, protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: [], limits: { max_concurrent_requests: 8 } } } }) + '\n');
  const reply = await frame;
  assert.equal(reply.id, 1);
  assert.equal(reply.error.code, -32030);
  assert.match(reply.error.message, /^pi_compat_fallback_eligible /);
  assert.equal(reply.error.data.fallback_eligible, true);
  assert.deepEqual(reply.error.data.missing_exports.map(g => g.name), ['parseSkillBlock']);
});

const realVersionFile = join(homedir(), '.pi', 'agent', 'install', 'current-version');
const realVersion = existsSync(realVersionFile) ? readFileSync(realVersionFile, 'utf8').split('\n', 1)[0] : '';
test('installed mode against the real managed Pi install', { skip: /^1\.0\.\d+$/.test(realVersion) ? false : `no managed Pi 1.0.x at ${realVersionFile}` }, () => {
  const env = { OCTET_PI_AGENT_DIR: '' }; // default resolution: ~/.pi/agent
  const result = run([fixture('missing-export.ts')], { mode: 'installed', env });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(describe(result.metadata, 'installed-probe'), `parsed=null version=${piVersion}`);
  const denied = run([fixture('denied.ts')], { mode: 'installed', env });
  assert.equal(denied.status, 0, denied.stderr);
  assert.match(describe(denied.metadata, 'trust'), /^unsupported_feature pi-coding-agent\.ProjectTrustStore/);
  assert.match(describe(denied.metadata, 'provider'), /^unsupported_feature pi-ai\.createProvider/);
});

test('approved installed mode uses real Pi built-in tool factories that path A refuses', { skip: /^1\.0\.\d+$/.test(realVersion) ? false : `no managed Pi 1.0.x at ${realVersionFile}` }, () => {
  const env = { OCTET_PI_AGENT_DIR: '' };
  const pathA = run([fixture('builtin-tools.ts')], { env });
  assert.notEqual(pathA.status, 0);
  assert.equal(pathA.fallback?.fallback_eligible, true, pathA.stderr);
  const installed = run([fixture('builtin-tools.ts')], { mode: 'installed', env });
  assert.equal(installed.status, 0, installed.stderr);
  assert.equal(describe(installed.metadata, 'builtin-tools'), 'bash=bash:function read=read:function');
});

test('path A offers the fallback for a shimmed built-in tool factory it refuses', () => {
  const pathA = run([fixture('shimmed-builtin.ts')]);
  assert.notEqual(pathA.status, 0);
  assert.equal(pathA.fallback?.fallback_eligible, true, pathA.stderr);
  assert.deepEqual(pathA.fallback.missing_exports.map(g => g.name), ['createBashTool']);
});

test('the pi-ai/compat subpath is fallback-eligible in path A and resolves to installed Pi', { skip: /^1\.0\.\d+$/.test(realVersion) ? false : `no managed Pi 1.0.x at ${realVersionFile}` }, () => {
  const env = { OCTET_PI_AGENT_DIR: '' };
  const pathA = run([fixture('compat-subpath.ts')], { env });
  assert.notEqual(pathA.status, 0);
  assert.equal(pathA.fallback?.fallback_eligible, true, pathA.stderr);
  assert.deepEqual(pathA.fallback.unsupported_subpaths.map(s => s.specifier), ['@earendil-works/pi-ai/compat']);
  const installed = run([fixture('compat-subpath.ts')], { mode: 'installed', env });
  assert.equal(installed.status, 0, installed.stderr);
  assert.equal(describe(installed.metadata, 'compat-subpath'), 'stream=function');
});
