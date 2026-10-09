import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, realpathSync, existsSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureFromPi } from '../configure.mjs';
import { foregroundTokens, backgroundTokens } from '../lib/theme-palette.mjs';
import { launch } from './helper.mjs';

function temporary(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-theme-import-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}
function palette(name, accent = '#123456') {
  return { name, appearance: 'dark', colors: Object.fromEntries([
    ...foregroundTokens.map(token => [token, accent]), ...backgroundTokens.map(token => [token, '']),
    ['text', ''], ['thinkingHigh', '#7e57c2'], ['thinkingXhigh', '#c792ea'],
  ]) };
}
function theme(dir, file, data) {
  mkdirSync(dir, { recursive: true });
  const path = join(dir, file + '.json');
  writeFileSync(path, JSON.stringify(data));
  return path;
}
// An isolated managed installation with the actual resolver/settings shape.
// No user factories, real Pi imports, network, credentials or inference.
function install(dir, themes, settings = {}) {
  const agent = join(dir, 'pi-agent');
  mkdirSync(join(agent, 'install'), { recursive: true });
  writeFileSync(join(agent, 'install/current-version'), '1.0.2\n');
  writeFileSync(join(agent, 'settings.json'), JSON.stringify(settings));
  const entry = join(dir, 'factory.ts');
  writeFileSync(entry, 'export default pi => pi.registerCommand("fixture", {handler(){}});');
  const sources = {
    'pi-coding-agent': `import {readFileSync} from 'node:fs';
export class SettingsManager {
  static create(cwd, agentDir) { const value = new SettingsManager(); value.settings = JSON.parse(readFileSync(agentDir+'/settings.json','utf8')); return value; }
  getThemeSetting() { return this.settings.theme; }
  getDefaultThinkingLevel() { return this.settings.defaultThinkingLevel; }
}
export class DefaultPackageManager {
  async resolve(onMissing) { if (await onMissing() !== 'skip') throw new Error('must not install'); return ${JSON.stringify({ extensions: [{ path: entry, enabled: true }], themes })}; }
}`,
    'pi-ai': 'export {};', 'pi-tui': 'export {};', 'pi-agent-core': 'export {};',
  };
  let builtinDir;
  for (const [name, source] of Object.entries(sources)) {
    const pkg = join(agent, 'install/releases/1.0.2/node_modules/@earendil-works', name);
    mkdirSync(join(pkg, 'dist/providers'), { recursive: true });
    writeFileSync(join(pkg, 'package.json'), JSON.stringify({ name: '@earendil-works/' + name, version: '1.0.2', type: 'module',
      exports: { '.': './dist/index.js', ...(name === 'pi-ai' ? { './compat': './dist/index.js', './oauth': './dist/index.js', './providers/*': './dist/providers/*.js' } : {}) } }));
    writeFileSync(join(pkg, 'dist/index.js'), source);
    writeFileSync(join(pkg, 'dist/providers/all.js'), 'export {};');
    if (name === 'pi-coding-agent') {
      builtinDir = join(pkg, 'dist/modes/interactive/theme');
      theme(builtinDir, 'dark', palette('dark'));
      theme(builtinDir, 'light', { ...palette('light'), appearance: 'light' });
    }
  }
  return { agent, builtinDir };
}

test('from-Pi imports enabled palettes as native themes, stages the Pi default and leaves host/Pi settings untouched', async t => {
  const dir = temporary(t), source = join(dir, 'source');
  const first = theme(source, 'filename-not-selector', palette('ghostty-dark'));
  const duplicate = theme(source, 'duplicate', palette('ghostty-dark', '#ffffff'));
  const disabled = theme(source, 'disabled', palette('disabled'));
  const { agent } = install(dir, [
    { path: first, enabled: true }, { path: duplicate, enabled: true }, { path: disabled, enabled: false },
  ], { theme: 'ghostty-dark', defaultThinkingLevel: 'high' });
  const home = join(dir, 'home');
  mkdirSync(join(home, '.octet'), { recursive: true });
  const hostConfig = 'theme = "Still"\nmodel = "keep-this"\n[compaction]\nmode = "local"\n';
  writeFileSync(join(home, '.octet/config.toml'), hostConfig);
  const piBefore = readFileSync(join(agent, 'settings.json'), 'utf8');
  const output = join(dir, 'octet-pi-compat'), logs = [];
  const result = await configureFromPi({ output, reviewed: true, cwd: dir,
    env: { ...process.env, HOME: home, OCTET_PI_AGENT_DIR: agent }, log: line => logs.push(line) });
  assert.equal(result.themeImport.theme, 'pi-ghostty-dark');
  assert.deepEqual(result.themeImport.themes.map(value => value.name), ['ghostty-dark', 'dark', 'light']);
  const imported = readFileSync(join(output, 'themes/pi-ghostty-dark.toml'), 'utf8');
  assert.match(imported, /accent = "#123456"/);
  assert.match(imported, /composer_border = "#7e57c2"/);
  assert.match(imported, /\[roles\."extension\.pi\.thinkingXhigh"\]\nforeground = "#c792ea"/);
  assert.match(imported, /prompt_wash = false/);
  assert.equal(readFileSync(join(output, 'octet-config.toml'), 'utf8').split('\n').filter(line => !line.startsWith('#') && line).join('\n'), 'theme = "pi-ghostty-dark"');
  const bridge = JSON.parse(readFileSync(join(output, 'bridge.json'), 'utf8'));
  assert.equal(bridge.pi_theme.name, 'ghostty-dark');
  assert.equal(bridge.pi_theme.path, join(output, 'themes/pi-ghostty-dark.json'));
  assert.ok(existsSync(join(output, 'themes/pi-dark.toml')));
  assert.ok(existsSync(join(output, 'themes/pi-light.toml')));
  assert.ok(!existsSync(join(output, 'themes/dark.toml')), 'native built-ins remain reserved/selectable');
  assert.ok(!existsSync(join(output, 'themes/pi-disabled.toml')));
  assert.ok(logs.some(line => /collision.*ghostty-dark/.test(line)));
  assert.ok(logs.some(line => line.includes('--theme-dir') && line.includes('--theme pi-ghostty-dark')));
  assert.equal(readFileSync(join(home, '.octet/config.toml'), 'utf8'), hostConfig);
  assert.equal(readFileSync(join(agent, 'settings.json'), 'utf8'), piBefore);
  assert.ok(result.registrations.hooks.includes('resources_discover'), 'imports reserve the real native resource consumer without a factory hook');
  const peer = launch(t, [join(dir, 'factory.ts')], { cwd: dir, config: join(output, 'bridge.json') });
  peer.metadata.hooks = result.registrations.hooks;
  await peer.init(['resource_paths_v1', 'session_entries']); await peer.start();
  const reply = await peer.request('hook/run', { hook: 'resources_discover', payload: { cwd: dir, reason: 'startup' }, context: peer.context() }).response;
  assert.deepEqual(reply.result, { resource_paths: { skill_paths: [], prompt_paths: [],
    theme_paths: result.themeImport.themes.map(theme => theme.nativePath), default_theme: result.themeImport.selected.nativePath } });
  await peer.close();
});

test('import refuses existing staged config without overwrite before running a factory', async t => {
  const dir = temporary(t), output = join(dir, 'octet-pi-compat');
  mkdirSync(output); writeFileSync(join(output, 'octet-config.toml'), '# user-owned\n');
  await assert.rejects(configureFromPi({ output, reviewed: true, cwd: dir, env: { HOME: dir, OCTET_PI_AGENT_DIR: '/absent' } }), /octet-config.toml exists; use --overwrite/);
  assert.equal(readFileSync(join(output, 'octet-config.toml'), 'utf8'), '# user-owned\n');
});

test('theme plan is data-only, first-name-wins and uses deterministic safe nonreserved selectors', async t => {
  const { planThemeImport } = await import('../lib/theme-import.mjs');
  const dir = temporary(t);
  const spaced = theme(dir, 'spaces', palette('My theme'));
  const native = theme(dir, 'native', palette('Cards'));
  const plan = planThemeImport({ paths: [spaced, native], selection: 'My theme', output: join(dir, 'output') });
  assert.match(plan.theme, /^pi-My-theme-[0-9a-f]{8}$/);
  assert.equal(plan.themes[1].selector, 'pi-Cards');
  assert.ok(!existsSync(join(dir, 'output')), 'planning does not write output or host settings');
  assert.equal(planThemeImport({ paths: [spaced], selection: 'My theme', output: join(dir, 'output') }).theme, plan.theme);
});

test('invalid, symlinked and oversized palettes are diagnosed without selecting a fabricated fallback', async t => {
  const { planThemeImport } = await import('../lib/theme-import.mjs');
  const dir = temporary(t);
  const good = theme(dir, 'good', palette('good'));
  const link = join(dir, 'link.json'); symlinkSync(good, link);
  const big = join(dir, 'big.json'); writeFileSync(big, Buffer.alloc(262145));
  const bad = theme(dir, 'bad', { name: 'bad', colors: {} });
  const plan = planThemeImport({ paths: [link, big, bad, good], selection: 'bad', output: join(dir, 'output') });
  assert.equal(plan.theme, undefined);
  assert.deepEqual(plan.themes.map(value => value.name), ['good']);
  assert.equal(plan.diagnostics.length, 4);
  assert.ok(plan.diagnostics.some(message => /selected Pi theme.*bad.*not imported/.test(message)));
});

test('system/automatic Pi themes are explicit limitations, not silently mapped to octet dark', async t => {
  const { planThemeImport } = await import('../lib/theme-import.mjs');
  const dir = temporary(t), path = theme(dir, 'dark', palette('dark'));
  for (const selection of [undefined, 'system', 'light/dark']) {
    const plan = planThemeImport({ paths: [path], selection, output: join(dir, 'output') });
    assert.equal(plan.theme, undefined);
    assert.ok(plan.diagnostics.some(message => /terminal|automatic/.test(message)));
  }
});

test('re-import advertises only current files and never overwrites unrelated files or follows output symlinks', async t => {
  const { planThemeImport, writeThemeImport } = await import('../lib/theme-import.mjs');
  const dir = temporary(t), output = join(dir, 'output');
  const first = theme(dir, 'first', palette('first')), second = theme(dir, 'second', palette('second'));
  const initial = planThemeImport({ paths: [first, second], selection: 'first', output });
  writeThemeImport(initial);
  assert.throws(() => writeThemeImport(initial), /exists; use --overwrite/);
  writeFileSync(join(output, 'notes.txt'), 'user-owned');
  const current = planThemeImport({ paths: [second], selection: 'second', output });
  writeThemeImport(current, { overwrite: true });
  assert.deepEqual(current.launchArgs, ['--theme-dir', join(output, 'themes/pi-second.toml'), '--theme', 'pi-second']);
  assert.ok(existsSync(join(output, 'themes/pi-first.toml')), 'old output is not deleted or advertised');
  assert.equal(readFileSync(join(output, 'notes.txt'), 'utf8'), 'user-owned');
  const outside = join(dir, 'outside.toml'); writeFileSync(outside, 'do not change');
  rmSync(join(output, 'themes/pi-second.toml')); symlinkSync(outside, join(output, 'themes/pi-second.toml'));
  assert.throws(() => writeThemeImport(current, { overwrite: true }), /non-symlink/);
  assert.equal(readFileSync(outside, 'utf8'), 'do not change');
});

test('an unset Pi thinking setting uses Pi 1.0.2 medium for the composer snapshot', async t => {
  const { planThemeImport } = await import('../lib/theme-import.mjs');
  const dir = temporary(t), value = palette('palette');
  value.colors.thinkingMedium = '#987654'; value.colors.thinkingOff = '#111111';
  const path = theme(dir, 'palette', value);
  const plan = planThemeImport({ paths: [path], selection: 'palette', output: join(dir, 'output') });
  assert.match(plan.themes[0].toml, /composer_border = "#987654"/);
});

test('native composer snapshot preserves indexed/default thinking colors and max fallback', async t => {
  const { planThemeImport } = await import('../lib/theme-import.mjs');
  const dir = temporary(t), value = palette('palette');
  value.colors.thinkingOff = ''; value.colors.thinkingXhigh = 5; delete value.colors.thinkingMax;
  const path = theme(dir, 'palette', value);
  for (const [thinkingLevel, expected] of [['off', 'default'], ['max', 'index:5']]) {
    const plan = planThemeImport({ paths: [path], selection: 'palette', thinkingLevel, output: join(dir, 'output') });
    assert.ok(plan.themes[0].toml.includes(`composer_border = "${expected}"`));
  }
});
