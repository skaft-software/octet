import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureMirror } from '../configure.mjs';
import { launch } from './helper.mjs';

function put(path, source) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, source);
  return path;
}
function fixture(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-cold-start-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const agent = join(dir, 'agent'), output = join(dir, 'extensions/octet-pi-compat');
  put(join(agent, 'settings.json'), '{}');
  const log = join(dir, 'setups.log'), cache = join(dir, 'cache');
  const env = { ...process.env, HOME: dir, OCTET_PI_AGENT_DIR: agent, OCTET_PI_COMPAT_CACHE: cache };
  return { dir, agent, output, log, env, options: { output, cwd: dir, env, reviewed: true, cache: true, log() {} } };
}
function factory(f, name, { imports = '', setup = '', handler = '', description = '"clean"' } = {}) {
  return put(join(f.agent, 'extensions', `${name}.js`), `import {appendFileSync} from 'node:fs';
    ${imports}
    export default async pi => {
      appendFileSync(${JSON.stringify(f.log)}, ${JSON.stringify(name + '\n')});
      ${setup}
      pi.registerCommand(${JSON.stringify(name)}, {description:${description},async handler(){${handler}}});
    };`);
}
function setups(f) { return existsSync(f.log) ? readFileSync(f.log, 'utf8').trim().split('\n') : []; }
function install(agent) {
  put(join(agent, 'install/current-version'), '1.0.2\n');
  for (const name of ['pi-coding-agent', 'pi-ai', 'pi-tui', 'pi-agent-core']) {
    const pkg = join(agent, 'install/releases/1.0.2/node_modules/@earendil-works', name);
    put(join(pkg, 'package.json'), JSON.stringify({ name: '@earendil-works/' + name, version: '1.0.2', type: 'module',
      exports: { '.': './dist/index.js', ...(name === 'pi-ai' ? { './compat': './dist/index.js', './oauth': './dist/index.js', './providers/*': './dist/providers/*.js' } : {}) } }));
    put(join(pkg, 'dist/index.js'), name === 'pi-coding-agent'
      ? 'export const formatReadLineRange = () => "installed range";'
      : 'export {};');
    put(join(pkg, 'dist/providers/all.js'), 'export {};');
  }
}

test('one-factory cold automatic mirror executes setup once before live activation', async t => {
  const f = fixture(t), entry = factory(f, 'single');
  const captured = await configureMirror(f.options);
  assert.deepEqual(setups(f), ['single']);
  assert.deepEqual(captured.config.extension_runtimes, { [entry]: 'shims' });
  const cached = await configureMirror({ ...f.options, overwrite: true });
  assert.deepEqual(setups(f), ['single']);
  assert.deepEqual(cached.registrations, captured.registrations);
  const p = launch(t, [], { cwd: f.dir, env: f.env, config: join(f.output, 'bridge.json'), metadata: captured.registrations });
  await p.init(['resource_paths_v1', 'session_entries', 'runtime_commands', 'dynamic_tools', 'shortcuts']);
  assert.deepEqual(setups(f), ['single', 'single']);
  assert.ok((await p.command('single', [], { has_ui: false }).response).result);
  await p.close();
});

test('cold mixed mirror selects the required installed helper route before executing the setup', async t => {
  const f = fixture(t);
  install(f.agent);
  const first = factory(f, 'a-first');
  const helper = put(join(f.agent, 'read-helper.mjs'), `import {formatReadLineRange as formatRange} from '@earendil-works/pi-coding-agent';
    export const read = () => formatRange(1, 2);`);
  const direct = factory(f, 'b-direct', { imports: `import {read} from '../read-helper.mjs';`,
    handler: `if (read() !== 'installed range') throw Error('wrong helper route');` });
  const last = factory(f, 'c-last');
  const captured = await configureMirror(f.options);
  assert.deepEqual(setups(f), ['a-first', 'b-direct', 'c-last']);
  assert.deepEqual(captured.config.extension_runtimes, { [first]: 'shims', [direct]: 'installed', [last]: 'shims' });
  assert.deepEqual(captured.registrations.commands.map(command => command.name), ['a-first', 'b-direct', 'c-last']);
  assert.ok(captured.config.entrypoint_sources[direct][helper]);
  const cached = await configureMirror({ ...f.options, overwrite: true });
  assert.deepEqual(setups(f), ['a-first', 'b-direct', 'c-last']);
  assert.deepEqual(cached.config.extension_runtimes, captured.config.extension_runtimes);
  const p = launch(t, [], { cwd: f.dir, env: f.env, config: join(f.output, 'bridge.json'), metadata: captured.registrations });
  await p.init(['resource_paths_v1', 'session_entries', 'runtime_commands', 'dynamic_tools', 'shortcuts']);
  assert.deepEqual(setups(f), ['a-first', 'b-direct', 'c-last', 'a-first', 'b-direct', 'c-last']);
  assert.ok((await p.command('b-direct', [], { has_ui: false }).response).result);
  await p.close();
});

test('a preselected installed route retries unavailable Pi modules on the next launch', async t => {
  const f = fixture(t);
  install(f.agent);
  const module = join(f.agent, 'install/releases/1.0.2/node_modules/@earendil-works/pi-coding-agent/dist/index.js');
  const source = readFileSync(module, 'utf8');
  rmSync(module);
  const kept = factory(f, 'a-kept');
  const direct = factory(f, 'b-direct', { imports: `import {formatReadLineRange} from '@earendil-works/pi-coding-agent';`,
    handler: `if (formatReadLineRange(1, 2) !== 'installed range') throw Error('wrong helper route');` });
  const first = await configureMirror(f.options);
  assert.deepEqual(first.config.extensions, [kept]);
  assert.deepEqual(first.config.mirror_excluded.map(item => item.entry), [direct]);
  assert.match(first.config.mirror_excluded[0].reason, /installed_pi_unavailable/);
  assert.deepEqual(setups(f), ['a-kept', 'a-kept']);
  put(module, source);
  const recovered = await configureMirror({ ...f.options, overwrite: true });
  assert.deepEqual(recovered.config.extensions, [kept, direct]);
  assert.equal(recovered.config.mirror_excluded, undefined);
  assert.equal(recovered.config.extension_runtimes[direct], 'installed');
  assert.deepEqual(setups(f), ['a-kept', 'a-kept', 'a-kept', 'b-direct']);
});

test('a child-only tool descriptor stays on the emulated route during cold capture', async t => {
  const f = fixture(t);
  const entry = factory(f, 'descriptor', { imports: `import {createReadTool} from '@earendil-works/pi-coding-agent';`,
    setup: `if (createReadTool().name !== 'read') throw Error('native descriptor lost');` });
  const captured = await configureMirror(f.options);
  assert.deepEqual(captured.config.extension_runtimes, { [entry]: 'shims' });
  assert.deepEqual(setups(f), ['descriptor']);
});

test('a failed single-factory capture is not executed a second time on the same route', async t => {
  const f = fixture(t);
  factory(f, 'broken', { setup: `throw Error('single setup failed');` });
  await assert.rejects(configureMirror(f.options), /none of your Pi extensions loaded/);
  assert.deepEqual(setups(f), ['broken']);
  assert.equal(existsSync(join(f.output, 'bridge.json')), false);
});

test('a stalled single-factory capture is not retried on either route', async t => {
  const f = fixture(t);
  factory(f, 'stalled', { setup: `await new Promise(() => {});` });
  await assert.rejects(configureMirror({ ...f.options, strict: true }), /setup did not finish within 10 s/);
  assert.deepEqual(setups(f), ['stalled']);
  assert.equal(existsSync(join(f.output, 'bridge.json')), false);
});

test('an executed failed factory still requires clean accepted-only recapture', async t => {
  const f = fixture(t);
  const broken = factory(f, 'a-broken', { setup: `globalThis.coldStartContaminated = true;
    pi.registerCommand('ghost', {handler(){}}); throw Error('setup failed after shared mutation');` });
  factory(f, 'b-kept', { description: `globalThis.coldStartContaminated ? 'contaminated' : 'clean'` });
  const captured = await configureMirror(f.options);
  assert.deepEqual(captured.config.mirror_excluded.map(item => item.entry), [broken]);
  assert.deepEqual(captured.registrations.commands.map(command => [command.name, command.description]), [['b-kept', 'clean']]);
  assert.equal(setups(f).filter(name => name === 'b-kept').length, 2);
  const p = launch(t, [], { cwd: f.dir, env: f.env, config: join(f.output, 'bridge.json'), metadata: captured.registrations });
  await p.init(['resource_paths_v1', 'session_entries', 'runtime_commands', 'dynamic_tools', 'shortcuts']);
  assert.ok((await p.command('b-kept', [], { has_ui: false }).response).result);
  await p.close();
});
