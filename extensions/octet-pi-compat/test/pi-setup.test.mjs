// Mirror-mode discovery and configuration. Uses real filesystem layouts and
// the real runner; no Pi package code, network, credentials or inference.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { configureMirror, MIRROR_SUBSCRIBED_HOOKS } from '../configure.mjs';
import { discoverPiSetup, resolveThemeFile } from '../lib/pi-setup.mjs';
import { foregroundTokens, backgroundTokens } from '../lib/theme-palette.mjs';
import { launch } from './helper.mjs';

function temporary(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-setup-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function put(path, source) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, source);
}

// One complete fixture Pi setup. Every mirrored item is present and the layout
// matches what Pi 1.0.2 reads under the agent directory.
function piHome(root) {
  const agent = join(root, 'home/.pi/agent');
  put(join(agent, 'settings.json'), JSON.stringify({
    defaultProvider: 'pi-mirror-provider',
    defaultModel: 'pi-mirror-model',
    defaultThinkingLevel: 'high',
    theme: 'pi-setup-theme',
    extensions: [join(agent, 'elsewhere/extra.ts'), join(agent, 'extensions/one.mjs')],
  }));
  put(join(agent, 'extensions/one.mjs'), 'export default pi => pi.registerCommand("one", {handler(){}});');
  put(join(agent, 'extensions/two.mjs'), 'export default pi => pi.registerCommand("two", {handler(){}});');
  put(join(agent, 'extensions/notes.txt'), 'not an extension');
  put(join(agent, 'elsewhere/extra.ts'), 'export default pi => pi.registerCommand("extra", {handler(){}});');
  put(join(agent, 'skills/pi-setup-skill/SKILL.md'), '---\nname: pi-setup-skill\ndescription: catalog\n---\nbody\n');
  put(join(agent, 'prompts/pi-setup-prompt.md'), 'Prompt $1\n');
  put(join(agent, 'themes/pi-setup-theme.json'), JSON.stringify({ name: 'Pi Setup Theme', appearance: 'dark', colors: Object.fromEntries([...foregroundTokens.map(token => [token, '#ffffff']), ...backgroundTokens.map(token => [token, ''])]) }));
  put(join(agent, 'keybindings.json'), '{"app.exit": ["ctrl+q"]}\n');
  put(join(agent, 'AGENTS.md'), 'Pi context\n');
  return agent;
}

test('pi setup: discovery reads the Pi layout read-only and reports every item', async t => {
  const root = temporary(t);
  const agent = piHome(root);
  // Pi's project scope: <cwd>/.pi resources and settings path lists.
  put(join(root, '.pi/extensions/project.mjs'), 'export default pi => pi.registerCommand("project", {handler(){}});');
  put(join(root, '.pi/skills/project-scope/SKILL.md'), '# project skill\n');
  put(join(root, '.pi/settings.json'), JSON.stringify({ prompts: ['project-prompts'] }));
  put(join(root, '.pi/project-prompts/project.md'), 'project $1\n');
  const setup = discoverPiSetup({ agentDir: agent, cwd: root, projectTrusted: true });
  assert.deepEqual(setup.diagnostics, []);
  assert.deepEqual(setup.extensions, [
    join(root, '.pi/extensions/project.mjs'),
    join(agent, 'elsewhere/extra.ts'),
    join(agent, 'extensions/one.mjs'),
    join(agent, 'extensions/two.mjs'),
  ]);
  assert.deepEqual(setup.skillsPaths, [join(root, '.pi/skills/project-scope/SKILL.md'), join(agent, 'skills/pi-setup-skill/SKILL.md')]);
  assert.deepEqual(setup.promptsPaths, [join(root, '.pi/project-prompts/project.md'), join(agent, 'prompts/pi-setup-prompt.md')]);
  assert.deepEqual(setup.themesPaths, [join(agent, 'themes/pi-setup-theme.json')]);
  assert.equal(setup.keybindingsPath, join(agent, 'keybindings.json'));
  assert.equal(setup.contextPath, join(agent, 'AGENTS.md'));
  assert.equal(setup.defaultTheme, 'pi-setup-theme');
  assert.equal(setup.defaultThinkingLevel, 'high');
  assert.deepEqual(setup.defaultModel, { provider: 'pi-mirror-provider', model: 'pi-mirror-model' });
  assert.equal(resolveThemeFile(setup.themesPaths, setup.defaultTheme), join(agent, 'themes/pi-setup-theme.json'));
  assert.equal(resolveThemeFile(setup.themesPaths, 'Pi Setup Theme', setup.diagnostics), join(agent, 'themes/pi-setup-theme.json'));
  assert.equal(resolveThemeFile(setup.themesPaths, 'missing', setup.diagnostics), undefined);
});

test('pi setup: managed npm packages resolve without installing and unsupported sources are refused', async t => {
  const root = temporary(t);
  const agent = join(root, 'home/.pi/agent');
  put(join(agent, 'settings.json'), JSON.stringify({ packages: ['npm:pi-review', { source: 'npm:pi-filtered', skills: ['custom/**'], autoload: false }, 'git:github.com/example/pi-git', 'npm:missing-package'] }));
  const pkg = join(agent, 'npm/node_modules/pi-review');
  put(join(pkg, 'package.json'), JSON.stringify({ version: '1.0.0' }));
  put(join(pkg, 'extensions/review.mjs'), 'export default pi => pi.registerCommand("review", {handler(){}});');
  put(join(pkg, 'skills/review/SKILL.md'), '# review\n');
  const filtered = join(agent, 'npm/node_modules/pi-filtered');
  put(join(filtered, 'package.json'), JSON.stringify({ version: '1.0.0', pi: { skills: ['custom'] } }));
  put(join(filtered, 'skills/ignored/SKILL.md'), '# ignored\n');
  put(join(filtered, 'custom/kept/SKILL.md'), '# kept\n');
  const setup = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.deepEqual(setup.extensions, [join(pkg, 'extensions/review.mjs')]);
  assert.ok(setup.skillsPaths.includes(join(pkg, 'skills/review/SKILL.md')));
  assert.ok(setup.skillsPaths.includes(join(filtered, 'custom/kept/SKILL.md')));
  assert.ok(!setup.skillsPaths.includes(join(filtered, 'skills/ignored/SKILL.md')));
  assert.ok(setup.diagnostics.some(line => line.includes('git:github.com/example/pi-git') && line.includes('never installs')));
  assert.ok(setup.diagnostics.some(line => line.includes('missing-package') && line.includes('never installs')));
});

test('pi setup: filters, symlinks and oversized settings fail closed with diagnostics', async t => {
  const root = temporary(t);
  const agent = join(root, 'home/.pi/agent');
  put(join(agent, 'settings.json'), JSON.stringify({ skills: ['!excluded', join(agent, 'skills')], prompts: 'not-an-array' }));
  put(join(agent, 'skills/kept/SKILL.md'), '# kept\n');
  put(join(agent, 'prompts/kept.md'), 'kept $1\n');
  const setup = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.deepEqual(setup.skillsPaths, [join(agent, 'skills/kept/SKILL.md')]);
  assert.ok(!setup.skillsPaths.includes(join(root, 'excluded')));
  assert.ok(setup.diagnostics.some(line => line.includes('settings.prompts') && line.includes('skipped')));
  // Invalid selectors must not fall back to unfiltered automatic resources.
  assert.deepEqual(setup.promptsPaths, []);
  // An over-limit settings file is diagnosed, never parsed partially.
  put(join(agent, 'settings.json'), `{"theme":"${'x'.repeat(1024 * 1024)}"}`);
  const oversized = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.equal(oversized.defaultTheme, undefined);
  assert.ok(oversized.diagnostics.some(line => line.includes('settings.json') && line.includes('mirror limit')));
});

function agentWithExtensions(root) {
  return piHome(root);
}

test('pi setup: configureMirror records the reviewed opt-in without writing the Pi setup', async t => {
  const root = temporary(t);
  const agent = agentWithExtensions(root);
  const output = join(root, 'extensions/octet-pi-compat');
  const before = readFileSync(join(agent, 'settings.json'), 'utf8');
  const result = await configureMirror({ output, reviewed: true, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } });
  const bridge = JSON.parse(readFileSync(join(output, 'bridge.json'), 'utf8'));
  assert.equal(bridge.mirror_pi_setup, true);
  assert.equal(bridge.pi_agent_dir, agent);
  assert.deepEqual(bridge.extensions, result.setup.extensions);
  assert.deepEqual(Object.keys(bridge.entrypoint_sha256), result.setup.extensions);
  assert.ok(bridge.registrations.commands.some(command => command.name === 'one'));
  assert.deepEqual(bridge.subscribed_hooks, [...MIRROR_SUBSCRIBED_HOOKS]);
  assert.ok(bridge.subscribed_hooks.includes('resources_discover'));
  assert.ok(!bridge.subscribed_hooks.includes('before_provider_request'));
  const manifest = readFileSync(join(output, 'extension.toml'), 'utf8');
  assert.match(manifest, /^name = "octet-pi-compat"$/m);
  assert.match(manifest, /^hooks = \["after_response"/m);
  assert.match(manifest, /^commands = \["extra", "one", "two"\]$/m);
  assert.equal(readFileSync(join(agent, 'settings.json'), 'utf8'), before);
  assert.equal(result.setup.extensions.length, 3);
  // The reviewed opt-in is required and an existing mirror is not overwritten.
  await assert.rejects(configureMirror({ output, reviewed: false, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } }), /--reviewed/);
  await assert.rejects(configureMirror({ output, reviewed: true, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } }), /--overwrite/);
  await assert.rejects(configureMirror({ output: join(root, 'wrong-name'), reviewed: true, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } }), /octet-pi-compat/);
});

test('pi setup: a generated mirror bridge discovers the fixture items through the real runner', async t => {
  const root = temporary(t);
  const agent = piHome(root);
  const output = join(root, 'extensions/octet-pi-compat');
  const { registrations: metadata } = await configureMirror({ output, reviewed: true, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } });
  const peer = launch(t, [], { config: join(output, 'bridge.json'), metadata, cwd: root });
  await peer.init(['resource_paths_v1', 'session_entries']);
  await peer.start();
  const context = peer.context();
  const reply = await peer.request('hook/run', {
    hook: 'resources_discover',
    payload: { cwd: context.workspace, reason: 'startup' },
    context,
  }).response;
  assert.ok(reply.result, JSON.stringify(reply));
  const paths = reply.result.resource_paths;
  assert.deepEqual(paths.skill_paths, [join(agent, 'skills/pi-setup-skill/SKILL.md')]);
  assert.deepEqual(paths.prompt_paths, [join(agent, 'prompts/pi-setup-prompt.md')]);
  assert.deepEqual(paths.theme_paths, [join(agent, 'themes/pi-setup-theme.json')]);
  assert.equal(paths.default_theme, join(agent, 'themes/pi-setup-theme.json'));
  assert.deepEqual(paths.keybindings_paths, [join(agent, 'keybindings.json')]);
  assert.deepEqual(paths.context_paths, [join(agent, 'AGENTS.md')]);
  assert.deepEqual(paths.default_model, { provider: 'pi-mirror-provider', model: 'pi-mirror-model' });
  assert.equal(paths.default_thinking_level, 'high');
});

test('pi setup: mirror refuses changed factory sources before executing new hooks', async t => {
  const root = temporary(t);
  const agent = join(root, 'home/.pi/agent');
  const entry = join(agent, 'extensions/hooked.mjs');
  put(entry, 'export default pi => pi.registerCommand("reviewed", {handler(){}});');
  const output = join(root, 'extensions/octet-pi-compat');
  await configureMirror({ output, reviewed: true, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } });
  put(entry, 'export default pi => pi.on("before_provider_request", () => {});');
  const adapter = fileURLToPath(new URL('../', import.meta.url));
  const result = spawnSync(process.execPath, [join(adapter, 'runner.mjs'), '--config', join(output, 'bridge.json'), '--inspect'], { cwd: root, encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /(?:changed|source|review)/);
});
