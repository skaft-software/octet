// @narumitw/pi-goal is not loaded: octet keeps its own built-in /goal, and a Pi
// package registering `goal` would shadow it. Discovery must skip the package
// (by npm name or resolved package.json name), say so once, and keep every other
// extension loading. pi-subagents is no longer skipped: it loads alongside.
// Synthetic filesystem only; no installed Pi, no network.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureMirror } from '../configure.mjs';
import { discoverPiSetup, UNSUPPORTED_PACKAGES } from '../lib/pi-setup.mjs';
import { host, launch } from './helper.mjs';

const GOAL_LINE = 'pi-goal: octet uses its built-in /goal';
// What the real package does that matters here: it owns the `goal` command.
const goalFactory = 'export default pi => { pi.registerCommand("goal", { description: "Pi goal", handler() {} }); };';
// Executing this factory is the failure the user used to see.
const explode = 'throw new Error("ModelRuntime.create is unavailable: this package must not load"); export default () => {};';
const kept = 'export default pi => pi.registerCommand("kept", {handler(){}});';

function put(path, value) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, typeof value === 'string' ? value : JSON.stringify(value));
  return path;
}
function fixture(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-goal-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const agent = join(dir, 'agent');
  put(join(agent, 'extensions/kept.js'), kept);
  return { dir, agent, output: join(dir, 'extensions/octet-pi-compat'), discover: options => discoverPiSetup({ agentDir: agent, cwd: dir, ...options }) };
}
const packageNotes = setup => setup.diagnostics.filter(line => /package|goal|subagents|ModelRuntime/.test(line));
function installNpm(agent, name, source = explode, version = '1.2.3') {
  const root = join(agent, 'npm/node_modules', name);
  put(join(root, 'package.json'), { name, version });
  return put(join(root, 'extensions/index.ts'), source);
}

test('the table holds exactly the one unsupported package, with one short line', () => {
  assert.deepEqual(UNSUPPORTED_PACKAGES.map(({ match, line }) => [match, line]), [
    ['@narumitw/pi-goal', GOAL_LINE],
  ]);
  assert.ok(UNSUPPORTED_PACKAGES.every(item => !item.line.includes('\n') && item.line.length < 80));
});

test('pi-goal is skipped once however it is declared, with no diagnostics, and other extensions load', t => {
  const declarations = [
    'npm:@narumitw/pi-goal',
    'npm:@narumitw/pi-goal@1.2.3',
    'npm:@narumitw/pi-goal@^9.0.0',
    // The shape in the real S01 setup: installed but autoload:false.
    { source: 'npm:@narumitw/pi-goal', autoload: false },
    { source: 'npm:@narumitw/pi-goal', extensions: ['extensions/index.ts'], skills: [] },
  ];
  for (const installed of [true, false]) for (const pkg of declarations) {
    const f = fixture(t);
    if (installed) installNpm(f.agent, '@narumitw/pi-goal', goalFactory);
    const other = installNpm(f.agent, 'pi-other', kept);
    put(join(f.agent, 'settings.json'), { packages: [pkg, 'npm:pi-other'] });
    const setup = f.discover();
    const label = JSON.stringify({ installed, pkg });
    assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js'), other], label);
    assert.deepEqual(packageNotes(setup), [], label);
    assert.deepEqual(setup.unsupportedPackages, [{ match: '@narumitw/pi-goal', line: GOAL_LINE }], label);
  }
});

test('pi-goal declared in user and project scope is reported once', t => {
  const f = fixture(t);
  put(join(f.agent, 'settings.json'), { packages: ['npm:@narumitw/pi-goal'] });
  put(join(f.dir, '.pi/settings.json'), { packages: [{ source: 'npm:@narumitw/pi-goal', autoload: false }] });
  const setup = f.discover({ projectTrusted: true });
  assert.equal(setup.unsupportedPackages.length, 1);
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js')]);
  assert.deepEqual(packageNotes(setup), []);
});

test('a local or git pi-goal is matched by its resolved package.json name', t => {
  const f = fixture(t);
  const local = join(f.dir, 'vendor/goal');
  put(join(local, 'package.json'), { name: '@narumitw/pi-goal', version: '0.1.0' });
  put(join(local, 'extensions/index.ts'), explode);
  const git = join(f.agent, 'git/github.com/someone/renamed');
  put(join(git, 'package.json'), { name: '@narumitw/pi-goal', version: '0.1.0' });
  put(join(git, 'extensions/index.ts'), explode);
  put(join(f.agent, 'settings.json'), { packages: [local, 'git:github.com/someone/renamed'] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js')]);
  assert.deepEqual(packageNotes(setup), []);
  assert.deepEqual(setup.unsupportedPackages, [{ match: '@narumitw/pi-goal', line: GOAL_LINE }]);
});

test('names that merely resemble an unsupported package are not skipped', t => {
  const f = fixture(t);
  const names = ['pi-goal', '@other/pi-goal', '@narumitw/pi-goal-extra', '@narumitw/pi-goals', 'my-pi-subagents', 'pi-subagents-extra', '@owner/pi-subagents'];
  const roots = names.map(name => installNpm(f.agent, name, kept));
  // A directory named like the package whose manifest says otherwise.
  const lookalike = join(f.dir, 'pi-goal');
  put(join(lookalike, 'package.json'), { name: 'unrelated', version: '1.0.0' });
  put(join(lookalike, 'extensions/index.ts'), kept);
  put(join(f.agent, 'settings.json'), { packages: [...names.map(name => `npm:${name}`), lookalike] });
  const setup = f.discover();
  assert.deepEqual(setup.unsupportedPackages, []);
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js'), ...roots, join(lookalike, 'extensions/index.ts')]);
});

test('pi-goal is skipped while pi-subagents in the same setup loads', t => {
  const f = fixture(t);
  const subagents = installNpm(f.agent, 'pi-subagents', kept);
  installNpm(f.agent, '@narumitw/pi-goal', goalFactory);
  const other = installNpm(f.agent, 'pi-other', kept);
  put(join(f.agent, 'settings.json'), { packages: ['npm:@narumitw/pi-goal', 'npm:pi-other', 'npm:pi-subagents@1.2.3'] });
  const setup = f.discover();
  assert.deepEqual(setup.unsupportedPackages.map(item => item.line), [GOAL_LINE]);
  assert.deepEqual(setup.extensions, [join(f.agent, 'extensions/kept.js'), other, subagents]);
  assert.deepEqual(packageNotes(setup), []);
});

async function start(t, f, config, registrations) {
  const p = launch(t, [], { cwd: f.dir, config, metadata: registrations,
    env: { HOME: join(f.dir, 'relocated'), OCTET_PI_AGENT_DIR: '/missing', PI_CODING_AGENT_DIR: '/wrong' } });
  const contributes = { tools: registrations.tools.map(tool => tool.name), commands: registrations.commands.map(command => command.name), hooks: registrations.hooks,
    tool_renderers: registrations.tool_renderers, shortcuts: registrations.shortcuts, flags: registrations.flags };
  const initialized = await p.request('initialize', { api_version: '0.4', workspace: p.context().workspace,
    host: { ...host, has_ui: false, project_trusted: false }, contributes, capabilities: { builtin_tool_overrides: [] }, flag_values: [],
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: ['shortcuts', 'resource_paths_v1', 'session_entries', 'runtime_commands', 'dynamic_tools', 'builtin_tool_overrides_v1'], limits: { max_concurrent_requests: 8 } } }).response;
  assert.ok(initialized.result, JSON.stringify(initialized));
  return p;
}

test('end to end: pi-goal never registers /goal, so the built-in /goal is the one that answers', async t => {
  // Control: the same factory under an ordinary package name does register
  // `goal`, which is what would shadow octet's built-in command.
  const control = fixture(t);
  installNpm(control.agent, 'pi-goal-fork', goalFactory);
  put(join(control.agent, 'settings.json'), { packages: ['npm:pi-goal-fork'] });
  const controlled = await configureMirror({ output: control.output, cwd: control.dir, env: { ...process.env, HOME: control.dir, OCTET_PI_AGENT_DIR: control.agent }, reviewed: true, log: () => {} });
  assert.ok(controlled.registrations.commands.some(command => command.name === 'goal'), 'the control package must register goal');

  const f = fixture(t);
  installNpm(f.agent, '@narumitw/pi-goal', goalFactory);
  put(join(f.agent, 'settings.json'), { packages: [{ source: 'npm:@narumitw/pi-goal', autoload: false }, 'npm:@narumitw/pi-goal'] });
  const lines = [];
  const captured = await configureMirror({ output: f.output, cwd: f.dir, env: { ...process.env, HOME: f.dir, OCTET_PI_AGENT_DIR: f.agent }, reviewed: true, log: line => lines.push(line) });
  assert.equal(lines.filter(line => line.includes(GOAL_LINE)).length, 1, lines.join('\n'));
  assert.ok(lines.every(line => !/ModelRuntime|refused|error/i.test(line)), lines.join('\n'));
  assert.deepEqual(captured.config.extensions, [join(f.agent, 'extensions/kept.js')]);
  assert.ok(captured.registrations.commands.every(command => command.name !== 'goal'), JSON.stringify(captured.registrations.commands));

  const p = await start(t, f, join(f.output, 'bridge.json'), captured.registrations);
  assert.ok((await p.command('kept', [], { has_ui: false }).response).result);
  const stderr = await p.waitStderr(/pi-goal: octet uses its built-in \/goal/);
  assert.equal(stderr.split(GOAL_LINE).length - 1, 1, stderr);
  assert.doesNotMatch(stderr, /ModelRuntime|must not load/);
  await p.close();
});

test('a setup reviewed before the package became unsupported still starts, with one line', async t => {
  const f = fixture(t);
  // Reviewed while the package was an ordinary one named differently...
  const dir = join(f.dir, 'vendor/goal');
  put(join(dir, 'package.json'), { name: 'my-goal', version: '1.0.0' });
  put(join(dir, 'extensions/index.ts'), goalFactory);
  put(join(f.agent, 'settings.json'), { packages: [dir] });
  const captured = await configureMirror({ output: f.output, cwd: f.dir, env: { ...process.env, HOME: f.dir, OCTET_PI_AGENT_DIR: f.agent }, reviewed: true, log: () => {} });
  assert.ok(captured.registrations.commands.some(command => command.name === 'goal'));
  assert.equal(captured.config.extensions.length, 2);
  // ...then the same directory turns out to be the unsupported package.
  put(join(dir, 'package.json'), { name: '@narumitw/pi-goal', version: '1.0.0' });
  const config = JSON.parse(readFileSync(join(f.output, 'bridge.json'), 'utf8'));
  assert.equal(config.mirror_pi_setup, true);

  const p = await start(t, f, join(f.output, 'bridge.json'), captured.registrations);
  assert.ok((await p.command('kept', [], { has_ui: false }).response).result);
  // The reviewed `goal` command is declared but never registered at runtime.
  const goal = await p.command('goal', [], { has_ui: false }).response;
  assert.ok(goal.error, JSON.stringify(goal));
  const stderr = await p.waitStderr(/pi-goal: octet uses its built-in \/goal/);
  assert.equal(stderr.split(GOAL_LINE).length - 1, 1, stderr);
  assert.doesNotMatch(stderr, /ModelRuntime|must not load|reviewed|configure again/);
  await p.close();
});
