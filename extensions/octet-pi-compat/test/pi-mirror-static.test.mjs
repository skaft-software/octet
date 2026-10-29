// Synthetic review/runtime receipts only. No user factories, auth, live Pi,
// package installs, providers, or native builds are used by this suite.
import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureFromPi, configureMirror, firstPartyTools, routeExtensions } from '../configure.mjs';
import { backgroundTokens, foregroundTokens } from '../lib/theme-palette.mjs';
import { host, launch, root } from './helper.mjs';

function put(path, value) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, value);
  return path;
}
function fixture(t, source = 'export default pi => pi.registerCommand("kept",{handler(){}});') {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-static-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const agent = join(dir, 'agent'), output = join(dir, 'extensions/octet-pi-compat');
  // Use Pi's supported flat autoload suffix; imported .mjs helpers below
  // remain explicit imports, not additional factories.
  const entry = put(join(agent, 'extensions/fixture.js'), source);
  const settings = put(join(agent, 'settings.json'), '{}');
  const env = { ...process.env, HOME: dir, OCTET_PI_AGENT_DIR: agent, PI_CODING_AGENT_DIR: '/not-the-recorded-root' };
  return { dir, agent, output, entry, settings, env, options: { output, cwd: dir, env, reviewed: true, log() {} } };
}
function contributes(metadata) {
  return { tools: metadata.tools.map(tool => tool.name), commands: metadata.commands.map(command => command.name),
    hooks: metadata.hooks, tool_renderers: metadata.tool_renderers, shortcuts: metadata.shortcuts, flags: metadata.flags };
}
function peer(t, f, captured) {
  return launch(t, [], { cwd: f.dir, config: join(f.output, 'bridge.json'), metadata: captured.registrations,
    env: { HOME: join(f.dir, 'relocated'), OCTET_PI_AGENT_DIR: '/missing', PI_CODING_AGENT_DIR: '/wrong' } });
}
async function initialize(p, captured, { trusted = false, grants = [], flagValues = [] } = {}) {
  return p.request('initialize', { api_version: '0.4', workspace: p.context().workspace,
    host: { ...host, has_ui: false, project_trusted: trusted }, contributes: contributes(captured.registrations),
    capabilities: { builtin_tool_overrides: grants }, flag_values: flagValues,
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'],
      optional_features: ['shortcuts', 'resource_paths_v1', 'session_entries', 'runtime_commands', 'dynamic_tools', 'builtin_tool_overrides_v1'],
      limits: { max_concurrent_requests: 8 } } }).response;
}

const tool = name => `pi.registerTool({name:${JSON.stringify(name)},description:'reviewed tool',parameters:{type:'object'},async execute(){return {content:[{type:'text',text:'synthetic'}]};}});`;

test('mirror captures exact shortcut/flag/tool definitions and real manifest startup accepts them', async t => {
  const f = fixture(t, `export default pi => {
    pi.registerShortcut('ctrl+x',{description:'Reviewed shortcut',handler(){}});
    pi.registerFlag('mirror-level',{description:'Reviewed flag',type:'integer',default:2});
    pi.registerCommand('kept',{handler(){if(pi.getFlag('mirror-level')!==7) throw Error('native flag lost');}});
    ${tool('fixture_tool')}
  };`);
  const before = readFileSync(f.settings, 'utf8');
  const captured = await configureMirror(f.options);
  const manifest = readFileSync(join(f.output, 'extension.toml'), 'utf8');
  assert.match(manifest, /tools = \["fixture_tool"\]/);
  assert.match(manifest, /shortcuts = \[\s*\{ name = "pi-shortcut-0", key = "ctrl\+x", description = "Reviewed shortcut" \}/);
  assert.match(manifest, /name = "mirror-level", type = "integer", default = 2/);
  assert.deepEqual(captured.config.extensions, [f.entry]);
  assert.deepEqual(captured.config.registrations.tools, captured.registrations.tools);
  assert.equal(readFileSync(f.settings, 'utf8'), before);
  const p = peer(t, f, captured);
  const initialized = await initialize(p, captured, { flagValues: [{ name: 'mirror-level', value: 7 }] });
  assert.ok(initialized.result, JSON.stringify(initialized));
  assert.deepEqual(initialized.result.shortcuts, captured.registrations.shortcuts);
  assert.ok((await p.command('kept', [], { has_ui: false }).response).result);
  assert.ok((await p.request('shortcut/execute', { name: 'pi-shortcut-0', context: p.context() }).response).result);
  await p.close();
});

test('mirror builtin override requires precisely the reviewed native grant', async t => {
  const f = fixture(t, `export default pi => { ${tool('read')} };`);
  const captured = await configureMirror(f.options);
  assert.match(readFileSync(join(f.output, 'extension.toml'), 'utf8'), /builtin_tool_overrides = \["read"\]/);
  const refused = peer(t, f, captured);
  assert.match((await initialize(refused, captured)).error.message, /builtin_tool_overrides_v1/);
  await refused.close();
  const accepted = peer(t, f, captured);
  assert.ok((await initialize(accepted, captured, { grants: ['read'] })).result);
  assert.ok((await accepted.request('tool/call', { name: 'read', arguments: {}, context: accepted.context() }).response).result);
  await accepted.close();
});

// Fake pinned managed Pi packages exercise only an admitted real-only helper.
function syntheticInstall(agent) {
  put(join(agent, 'install/current-version'), '1.0.2\n');
  for (const name of ['pi-coding-agent', 'pi-ai', 'pi-tui', 'pi-agent-core']) {
    const pkg = join(agent, 'install/releases/1.0.2/node_modules/@earendil-works', name);
    put(join(pkg, 'package.json'), JSON.stringify({ name: '@earendil-works/' + name, version: '1.0.2', type: 'module',
      exports: { '.': './dist/index.js', ...(name === 'pi-ai' ? { './compat': './dist/index.js', './oauth': './dist/index.js', './providers/*': './dist/providers/*.js' } : {}) } }));
    put(join(pkg, 'dist/index.js'), name === 'pi-coding-agent' ? 'export const parseSkillBlock = text => ({text});' : 'export {};');
    put(join(pkg, 'dist/providers/all.js'), 'export {};');
  }
}

test('mirror review captures installed-only helper route; recorded root survives HOME relocation', async t => {
  const f = fixture(t, `import {parseSkillBlock,getAgentDir} from '@earendil-works/pi-coding-agent';
    export default pi => pi.registerCommand('kept',{description:parseSkillBlock(getAgentDir()).text,handler(){}});`);
  syntheticInstall(f.agent);
  const captured = await configureMirror(f.options);
  assert.deepEqual(captured.config.extension_runtimes, { [f.entry]: 'installed' });
  assert.equal(captured.registrations.commands[0].description, f.agent);
  const p = peer(t, f, captured);
  assert.ok((await initialize(p, captured)).result);
  assert.ok((await p.command('kept', [], { has_ui: false }).response).result);
  await p.close();
});

test('new factories and changed entrypoint/imported sources require review before execution', async t => {
  const f = fixture(t, `import {label} from '../label.mjs'; export default pi => pi.registerCommand('kept',{description:label,handler(){}});`);
  const helper = put(join(f.agent, 'label.mjs'), 'export const label="reviewed";');
  const captured = await configureMirror(f.options);
  put(helper, 'export const label="changed";');
  const p = peer(t, f, captured);
  assert.match((await initialize(p, captured)).error.message, /reviewed factory sources changed.*configure again/);
  await p.close();
  put(helper, 'export const label="reviewed";');
  const original = readFileSync(f.entry, 'utf8');
  put(f.entry, original + '\n// changed executable source\n');
  const changedEntry = peer(t, f, captured);
  assert.match((await initialize(changedEntry, captured)).error.message, /reviewed factory sources changed.*configure again/);
  await changedEntry.close();
  put(f.entry, original);
  const marker = join(f.dir, 'executed');
  put(join(f.agent, 'extensions/new.js'), `import {writeFileSync} from 'node:fs'; export default () => writeFileSync(${JSON.stringify(marker)},'no');`);
  const added = peer(t, f, captured);
  assert.match((await initialize(added, captured)).error.message, /factory list changed.*configure again/);
  assert.equal(existsSync(marker), false);
  await added.close();
});

test('environment-dependent static definitions and late registrations cannot expand review', async t => {
  const f = fixture(t, `export default pi => {
    pi.registerCommand('kept',{description:process.env.CHANGE_STATIC==='1'?'changed':'reviewed',handler(){pi.registerShortcut('ctrl+y',{handler(){}});}});
    pi.registerTool({name:'fixture_tool',description:process.env.CHANGE_TOOL==='1'?'changed':'reviewed',parameters:{type:'object'},async execute(){return {content:[]};}});
  };`);
  const captured = await configureMirror(f.options);
  for (const name of ['CHANGE_STATIC', 'CHANGE_TOOL']) {
    const changed = launch(t, [], { cwd: f.dir, config: join(f.output, 'bridge.json'), metadata: captured.registrations, env: { [name]: '1' } });
    assert.match((await initialize(changed, captured)).error.message, /reviewed registration metadata changed/);
    await changed.close();
  }
  const p = peer(t, f, captured);
  assert.ok((await initialize(p, captured)).result);
  assert.match((await p.command('kept', [], { has_ui: false }).response).error.message, /mirror registerShortcut changed static registrations.*configure again/);
  await p.close();
});

test('dynamic tool negotiation does not unfreeze mirror commands, shortcuts, flags or renderer declarations', async t => {
  const f = fixture(t, `export default pi => pi.registerCommand('late',{handler(method){
    if(method==='registerCommand') pi.registerCommand('added',{handler(){}});
    if(method==='registerShortcut') pi.registerShortcut('ctrl+y',{handler(){}});
    if(method==='registerFlag') pi.registerFlag('added',{type:'boolean',default:false});
    if(method==='registerMessageRenderer') pi.registerMessageRenderer('added',()=>({render:()=>[]}));
    if(method==='registerEntryRenderer') pi.registerEntryRenderer('added',()=>({render:()=>[]}));
  }});`);
  const captured = await configureMirror(f.options), p = peer(t, f, captured);
  assert.ok((await initialize(p, captured)).result);
  for (const method of ['registerCommand', 'registerShortcut', 'registerFlag', 'registerMessageRenderer', 'registerEntryRenderer']) {
    assert.match((await p.command('late', [method], { has_ui: false }).response).error.message,
      new RegExp(`mirror ${method} changed static registrations; review and configure again`));
  }
  assert.equal(p.seen.some(frame => ['tools/register', 'shortcut/register'].includes(frame.method)), false);
  await p.close();
});

test('present but disabled first-party manifests do not discard a factory; active collisions refuse the review', async t => {
  const f = fixture(t, `export default pi => { ${tool('web_search')} pi.registerCommand('unrelated',{handler(){}}); };`);
  const nativeRoot = join(f.dir, 'native');
  put(join(nativeRoot, 'octet-web-search/extension.toml'), 'name="octet-web-search"\n[contributes]\ntools=["web_search"]\n');
  // Presence is discoverable data only, not an activation/ownership grant.
  const owners = firstPartyTools([nativeRoot]);
  assert.equal(owners.get('web_search'), 'octet-web-search');
  const disabled = await routeExtensions([f.entry], { cwd: f.dir, env: f.env, installed: false });
  assert.equal(disabled[0].route, 'shims');
  const captured = await configureMirror(f.options);
  assert.deepEqual(captured.registrations.commands.map(command => command.name), ['unrelated']);
  const active = await routeExtensions([f.entry], { cwd: f.dir, env: f.env, activeNativeTools: owners });
  assert.equal(active[0].route, null);
  assert.equal(active[0].disposition, 'review_required');
  assert.match(active[0].error, /active native tool collision.*unrelated registrations were not discarded/);
  const before = readFileSync(join(f.output, 'bridge.json'), 'utf8');
  await assert.rejects(configureMirror({ ...f.options, overwrite: true, activeNativeTools: owners }), /no partial import.*|active native tool collision/);
  assert.equal(readFileSync(join(f.output, 'bridge.json'), 'utf8'), before);
});

test('mirror factory and resource discovery use only authoritative native project trust', async t => {
  const f = fixture(t);
  const projectMarker = join(f.dir, 'project-executed');
  const project = put(join(f.dir, '.pi/extensions/project.js'), `import {writeFileSync} from 'node:fs'; export default pi => {writeFileSync(${JSON.stringify(projectMarker)},'executed');pi.registerCommand('project',{handler(){}});};`);
  const skill = put(join(f.dir, '.pi/skills/private/SKILL.md'), '---\nname: private\ndescription: private\n---\n');
  put(join(f.dir, '.pi/trust.json'), '{"trusted":true}');
  const captured = await configureMirror(f.options);
  assert.deepEqual(captured.config.extensions, [f.entry]);
  assert.equal(existsSync(projectMarker), false);
  const p = peer(t, f, captured);
  assert.ok((await initialize(p, captured)).result);
  const resources = await p.request('hook/run', { hook: 'resources_discover', payload: { cwd: f.dir, reason: 'startup' }, context: p.context({ project_trusted: false }) }).response;
  assert.ok(resources.result, JSON.stringify(resources));
  assert.ok(!resources.result.resource_paths.skill_paths.includes(skill));
  assert.equal(existsSync(projectMarker), false);
  await p.close();
  const trusted = peer(t, f, captured);
  assert.match((await initialize(trusted, captured, { trusted: true })).error.message, /factory list changed.*configure again/);
  assert.equal(existsSync(projectMarker), false, 'newly trusted code still needs review');
  await trusted.close();
  assert.ok(existsSync(project));
  // Even previously reviewed project sources stop when native trust is absent.
  const reviewedProject = await configureMirror({ ...f.options, overwrite: true, projectTrusted: true });
  assert.ok(reviewedProject.config.extensions.includes(project));
  rmSync(projectMarker);
  const untrusted = peer(t, f, reviewedProject);
  const reduced = await initialize(untrusted, reviewedProject);
  assert.ok(reduced.result, JSON.stringify(reduced));
  assert.deepEqual(reduced.result.commands.map(command => command.name), ['kept']);
  assert.equal(existsSync(projectMarker), false);
  await untrusted.close();
});

function palette(name, color) {
  return { name, appearance: 'dark', colors: Object.fromEntries([...foregroundTokens.map(token => [token, color]), ...backgroundTokens.map(token => [token, ''])]) };
}
function nativePalette(name, color) {
  const value = palette(name, color);
  return { ...value, path: null, foregrounds: Object.fromEntries(foregroundTokens.map(token => [token, { color, dim: false }])),
    backgrounds: Object.fromEntries(backgroundTokens.map(token => [token, ''])),
    capabilities: { color: 'truecolor', bold: true, dim: true, italic: true, underline: true, inverse: true, strikethrough: true } };
}

test('dynamic mirror resources forward thinking preference; chosen Pi and authoritative native palettes reach helpers', async t => {
  const f = fixture(t, `import {getAgentDir,getMarkdownTheme} from '@earendil-works/pi-coding-agent';
    export default pi => {
      pi.registerCommand('root',{description:getAgentDir(),handler(){}});
      pi.registerCommand('palette',{description:Buffer.from(getMarkdownTheme().heading('heading')).toString('hex'),handler(_args,ctx){
        if(!ctx.ui.theme.fg('accent','accent').startsWith('\\x1b[38;2;18;52;86m')) throw Error('native palette lost');
        if(!getMarkdownTheme().heading('heading').startsWith('\\x1b[38;2;18;52;86m')) throw Error('native helper palette lost');
      }});
    };`);
  put(f.settings, JSON.stringify({ theme: 'chosen', defaultThinkingLevel: 'high' }));
  const theme = put(join(f.agent, 'themes/chosen.json'), JSON.stringify(palette('Chosen Theme', '#c792ea')));
  const captured = await configureMirror(f.options);
  assert.equal(captured.registrations.commands[0].description, f.agent);
  assert.ok(Buffer.from(captured.registrations.commands[1].description, 'hex').toString().startsWith('\x1b[38;2;199;146;234m'));
  const p = peer(t, f, captured);
  assert.ok((await initialize(p, captured)).result);
  let resources = await p.request('hook/run', { hook: 'resources_discover', payload: { cwd: f.dir, reason: 'startup' }, context: p.context() }).response;
  assert.ok(resources.result, JSON.stringify(resources));
  assert.equal(resources.result.resource_paths.default_theme, theme);
  assert.equal(resources.result.resource_paths.default_thinking_level, 'high');
  put(f.settings, JSON.stringify({ theme: 'chosen', defaultThinkingLevel: 'low' }));
  const extra = put(join(f.agent, 'prompts/after-review.md'), 'dynamic prompt');
  resources = await p.request('hook/run', { hook: 'resources_discover', payload: { cwd: f.dir, reason: 'reload' }, context: p.context() }).response;
  assert.equal(resources.result.resource_paths.default_thinking_level, 'low');
  assert.ok(resources.result.resource_paths.prompt_paths.includes(extra));
  assert.ok((await p.command('palette', [], { has_ui: false, theme: nativePalette('Native Chosen', '#123456') }).response).result);
  assert.ok(!p.seen.some(frame => frame.method === 'theme/select'));
  await p.close();
});

test('snapshot active command collisions refuse rather than dropping unrelated commands', async t => {
  const f = fixture(t);
  const other = put(join(f.agent, 'extensions/other.js'), 'export default pi => {pi.registerCommand("unrelated",{handler(){}});pi.registerCommand("kept",{handler(){}});};');
  syntheticInstall(f.agent);
  const codingAgent = join(f.agent, 'install/releases/1.0.2/node_modules/@earendil-works/pi-coding-agent/dist/index.js');
  put(codingAgent, `export const parseSkillBlock = text => ({text});
    export class SettingsManager {static create(){return {getThemeSetting(){},getDefaultThinkingLevel(){}};}}
    export class DefaultPackageManager {async resolve(){return {extensions:[{path:${JSON.stringify(f.entry)},enabled:true},{path:${JSON.stringify(other)},enabled:true}],themes:[]};}}`);
  await assert.rejects(configureFromPi(f.options), /active registration collision.*|no partial import/);
  assert.equal(existsSync(join(f.output, 'bridge.json')), false);
});

test('legacy unfrozen mirror configuration refuses before executing factories', t => {
  const f = fixture(t, 'export default () => {throw Error("executed legacy factory");};');
  const config = put(join(f.dir, 'legacy.json'), JSON.stringify({ mirror_pi_setup: true, pi_agent_dir: f.agent, extensions: [] }));
  const inspected = spawnSync(process.execPath, [join(root, 'runner.mjs'), '--config', config, '--inspect'], { cwd: f.dir, env: f.env, encoding: 'utf8', timeout: 10000 });
  assert.equal(inspected.status, 1);
  assert.match(inspected.stderr, /mirror static registrations require review/);
  assert.doesNotMatch(inspected.stderr, /executed legacy factory/);
});

// The automatic mirror prepares at every start, before octet's first frame. An
// unchanged setup must reuse its captured catalog instead of importing every
// factory again; any change to a factory's sources reruns the capture.
test('automatic mirror reuses its capture for an unchanged setup and recaptures a changed one', async t => {
  const f = fixture(t);
  const imports = join(f.dir, 'imports.log');
  const factory = command => `import {appendFileSync} from 'node:fs'; appendFileSync(${JSON.stringify(imports)}, 'x');
    export default pi => pi.registerCommand(${JSON.stringify(command)},{handler(){}});`;
  put(f.entry, factory('kept'));
  const cacheDir = join(f.dir, 'mirror-cache');
  const options = { ...f.options, env: { ...f.env, OCTET_PI_COMPAT_CACHE: cacheDir }, cache: true, overwrite: true };
  const count = () => existsSync(imports) ? readFileSync(imports, 'utf8').length : 0;
  const first = await configureMirror(options);
  const imported = count();
  assert.ok(imported > 0, 'a cold start captures the factories');
  const second = await configureMirror(options);
  assert.equal(count(), imported, 'an unchanged setup imports no factory');
  assert.deepEqual(second.registrations, first.registrations);
  // The review fingerprint also records that the output now exists; the
  // catalog itself is identical.
  const { pi_setup_fingerprint: _second, ...secondConfig } = second.config;
  const { pi_setup_fingerprint: _first, ...firstConfig } = first.config;
  assert.deepEqual(secondConfig, firstConfig);
  const p = peer(t, f, second);
  assert.ok((await initialize(p, second)).result);
  assert.ok((await p.command('kept', [], { has_ui: false }).response).result);
  await p.close();
  put(f.entry, factory('changed'));
  const third = await configureMirror(options);
  assert.ok(count() > imported, 'a changed factory is captured again');
  assert.deepEqual(third.registrations.commands.map(command => command.name), ['changed']);
  // An explicit review never takes the shortcut.
  const before = count();
  await configureMirror({ ...options, cache: false });
  assert.ok(count() > before);
});

test('automatic mirror retries a missing dependency without a factory source change', async t => {
  const f = fixture(t);
  const dependent = put(join(f.agent, 'extensions/dependent.js'), `import {label} from 'fixture-dependency';
    export default pi => pi.registerCommand('dependent',{description:label,handler(){}});`);
  const options = { ...f.options, env: { ...f.env, OCTET_PI_COMPAT_CACHE: join(f.dir, 'mirror-cache') }, cache: true, overwrite: true };
  const first = await configureMirror(options);
  assert.deepEqual(first.config.extensions, [f.entry]);
  assert.deepEqual(first.config.mirror_excluded.map(item => item.entry), [dependent]);
  // No factory/settings edit: only the previously absent dependency appears.
  const dependency = join(f.agent, 'node_modules/fixture-dependency');
  put(join(dependency, 'package.json'), JSON.stringify({ name: 'fixture-dependency', type: 'module', exports: './index.js' }));
  put(join(dependency, 'index.js'), 'export const label="dependency ready";');
  const second = await configureMirror(options);
  assert.deepEqual([...second.config.extensions].sort(), [f.entry, dependent].sort());
  assert.equal(second.config.mirror_excluded, undefined);
  assert.equal(second.registrations.commands.find(command => command.name === 'dependent').description, 'dependency ready');
});

test('automatic mirror settles a mixed setup in one capture and names the factory it left out', async t => {
  const f = fixture(t);
  put(join(f.agent, 'extensions/broken.js'), 'export default () => { throw new Error("broken factory"); };');
  const options = { ...f.options, env: { ...f.env, OCTET_PI_COMPAT_CACHE: join(f.dir, 'mirror-cache') }, cache: true };
  const mirrored = await configureMirror(options);
  assert.deepEqual(mirrored.config.extensions, [f.entry]);
  assert.deepEqual(mirrored.config.extension_runtimes, { [f.entry]: 'shims' });
  assert.deepEqual(mirrored.config.mirror_excluded.map(item => item.entry), [realpathSync(join(f.agent, 'extensions/broken.js'))]);
  assert.deepEqual(mirrored.registrations.commands.map(command => command.name), ['kept']);
});
