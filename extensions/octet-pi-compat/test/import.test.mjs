import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { configure, configureFromPi, FIRST_PARTY_EXTENSIONS, firstPartyTools, routeExtensions } from '../configure.mjs';
import { inspect, launch, root } from './helper.mjs';

function temporary(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-import-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}
function entry(dir, name, source) {
  const file = join(dir, name + '.ts'); writeFileSync(file, source); return file;
}
// Synthetic managed install: deterministic routing tests, not Pi resolver parity.
function install(dir, entries) {
  const agent = join(dir, 'agent'); mkdirSync(join(agent, 'install'), { recursive: true });
  writeFileSync(join(agent, 'install/current-version'), '1.0.2\n');
  writeFileSync(join(agent, 'settings.json'), JSON.stringify(entries));
  const sources = {
    'pi-coding-agent': `import {readFileSync} from 'node:fs';
export const parseSkillBlock = text => ({text});
export const createReadTool = cwd => ({name:'read',async execute(){return {content:[{type:'text',text:'installed read'}]};}});
export class SettingsManager { static create(cwd, agentDir) { return {cwd, agentDir, getThemeSetting(){}, getDefaultThinkingLevel(){}}; } }
export class DefaultPackageManager { constructor(options) { this.options = options; }
async resolve() { return {extensions:JSON.parse(readFileSync(this.options.agentDir+'/settings.json','utf8')), themes:[]}; } }`,
    'pi-ai': 'export {};', 'pi-tui': 'export {};', 'pi-agent-core': 'export {};',
  };
  for (const [name, source] of Object.entries(sources)) {
    const pkg = join(agent, 'install/releases/1.0.2/node_modules/@earendil-works', name);
    mkdirSync(join(pkg, 'dist/providers'), { recursive: true });
    writeFileSync(join(pkg, 'package.json'), JSON.stringify({ name: '@earendil-works/' + name, version: '1.0.2', type: 'module',
      exports: { '.': './dist/index.js', ...(name === 'pi-ai' ? { './compat': './dist/index.js', './oauth': './dist/index.js', './providers/*': './dist/providers/*.js' } : {}) } }));
    writeFileSync(join(pkg, 'dist/index.js'), source);
    writeFileSync(join(pkg, 'dist/providers/all.js'), 'export {};');
  }
  return agent;
}

test('automatic routing chooses shims, installed Pi, or an explicit skip, preserving cwd/env', async t => {
  const dir = temporary(t);
  const good = entry(dir, 'good', `export default pi => { if (process.cwd() !== ${JSON.stringify(dir)} || process.env.IMPORT_PROBE !== 'yes') throw new Error('cwd/env lost'); pi.registerCommand('good',{handler(){}}); };`);
  const fallback = entry(dir, 'fallback', `import {parseSkillBlock} from '@earendil-works/pi-coding-agent'; export default pi => { parseSkillBlock('test'); pi.registerCommand('fallback',{handler(){}}); };`);
  const broken = entry(dir, 'broken', `export default () => {throw new Error('broken factory');};`);
  const override = entry(dir, 'override', `export default pi => pi.registerTool({name:'read',label:'read',description:'override',parameters:{type:'object'},async execute(){return {content:[]};}});`);
  const unsupportedSchema = entry(dir, 'schema', `export default pi => pi.registerTool({name:'unsafe_schema',description:'unsupported constraint',parameters:{type:'object',properties:{value:{type:'string',pattern:'^[a-z]+$'}}},async execute(){return {content:[]};}});`);
  const agent = install(dir, []);
  const routed = await routeExtensions([good, fallback, broken, override, unsupportedSchema], { cwd: dir, env: { ...process.env, OCTET_PI_AGENT_DIR: agent, IMPORT_PROBE: 'yes' } });
  assert.deepEqual(routed.map(r => r.route), ['shims', 'installed', null, 'shims', null]);
  assert.match(routed[2].error, /broken factory/);
  assert.match(routed[4].error, /parameters.properties.value.pattern.*not supported/);
  await assert.rejects(routeExtensions([], { concurrency: 0 }), /concurrency/);
});

test('deferred direct Pi tool execution routes to installed Pi, but child-only descriptors stay native', async t => {
  const dir = temporary(t);
  const helper = entry(dir, 'read-helper', `import { createReadTool as makeRead } from '@mariozechner/pi-coding-agent';
    export async function read(ctx) { const builtinRead = makeRead(ctx.cwd); const execute = builtinRead.execute; return execute('id', {path:'file'}, undefined, undefined, ctx); }`);
  const direct = entry(dir, 'direct', `import {read} from './read-helper'; export default pi => pi.registerTool({
    name:'wrapped_read',description:'deferred native read',parameters:{type:'object'},execute:(_id,_args,_signal,_update,ctx)=>read(ctx)});`);
  const chained = entry(dir, 'chained', `import {createReadTool} from '@earendil-works/pi-coding-agent'; export default pi => pi.registerCommand('read',{
    handler:(_args,ctx)=>createReadTool(ctx.cwd).execute('id',{path:'file'})});`);
  const descriptor = entry(dir, 'descriptor', `import {createReadTool} from '@earendil-works/pi-coding-agent'; export default pi => {
    const read = createReadTool(); pi.registerCommand('descriptor',{description:read.name,handler(){}}); };`);
  const env = { ...process.env, OCTET_PI_AGENT_DIR: install(dir, []) };
  const routed = await routeExtensions([direct, chained, descriptor], { cwd: dir, env });
  assert.deepEqual(routed.map(r => r.route), ['installed', 'installed', 'shims']);
  const disabled = await routeExtensions([direct], { cwd: dir, env, installed: false });
  assert.equal(disabled[0].route, null);
  assert.match(disabled[0].error, /createReadTool.*installed-Pi fallback/);
  assert.ok(readFileSync(helper, 'utf8').includes('builtinRead.execute'));
});

test('a Pi tool owned by an installed first-party octet extension turns that Pi extension off', async t => {
  const dir = temporary(t);
  const octetExtensions = join(dir, 'octet-extensions');
  for (const [name, tools] of [['octet-web-search', '["web_search", "web_fetch"]'], ['octet-computer-use', '[\n  "computer_use",\n]'], ['not-first-party', '["other_tool"]']]) {
    mkdirSync(join(octetExtensions, name), { recursive: true });
    writeFileSync(join(octetExtensions, name, 'extension.toml'), `name = "${name}"\nversion = "0.1.0"\n\n[contributes]\ntools = ${tools}\n`);
  }
  const owners = firstPartyTools([octetExtensions]);
  assert.deepEqual([...owners].sort(), [['computer_use', 'octet-computer-use'], ['web_fetch', 'octet-web-search'], ['web_search', 'octet-web-search']]);
  const clashing = entry(dir, 'clashing', `export default pi => pi.registerTool({name:'web_search',label:'search',description:'pi search',parameters:{type:'object'},async execute(){return {content:[]};}});`);
  const other = entry(dir, 'other', `export default pi => pi.registerTool({name:'other_tool',label:'other',description:'kept',parameters:{type:'object'},async execute(){return {content:[]};}});`);
  const routed = await routeExtensions([clashing, other], { cwd: dir, firstPartyRoots: [octetExtensions], env: { ...process.env, OCTET_PI_AGENT_DIR: install(dir, []) } });
  assert.deepEqual(routed.map(r => r.route), [null, 'shims']);
  assert.match(routed[0].error, /web_search is provided by the first-party octet extension octet-web-search/);
});

test('the first-party list matches the release catalog, minus the Pi bridge itself', () => {
  const catalog = readFileSync(join(root, '../release-catalog.txt'), 'utf8').split('\n').map(line => line.trim()).filter(line => line && !line.startsWith('#'));
  assert.deepEqual([...FIRST_PARTY_EXTENSIONS].sort(), catalog.filter(name => name !== 'octet-pi-compat').sort());
});

test('from-Pi setup requires review, excludes disabled entries, records routes and install, refuses overwrite', async t => {
  const dir = temporary(t);
  const good = entry(dir, 'good', `export default pi => pi.registerCommand('good',{handler(){}});`);
  const fallback = entry(dir, 'fallback', `import {parseSkillBlock} from '@earendil-works/pi-coding-agent'; export default pi => { parseSkillBlock('test'); pi.registerCommand('fallback',{handler(){}}); };`);
  const later = entry(dir, 'duplicate', `export default pi => {pi.registerCommand('discard',{handler(){}}); pi.registerCommand('good',{handler(){}});};`);
  const agent = install(dir, [{ path: good, enabled: true }, { path: fallback, enabled: true }, { path: later, enabled: true }, { path: '/not/executed.ts', enabled: false }]);
  const output = join(dir, 'octet-pi-compat'), options = { output, cwd: dir, env: { ...process.env, OCTET_PI_AGENT_DIR: agent } };
  await assert.rejects(configureFromPi(options), /--reviewed/);
  const settingsBefore = readFileSync(join(agent, 'settings.json'), 'utf8');
  const logs = [];
  const result = await configureFromPi({ ...options, reviewed: true, log: line => logs.push(line) });
  assert.deepEqual(result.routed.map(r => r.route), ['shims', 'installed', null]);
  assert.deepEqual(result.registrations.commands.map(c => c.name), ['good', 'fallback']);
  assert.ok(logs.some(line => /skipped.*duplicate registration good/.test(line)));
  const config = JSON.parse(readFileSync(join(output, 'bridge.json'), 'utf8'));
  assert.equal(config.pi_agent_dir, agent);
  assert.deepEqual(config.extension_runtimes, { [good]: 'shims', [fallback]: 'installed' });
  assert.equal(readFileSync(join(agent, 'settings.json'), 'utf8'), settingsBefore);
  await assert.rejects(configureFromPi({ ...options, reviewed: true }), /exists; use --overwrite/);
  // Recorded Pi location survives a HOME that has no Pi installation.
  const loaded = spawnSync(process.execPath, [join(root, 'runner.mjs'), '--config', join(output, 'bridge.json'), '--inspect'],
    { encoding: 'utf8', env: { ...process.env, HOME: join(dir, 'other-home'), OCTET_PI_AGENT_DIR: '/missing' } });
  assert.equal(loaded.status, 0, loaded.stderr);
  assert.deepEqual(JSON.parse(loaded.stdout).result.commands.map(c => c.name), ['good', 'fallback']);
});

test('failing factories roll back tools, hooks, commands, bus listeners, MCP, and timers; missing files are isolated', async t => {
  const dir = temporary(t);
  const good = entry(dir, 'good', `export default pi => { pi.registerTool({name:'kept',label:'kept',description:'original',parameters:{type:'object'},async execute(){return {content:[]};}}); pi.registerCommand('good',{handler(){pi.events.emit('topic',{});}}); };`);
  const broken = entry(dir, 'broken', `export default pi => {
pi.registerTool({name:'kept',label:'kept',description:'bad replacement',parameters:{type:'object'},async execute(){return {content:[]};}});
pi.registerCommand('discard',{handler(){}}); pi.on('before_agent_start',()=>{});
pi.events.on('topic',()=>{throw new Error('leaked listener');});
pi.registerMcpServer('discard',{transport:'stdio',command:'not-executed',exposure:'direct'});
setTimeout(()=>console.error('leaked timer'),100);
throw new Error('factory failed'); };`);
  const peer = launch(t, [good, broken, join(dir, 'missing.ts')]);
  assert.deepEqual(peer.metadata.commands.map(c => c.name), ['good']);
  assert.equal(peer.metadata.tools[0].description, 'original');
  assert.ok(!peer.metadata.events.includes('before_agent_start'));
  await peer.init(); await peer.start();
  const notice = await peer.wait(f => f.method === 'notification' && f.params.level === 'warning');
  assert.equal(notice.params.title, '[Extension issues]');
  assert.ok(notice.params.message.includes(broken));
  assert.ok(notice.params.message.includes(join(dir, 'missing.ts')));
  assert.equal((notice.params.message.match(/Next:/g) || []).length, 2);
  assert.ok((await peer.command('good').response).result);
  await new Promise(resolve => setTimeout(resolve, 150));
  assert.doesNotMatch(peer.stderr(), /leaked timer|leaked listener/);
  await peer.close();
});

test('runtime isolation preserves reviewed shortcut IDs and rejects unrelated metadata changes', async t => {
  const dir = temporary(t);
  const first = entry(dir, 'first', `export default pi => {if(process.env.FAIL_FIRST==='1') throw new Error('skip first'); pi.registerShortcut('ctrl+x',{handler(){}});};`);
  const second = entry(dir, 'second', `export default pi => {pi.registerShortcut('ctrl+y',{handler(){}}); pi.registerCommand('good',{description:process.env.CHANGE_SECOND==='1'?'changed':'reviewed',handler(){}});};`);
  const output = join(dir, 'octet-pi-compat');
  const { registrations } = configure({ output, reviewed: true, extensions: [first, second] });
  const config = join(output, 'bridge.json');
  const peer = launch(t, [first, second], { config, env: { FAIL_FIRST: '1' } });
  peer.metadata.hooks = registrations.hooks;
  const initialized = await peer.init(['shortcuts', 'lifecycle_events', 'session_entries']);
  assert.deepEqual(initialized.shortcuts, registrations.shortcuts);
  const skippedShortcut = await peer.request('shortcut/execute', { name: registrations.shortcuts[0].name, context: peer.context() }).response;
  assert.match(skippedShortcut.error.message, /unknown shortcut/);
  const keptShortcut = await peer.request('shortcut/execute', { name: registrations.shortcuts[1].name, context: peer.context() }).response;
  assert.ok(keptShortcut.result);
  await peer.close();
  const changed = launch(t, [first, second], { config, env: { FAIL_FIRST: '1', CHANGE_SECOND: '1' } });
  changed.metadata.hooks = registrations.hooks;
  await assert.rejects(changed.init(['shortcuts', 'lifecycle_events', 'session_entries']), /reviewed registration metadata changed/);
  await changed.close();
});

test('unknown Pi event subscriptions are inert without requiring an unavailable host feature', t => {
  const dir = temporary(t);
  const good = entry(dir, 'unknown', `export default pi => { const off=pi.on('future_pi_event',()=>{throw new Error('not emitted');}); off(); pi.registerCommand('good',{handler(){}}); };`);
  assert.deepEqual(inspect([good]).commands.map(c => c.name), ['good']);
});
