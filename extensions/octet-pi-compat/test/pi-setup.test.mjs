// Mirror-mode discovery and configuration. Uses real filesystem layouts and
// the real runner; no Pi package code, network, credentials or inference.
import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { configure, configureMirror, MIRROR_SUBSCRIBED_HOOKS, preparePiMirror } from '../configure.mjs';
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
  put(join(agent, 'extensions/two.js'), 'export default pi => pi.registerCommand("two", {handler(){}});');
  put(join(agent, 'extensions/ignored.mjs'), 'not automatically discovered by Pi');
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
  put(join(root, '.pi/extensions/project.js'), 'export default pi => pi.registerCommand("project", {handler(){}});');
  put(join(root, '.pi/skills/project-scope/SKILL.md'), '# project skill\n');
  put(join(root, '.pi/settings.json'), JSON.stringify({ prompts: ['project-prompts'] }));
  put(join(root, '.pi/project-prompts/project.md'), 'project $1\n');
  const setup = discoverPiSetup({ agentDir: agent, cwd: root, projectTrusted: true });
  assert.deepEqual(setup.diagnostics, []);
  assert.deepEqual(setup.extensions, [
    join(root, '.pi/extensions/project.js'),
    join(agent, 'elsewhere/extra.ts'),
    join(agent, 'extensions/one.mjs'),
    join(agent, 'extensions/two.js'),
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

test('pi setup: preparation returns the ordered enabled setup and stale reviews refuse before factory execution', async t => {
  const root = temporary(t), agent = piHome(root), output = join(root, 'extensions/octet-pi-compat');
  const marker = join(root, 'factory-was-executed');
  put(join(agent, 'extensions/one.mjs'), `import { writeFileSync } from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, 'executed'); export default pi => pi.registerCommand('one', {handler(){}});`);
  const review = preparePiMirror({ output, cwd: root, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } });
  const setup = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.equal(review.schema, 'octet.pi-setup-mirror/v1');
  assert.match(review.fingerprint, /^[a-f0-9]{64}$/);
  assert.equal(review.overwriteRequired, false);
  assert.deepEqual(review.extensions.map(item => item.path), setup.extensions);
  assert.ok(review.extensions.every(item => item.reviewable && item.sources.length));
  assert.deepEqual(review.resources.skills, setup.skillsPaths);
  assert.deepEqual(review.resources.prompts, setup.promptsPaths);
  assert.deepEqual(review.defaults.model, setup.defaultModel);
  assert.equal(existsSync(marker), false);
  assert.equal(existsSync(output), false);
  put(join(agent, 'extensions/one.mjs'), `// changed after preparation\nimport { writeFileSync } from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, 'executed'); export default pi => pi.registerCommand('changed', {handler(){}});`);
  await assert.rejects(configureMirror({ output, reviewed: true, expectedFingerprint: review.fingerprint, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } }), /changed since preparation/);
  assert.equal(existsSync(marker), false);
  assert.equal(existsSync(output), false);
});

test('pi setup: resources remain complete above 64, accept 256, and refuse partial setup at 257', async t => {
  const root = temporary(t), cwd = root;
  const skills = count => {
    const agent = join(root, `agent-${count}`);
    put(join(agent, 'settings.json'), '{}');
    for (let index = 0; index < count; index++) put(join(agent, `skills/skill-${String(index).padStart(3, '0')}/SKILL.md`), '# skill\\n');
    return agent;
  };
  const abovePrevious = discoverPiSetup({ agentDir: skills(65), cwd });
  assert.equal(abovePrevious.skillsPaths.length, 65);
  assert.deepEqual(abovePrevious.resourceLimitsExceeded, []);

  const agent = skills(255);
  put(join(agent, 'skills/zz-extra/a/SKILL.md'), '# nested skill\\n');
  let setup = discoverPiSetup({ agentDir: agent, cwd });
  assert.equal(setup.skillsPaths.length, 256);
  assert.deepEqual(setup.resourceLimitsExceeded, []);

  put(join(agent, 'skills/zz-extra/b/SKILL.md'), '# nested skill\\n');
  setup = discoverPiSetup({ agentDir: agent, cwd });
  assert.deepEqual(setup.skillsPaths, []);
  assert.deepEqual(setup.resourceLimitsExceeded, [
    { dimension: 'kind', kind: 'skills', actual: 257, limit: 256 },
    { dimension: 'paths', kind: 'combined', actual: 257, limit: 256 },
  ]);
  const output = join(root, 'extensions/octet-pi-compat');
  const env = { ...process.env, OCTET_PI_AGENT_DIR: agent };
  const review = preparePiMirror({ output, cwd, env });
  assert.deepEqual(review.resourceLimitsExceeded, setup.resourceLimitsExceeded);
  await assert.rejects(configureMirror({ output, reviewed: true, expectedFingerprint: review.fingerprint, cwd, env, log: () => {} }), /resource bounds.*no partial mirror configured/);
  assert.equal(existsSync(output), false);

  const flatAgent = skills(257);
  setup = discoverPiSetup({ agentDir: flatAgent, cwd });
  assert.equal(setup.skillsPaths.length, 256);
  assert.deepEqual(setup.resourceLimitsExceeded, [
    { dimension: 'directory_entries', kind: 'directory', actual: 257, limit: 256 },
  ]);
  const flatEnv = { ...process.env, OCTET_PI_AGENT_DIR: flatAgent };
  const flatReview = preparePiMirror({ output, cwd, env: flatEnv });
  await assert.rejects(configureMirror({ output, reviewed: true, expectedFingerprint: flatReview.fingerprint, cwd, env: flatEnv, log: () => {} }), /resource bounds.*no partial mirror configured/);
  assert.equal(existsSync(output), false);
});

test('pi setup: the combined 256-path review budget covers all five resource kinds and refuses the complete 257-path setup', async t => {
  const root = temporary(t), agent = join(root, 'combined-agent'), output = join(root, 'extensions/octet-pi-compat');
  put(join(agent, 'settings.json'), '{}');
  put(join(agent, 'keybindings.json'), '{}');
  put(join(agent, 'AGENTS.md'), 'context');
  for (let index = 0; index < 100; index++) put(join(agent, `skills/skill-${String(index).padStart(3, '0')}/SKILL.md`), '# skill');
  for (let index = 0; index < 100; index++) put(join(agent, `prompts/prompt-${String(index).padStart(3, '0')}.md`), '# prompt');
  for (let index = 0; index < 54; index++) put(join(agent, `themes/theme-${String(index).padStart(3, '0')}.json`), '{}');
  const env = { ...process.env, OCTET_PI_AGENT_DIR: agent };
  let setup = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.equal(setup.skillsPaths.length + setup.promptsPaths.length + setup.themesPaths.length + Number(!!setup.keybindingsPath) + Number(!!setup.contextPath), 256);
  assert.deepEqual(setup.resourceLimitsExceeded, []);
  put(join(agent, 'skills/skill-100/SKILL.md'), '# skill');
  setup = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.deepEqual(setup.resourceLimitsExceeded, [{ dimension: 'paths', kind: 'combined', actual: 257, limit: 256 }]);
  const review = preparePiMirror({ output, cwd: root, env });
  assert.equal(review.resources.skills.length + review.resources.prompts.length + review.resources.themes.length + Number(!!review.resources.keybindings) + Number(!!review.resources.context), 257);
  await assert.rejects(configureMirror({ output, reviewed: true, expectedFingerprint: review.fingerprint, cwd: root, env, log: () => {} }), /resource bounds.*no partial mirror configured/);
  assert.equal(existsSync(output), false);
});

test('pi setup: managed npm packages resolve without installing and unsupported sources are refused', async t => {
  const root = temporary(t);
  const agent = join(root, 'home/.pi/agent');
  put(join(agent, 'settings.json'), JSON.stringify({ packages: ['npm:pi-review', { source: 'npm:pi-filtered', skills: ['custom/**'], autoload: false }, 'git:github.com/example/pi-git', 'npm:missing-package'] }));
  const pkg = join(agent, 'npm/node_modules/pi-review');
  put(join(pkg, 'package.json'), JSON.stringify({ version: '1.0.0' }));
  put(join(pkg, 'extensions/review.js'), 'export default pi => pi.registerCommand("review", {handler(){}});');
  put(join(pkg, 'skills/review/SKILL.md'), '# review\n');
  const filtered = join(agent, 'npm/node_modules/pi-filtered');
  put(join(filtered, 'package.json'), JSON.stringify({ version: '1.0.0', pi: { skills: ['custom'] } }));
  put(join(filtered, 'skills/ignored/SKILL.md'), '# ignored\n');
  put(join(filtered, 'custom/kept/SKILL.md'), '# kept\n');
  const setup = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.deepEqual(setup.extensions, [join(pkg, 'extensions/review.js')]);
  assert.ok(setup.skillsPaths.includes(join(pkg, 'skills/review/SKILL.md')));
  assert.ok(setup.skillsPaths.includes(join(filtered, 'custom/kept/SKILL.md')));
  assert.ok(!setup.skillsPaths.includes(join(filtered, 'skills/ignored/SKILL.md')));
  assert.ok(setup.diagnostics.some(line => line.includes('git:github.com/example/pi-git') && line.includes('never installs')));
  assert.ok(setup.diagnostics.some(line => line.includes('missing-package') && line.includes('never installs')));
});

test('pi setup: ordered autoload deltas, local-file packages and trusted .agents skills resolve without loading disabled packages', t => {
  const root = temporary(t), agent = join(root, 'home/.pi/agent');
  put(join(agent, 'settings.json'), JSON.stringify({ packages: [
    { source: 'npm:delta', autoload: false, extensions: ['-extensions/a.js', '+extensions/b.js'] },
    { source: 'npm:disabled', autoload: false },
    { source: 'loose/local.mjs', autoload: false, extensions: [] },
  ] }));
  const pkg = join(agent, 'npm/node_modules/delta');
  put(join(pkg, 'package.json'), JSON.stringify({ version: '1.0.0', pi: {} }));
  put(join(pkg, 'extensions/a.js'), 'throw new Error("disabled delta target must not execute");');
  put(join(pkg, 'extensions/b.js'), 'throw new Error("enabled delta target is only discovered");');
  put(join(agent, 'loose/local.mjs'), 'throw new Error("local package is only discovered");');
  put(join(root, '.git/keep'), '');
  put(join(root, '.agents/skills/local/SKILL.md'), '# local agent skill\n');
  const setup = discoverPiSetup({ agentDir: agent, cwd: root, projectTrusted: true });
  assert.deepEqual(setup.extensions, [join(pkg, 'extensions/b.js'), join(agent, 'loose/local.mjs')]);
  assert.ok(setup.skillsPaths.includes(join(root, '.agents/skills/local/SKILL.md')));
  assert.ok(!setup.diagnostics.some(line => line.includes('npm:disabled')));
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
  assert.match(bridge.pi_setup_fingerprint, /^[a-f0-9]{64}$/);
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
  put(join(agent, 'settings.json'), JSON.stringify({ extensions: [entry] }));
  put(entry, 'export default pi => pi.registerCommand("reviewed", {handler(){}});');
  const output = join(root, 'extensions/octet-pi-compat');
  await configureMirror({ output, reviewed: true, cwd: root, log: () => {}, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } });
  put(entry, 'export default pi => pi.on("before_provider_request", () => {});');
  const adapter = fileURLToPath(new URL('../', import.meta.url));
  const result = spawnSync(process.execPath, [join(adapter, 'runner.mjs'), '--config', join(output, 'bridge.json'), '--inspect'], { cwd: root, encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /(?:changed|source|review)/);
});

test('pi setup: non-mirror palette discovery respects package deltas and live project trust without reordering factory roots', async t => {
  const root = temporary(t), agent = piHome(root), output = join(root, 'extensions/octet-pi-compat');
  const palette = name => JSON.stringify({ name, appearance: 'dark', colors: Object.fromEntries([
    ...foregroundTokens.map(token => [token, '#ffffff']), ...backgroundTokens.map(token => [token, '']),
  ]) });
  const settings = JSON.parse(readFileSync(join(agent, 'settings.json'), 'utf8'));
  settings.packages = [
    { source: 'npm:filtered@1.2.3', autoload: false, themes: ['themes/kept.json'] },
    { source: 'npm:disabled@1.2.3', autoload: false },
    { source: 'npm:empty@1.2.3', themes: [] },
  ];
  put(join(agent, 'settings.json'), JSON.stringify(settings));
  for (const name of ['filtered', 'disabled', 'empty']) {
    const pkg = join(agent, 'npm/node_modules', name);
    put(join(pkg, 'package.json'), JSON.stringify({ version: '1.2.3', pi: { themes: ['themes'] } }));
    put(join(pkg, 'themes/kept.json'), palette(`${name}-kept`));
    put(join(pkg, 'themes/excluded.json'), palette(`${name}-excluded`));
    put(join(pkg, 'extensions/never.mjs'), 'throw new Error("inventory must not execute package code");');
  }
  const project = join(root, '.pi/themes/project.json'), later = join(root, 'factory-root/pi-setup-theme.json');
  put(project, palette('project')); put(later, '{');
  // The malformed later same-stem file must reach native winner resolution;
  // the adapter must not remove it and revive the earlier valid palette.
  const factory = join(root, 'manual.mjs');
  put(factory, `export default pi => pi.on('resources_discover', () => ({themePaths:[${JSON.stringify(later)}]}));`);
  const { registrations: metadata } = configure({ output, reviewed: true, extensions: [factory], piAgentDir: agent, cwd: root });
  const peer = launch(t, [factory], { config: join(output, 'bridge.json'), metadata, cwd: root });
  await peer.init(['resource_paths_v1', 'session_entries']); await peer.start();
  for (const project_trusted of [false, true, false]) {
    const setup = discoverPiSetup({ agentDir: agent, cwd: root, projectTrusted: project_trusted });
    const response = await peer.request('hook/run', { hook: 'resources_discover', payload: { cwd: root, reason: 'reload' }, context: peer.context({ project_trusted }) }).response;
    assert.deepEqual(response.result.resource_paths, { skill_paths: [], prompt_paths: [], theme_paths: [...setup.themesPaths, later] });
    assert.equal(response.result.resource_paths.theme_paths.includes(project), project_trusted);
    assert.ok(response.result.resource_paths.theme_paths.includes(join(agent, 'npm/node_modules/filtered/themes/kept.json')));
    assert.ok(!response.result.resource_paths.theme_paths.some(path => /excluded|disabled|empty/.test(path)));
  }
  await peer.close();
});
