import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Runtime } from '../lib/runtime.mjs';
import { SettingsManager } from '../shims/child-sdk.mjs';
import { launch, owner } from './helper.mjs';

function fixture(t, initial = {}) {
  const root = mkdtempSync(join(tmpdir(), 'octet-pi-settings-'));
  const cwd = join(root, 'work'), agentDir = join(root, 'agent'), path = join(agentDir, 'settings.json');
  mkdirSync(cwd); mkdirSync(agentDir); writeFileSync(path, JSON.stringify(initial));
  const calls = [], runtime = new Runtime({ extensions: [], pi_agent_dir: agentDir }, {});
  const store = { id: 1, method: 'hook/run', live: true, controller: new AbortController(), pending: new Set(), errors: [] };
  runtime.bind({ context: { workspace: cwd, resource_owner: owner, host: { project_trusted: false } } }, store, { foreground: true });
  runtime.active.set(store.id, store); runtime.features.add('pi_settings_v1');
  runtime.transport.requestSync = (method, params, options) => {
    calls.push({ method, params, options });
    assert.equal(method, 'pi/settings/read');
    assert.deepEqual(params.resource_owner, owner);
    assert.equal(params.cwd, cwd); assert.equal(params.agent_dir, agentDir);
    assert.equal(params.project_trusted, false);
    return { global: JSON.parse(readFileSync(path, 'utf8')), project: {}, project_trusted: false,
      global_path: path, project_path: join(cwd, '.pi', 'settings.json'), errors: [] };
  };
  runtime.transport.request = async (method, params) => {
    calls.push({ method, params });
    assert.equal(method, 'pi/settings/update');
    assert.deepEqual(params.resource_owner, owner);
    assert.equal(params.scope, 'global');
    const settings = JSON.parse(readFileSync(path, 'utf8'));
    const previous = settings.extensions ?? [];
    if (JSON.stringify(params.extensions) !== JSON.stringify(previous)
        && (params.extensions.length !== previous.length + 1 || params.extensions.at(-1) !== '-builtin:mcp'
          || JSON.stringify(params.extensions.slice(0, -1)) !== JSON.stringify(previous))) throw new Error('unsupported extension selector update');
    writeFileSync(path, JSON.stringify({ ...settings, extensions: params.extensions }));
    return {};
  };
  t.after(() => { runtime.uninstallChildren(); rmSync(root, { recursive: true, force: true }); });
  const create = (options = { projectTrusted: false }, directory = agentDir) => runtime.scope.run(store,
    () => SettingsManager.create(cwd, directory, options));
  return { root, cwd, agentDir, path, calls, runtime, store, create };
}

test('SettingsManager persists native-mediated global extension paths and reload reads committed data', async t => {
  const f = fixture(t, { extensions: ['existing.ts'], theme: 'kept', retry: { enabled: false } });
  const settings = f.create();
  assert.deepEqual(settings.drainErrors(), []);
  const snapshot = settings.getGlobalSettings(); snapshot.theme = 'not a mutation';
  assert.equal(settings.getGlobalSettings().theme, 'kept');
  settings.setExtensionPaths([...settings.getGlobalSettings().extensions, '-builtin:mcp']);
  await settings.flush();
  assert.deepEqual(settings.drainErrors(), []);
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), { extensions: ['existing.ts', '-builtin:mcp'], theme: 'kept', retry: { enabled: false } });
  assert.deepEqual(f.create().getGlobalSettings().extensions, ['existing.ts', '-builtin:mcp']);
  assert.deepEqual(f.calls.map(call => call.method), ['pi/settings/read', 'pi/settings/update', 'pi/settings/read']);
});

test('SettingsManager requires negotiated live native owner and validates configured paths before I/O', t => {
  const f = fixture(t);
  assert.throws(() => SettingsManager.create(f.cwd, f.agentDir), /native|owner|host/);
  f.runtime.features.delete('pi_settings_v1');
  assert.throws(() => f.create(), /pi_settings_v1/);
  f.runtime.features.add('pi_settings_v1');
  assert.throws(() => f.create({ projectTrusted: false }, join(f.root, 'other')), /agent|path|directory/);
  f.store.controller.abort();
  assert.throws(() => f.create(), /abort/i);
  assert.equal(f.calls.length, 0);
});

test('SettingsManager project trust cannot be raised by caller options', t => {
  const f = fixture(t);
  assert.throws(() => f.create({ projectTrusted: true }), /trust/i);
  assert.equal(f.calls.length, 0);
});

test('SettingsManager flush waits native commitment and records storage failures as drainable Errors', async t => {
  const f = fixture(t);
  const settings = f.create();
  let fail;
  f.runtime.transport.request = () => new Promise((_resolve, reject) => { fail = reject; });
  settings.setExtensionPaths(['-builtin:mcp']);
  let finished = false;
  const flushed = settings.flush().then(() => { finished = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(finished, false);
  fail(new Error('isolated storage denied'));
  await flushed;
  const errors = settings.drainErrors();
  assert.equal(errors.length, 1); assert.equal(errors[0].scope, 'global');
  assert.equal(errors[0].path, f.path); assert.equal(errors[0].error.message, 'isolated storage denied');
  assert.deepEqual(settings.drainErrors(), []);
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), {});
});

test('SettingsManager does not write after cancellation or owner retirement', async t => {
  const f = fixture(t);
  const settings = f.create();
  f.store.state.alive = false;
  assert.throws(() => settings.setExtensionPaths(['-builtin:mcp']), /owner|context/);
  assert.equal(f.calls.length, 1);
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), {});
});

test('SettingsManager load errors are reported without overwriting malformed storage', async t => {
  const f = fixture(t);
  f.runtime.transport.requestSync = () => ({ global: {}, project: {}, project_trusted: false,
    global_path: f.path, project_path: join(f.cwd, '.pi', 'settings.json'),
    errors: [{ scope: 'global', path: f.path, message: 'invalid settings JSON' }] });
  const settings = f.create();
  assert.equal(settings.drainErrors()[0].error.message, 'invalid settings JSON');
  settings.setExtensionPaths(['-builtin:mcp']);
  await settings.flush();
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), {});
  assert.equal(f.calls.length, 0);
});

test('SettingsManager keeps unbound child and unknown helper effects refused', t => {
  const f = fixture(t);
  assert.throws(() => new SettingsManager(), /SettingsManager/);
  assert.throws(() => SettingsManager.inMemory(), /SettingsManager.inMemory/);
  const settings = f.create();
  assert.throws(() => settings.setProjectExtensionPaths(['other.ts']), /unsupported|not a function/);
});

test('SettingsManager background creation retains the original live owner after callback settlement', async t => {
  const f = fixture(t);
  f.store.live = false; f.runtime.active.delete(f.store.id);
  const settings = f.create();
  settings.setExtensionPaths(['-builtin:mcp']);
  await settings.flush();
  assert.deepEqual(settings.drainErrors(), []);
  assert.equal(f.calls[0].options.parent, undefined);
  assert.deepEqual(f.calls[1].params.resource_owner, owner);
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), { extensions: ['-builtin:mcp'] });
});

test('SettingsManager preserves concurrent unrelated native fields and reports denied selector mutations', async t => {
  const f = fixture(t, { extensions: ['existing.ts'], theme: 'old' });
  const settings = f.create();
  writeFileSync(f.path, JSON.stringify({ extensions: ['existing.ts'], theme: 'new', untouched: true }));
  settings.setExtensionPaths(['existing.ts', '-builtin:mcp']);
  await settings.flush();
  assert.deepEqual(settings.drainErrors(), []);
  const committed = { extensions: ['existing.ts', '-builtin:mcp'], theme: 'new', untouched: true };
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), committed);
  settings.setExtensionPaths(['other.ts']);
  await settings.flush();
  assert.match(settings.drainErrors()[0].error.message, /unsupported extension selector update/);
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), committed);
});

test('SettingsManager queued writes cannot outlive cancellation', async t => {
  const f = fixture(t);
  const settings = f.create();
  settings.setExtensionPaths(['-builtin:mcp']);
  f.store.controller.abort();
  await settings.flush();
  assert.match(settings.drainErrors()[0].error.message, /abort/i);
  assert.equal(f.calls.length, 1);
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), {});
});

test('SettingsManager captured managers cannot rebind to a newer foreground generation', t => {
  const f = fixture(t);
  const settings = f.create();
  const next = { id: 2, method: 'hook/run', live: true, controller: new AbortController(), pending: new Set(), errors: [] };
  f.runtime.bind({ hook: 'session_start', context: { workspace: f.cwd, resource_owner: { ...owner, process_generation: owner.process_generation + 1 },
    host: { project_trusted: false } } }, next, { foreground: true });
  assert.throws(() => settings.setExtensionPaths(['-builtin:mcp']), /owner|context/);
  assert.equal(f.calls.length, 1);
  assert.deepEqual(JSON.parse(readFileSync(f.path, 'utf8')), {});
});

test('SettingsManager rejects mismatched native bindings and untrusted project content before exposing settings', t => {
  const f = fixture(t);
  const base = { global: {}, project: {}, project_trusted: false, global_path: f.path,
    project_path: join(f.cwd, '.pi', 'settings.json'), errors: [] };
  for (const change of [{ global_path: join(f.root, 'other') }, { project_trusted: true },
    { project: { theme: 'untrusted' } }, { global: [] }, { errors: [{ scope: 'project', path: base.project_path, message: 'not admitted' }] }]) {
    f.runtime.transport.requestSync = () => ({ ...base, ...change });
    assert.throws(() => f.create(), /Pi settings|untrusted project/);
  }
  assert.throws(() => f.create({ projectTrusted: false, arbitrary: true }), /option would not be honored/);
  assert.throws(() => f.runtime.scope.run(f.store, () => SettingsManager.create(join(f.root, 'other'), f.agentDir, { projectTrusted: false })), /path/);
});

for (const cancel of ['parent', 'child', 'both']) {
  test(`SettingsManager real sync transport survives ${cancel} cancellation and refuses late read authority`, async t => {
    const f = fixture(t);
    const entry = join(f.root, 'settings-factory.mjs'), config = join(f.root, 'bridge.json');
    writeFileSync(entry, `import { SettingsManager } from '@earendil-works/pi-coding-agent';
      export default pi => pi.registerCommand('settings', { handler: (_, ctx) => {
        const settings = SettingsManager.create(ctx.cwd, ${JSON.stringify(f.agentDir)}, { projectTrusted: false });
        if (settings.getGlobalSettings().theme !== 'healthy') throw new Error('not a native settings read');
      } });`);
    writeFileSync(config, JSON.stringify({ extensions: [entry], pi_agent_dir: f.agentDir }));
    const peer = launch(t, [entry], { config, cwd: f.cwd });
    const initialized = await peer.init(['pi_settings_v1']);
    assert.ok(initialized.protocol.features.includes('pi_settings_v1'));
    const command = peer.command('settings');
    const read = await peer.wait(frame => frame.method === 'pi/settings/read');
    assert.deepEqual(read.params, { parent_request_id: command.id, resource_owner: owner,
      cwd: f.cwd, agent_dir: f.agentDir, project_trusted: false });
    if (cancel !== 'child') peer.notify('$/cancelRequest', { id: command.id, reason: 'dropped parent' });
    if (cancel !== 'parent') peer.notify('$/cancelRequest', { id: read.id, reason: 'dropped child' });
    assert.equal((await command.response).error?.code, -32800);
    const value = theme => ({ global: { theme }, project: {}, project_trusted: false,
      global_path: f.path, project_path: join(f.cwd, '.pi', 'settings.json'), errors: [] });
    peer.send({ jsonrpc: '2.0', id: read.id, result: value('late') });
    const next = peer.command('settings');
    const healthy = await peer.wait(frame => frame.method === 'pi/settings/read' && frame.params.parent_request_id === next.id);
    assert.notEqual(healthy.id, read.id);
    peer.send({ jsonrpc: '2.0', id: healthy.id, result: value('healthy') });
    assert.ok((await next.response).result);
    assert.equal(peer.child.exitCode, null, peer.stderr());
    assert.doesNotMatch(peer.stderr(), /not a native settings read/);
    await peer.close();
  });
}
