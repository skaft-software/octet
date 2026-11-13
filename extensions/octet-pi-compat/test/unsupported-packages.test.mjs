// Packages octet deliberately does not load (UNSUPPORTED_PACKAGES): today only
// @narumitw/pi-goal, which octet's built-in /goal replaces. Discovery must skip
// the package (by npm name or resolved package.json name), say so once, and keep
// every other extension loading. pi-subagents is no longer skipped: it loads
// through the adapter like any other package. Synthetic filesystem only.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureMirror } from '../configure.mjs';
import { discoverPiSetup } from '../lib/pi-setup.mjs';
import { host, launch } from './helper.mjs';

const LINE = 'pi-goal: octet uses its built-in /goal';
// Executing this factory is the failure the user used to see.
const explode = 'throw new Error("ModelRuntime.create is unavailable: pi-goal must not load"); export default () => {};';
const kept = 'export default pi => pi.registerCommand("kept", {handler(){}});';

function put(path, value) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, typeof value === 'string' ? value : JSON.stringify(value));
  return path;
}
function fixture(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-unsupported-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const agent = join(dir, 'agent');
  put(join(agent, 'extensions/kept.js'), kept);
  return { dir, agent, output: join(dir, 'extensions/octet-pi-compat'), discover: options => discoverPiSetup({ agentDir: agent, cwd: dir, ...options }) };
}
// The host HOME can add unrelated notes (for example a symlinked temp home);
// only package diagnostics matter here.
const packageNotes = setup => setup.diagnostics.filter(line => /package|pi-goal|ModelRuntime/.test(line));
function installNpm(agent, name, version = '1.2.3') {
  const root = join(agent, 'npm/node_modules', name);
  put(join(root, 'package.json'), { name, version });
  return put(join(root, 'extensions/index.ts'), explode);
}

test('pi-goal by npm name is skipped once, with no diagnostics, and other extensions load', t => {
  const f = fixture(t);
  installNpm(f.agent, '@narumitw/pi-goal');
  const other = installNpm(f.agent, 'pi-other');
  put(join(f.agent, 'settings.json'), { packages: ['npm:@narumitw/pi-goal', 'npm:pi-other'] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js'), other]);
  assert.deepEqual(packageNotes(setup), []);
  assert.deepEqual(setup.unsupportedPackages, [{ match: '@narumitw/pi-goal', line: LINE }]);
});

test('pi-goal is skipped even when pinned, object-form, or not installed', t => {
  for (const pkg of ['npm:@narumitw/pi-goal@1.2.3', { source: 'npm:@narumitw/pi-goal', extensions: ['extensions/index.ts'] }, 'npm:@narumitw/pi-goal@^9.0.0']) {
    const f = fixture(t);
    put(join(f.agent, 'settings.json'), { packages: [pkg] });
    const setup = f.discover();
    assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js')]);
    assert.deepEqual(packageNotes(setup), [], JSON.stringify(pkg));
    assert.equal(setup.unsupportedPackages.length, 1);
  }
});

test('the same package in user and project scope is reported once', t => {
  const f = fixture(t);
  put(join(f.agent, 'settings.json'), { packages: ['npm:@narumitw/pi-goal'] });
  put(join(f.dir, '.pi/settings.json'), { packages: ['npm:@narumitw/pi-goal@1.2.3'] });
  const setup = f.discover({ projectTrusted: true });
  assert.equal(setup.unsupportedPackages.length, 1);
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js')]);
});

test('a local or git source is matched by its resolved package.json name', t => {
  const f = fixture(t);
  const local = join(f.dir, 'vendor/renamed');
  put(join(local, 'package.json'), { name: '@narumitw/pi-goal', version: '9.9.9' });
  put(join(local, 'extensions/index.ts'), explode);
  const git = join(f.agent, 'git/github.com/nicobailon/anything');
  put(join(git, 'package.json'), { name: '@narumitw/pi-goal', version: '1.0.0' });
  put(join(git, 'extensions/index.ts'), explode);
  put(join(f.agent, 'settings.json'), { packages: [local, 'git:github.com/nicobailon/anything'] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js')]);
  assert.deepEqual(packageNotes(setup), []);
  assert.equal(setup.unsupportedPackages.length, 1);
});

test('packages that merely contain the string are not skipped', t => {
  const f = fixture(t);
  const names = ['@narumitw/pi-goal-extra', 'my-pi-goal', '@owner/pi-goal'];
  const roots = names.map(name => installNpm(f.agent, name));
  // Directory named pi-goal whose manifest says otherwise.
  const lookalike = join(f.dir, 'pi-goal');
  put(join(lookalike, 'package.json'), { name: 'unrelated', version: '1.0.0' });
  put(join(lookalike, 'extensions/index.ts'), kept);
  put(join(f.agent, 'settings.json'), { packages: [...names.map(name => `npm:${name}`), lookalike] });
  const setup = f.discover();
  assert.deepEqual(setup.unsupportedPackages, []);
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js'), ...roots, join(lookalike, 'extensions/index.ts')]);
});

test('end to end: configure and runtime print one line, never execute the package, and keep other extensions', async t => {
  const f = fixture(t);
  installNpm(f.agent, '@narumitw/pi-goal');
  put(join(f.agent, 'settings.json'), { packages: ['npm:@narumitw/pi-goal'] });
  const lines = [];
  const env = { ...process.env, HOME: f.dir, OCTET_PI_AGENT_DIR: f.agent };
  const captured = await configureMirror({ output: f.output, cwd: f.dir, env, reviewed: true, log: line => lines.push(line) });
  assert.equal(lines.filter(line => line.includes(LINE)).length, 1, lines.join('\n'));
  assert.ok(lines.every(line => !/ModelRuntime|refused|error/i.test(line)), lines.join('\n'));
  assert.deepEqual(captured.config.extensions, [join(f.agent, 'extensions/kept.js')]);

  const p = launch(t, [], { cwd: f.dir, config: join(f.output, 'bridge.json'), metadata: captured.registrations,
    env: { HOME: join(f.dir, 'relocated'), OCTET_PI_AGENT_DIR: '/missing', PI_CODING_AGENT_DIR: '/wrong' } });
  const contributes = { tools: captured.registrations.tools.map(tool => tool.name), commands: captured.registrations.commands.map(command => command.name), hooks: captured.registrations.hooks,
    tool_renderers: captured.registrations.tool_renderers, shortcuts: captured.registrations.shortcuts, flags: captured.registrations.flags };
  const initialized = await p.request('initialize', { api_version: '0.4', workspace: p.context().workspace,
    host: { ...host, has_ui: false, project_trusted: false }, contributes, capabilities: { builtin_tool_overrides: [] }, flag_values: [],
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: ['shortcuts', 'resource_paths_v1', 'session_entries', 'runtime_commands', 'dynamic_tools', 'builtin_tool_overrides_v1'], limits: { max_concurrent_requests: 8 } } }).response;
  assert.ok(initialized.result, JSON.stringify(initialized));
  assert.ok((await p.command('kept', [], { has_ui: false }).response).result);
  const stderr = await p.waitStderr(/pi-goal: octet uses its built-in \/goal/);
  assert.equal(stderr.split(LINE).length - 1, 1, stderr);
  assert.doesNotMatch(stderr, /ModelRuntime|must not load/);
  await p.close();
});

test('pi-subagents loads like any other package', t => {
  const f = fixture(t);
  const root = join(f.agent, 'npm/node_modules/pi-subagents');
  put(join(root, 'package.json'), { name: 'pi-subagents', version: '0.59.0' });
  const entry = put(join(root, 'extensions/index.ts'), kept);
  put(join(f.agent, 'settings.json'), { packages: ['npm:pi-subagents@0.59.0'] });
  const setup = f.discover();
  assert.deepEqual(setup.unsupportedPackages, []);
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js'), entry]);
});
