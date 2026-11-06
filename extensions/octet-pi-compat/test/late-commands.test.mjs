import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createAPI } from '../lib/api.mjs';
import { owner } from './helper.mjs';

function harness(t) {
  const calls = [], runtime = new Runtime({ extensions: ['/factory.ts', '/other.ts'] }, {
    requestSync(method, params, options) { calls.push({ method, params, options }); return { commands: params.commands.map(command => command.name) }; },
  });
  runtime.uninstallChildren();
  runtime.features = new Set(['runtime_commands', 'runtime_command_registration_v1']);
  const store = { id: 7, live: true, method: 'hook/run', hook: 'session_start', controller: new AbortController(), pending: new Set(), errors: [] };
  runtime.bind({ context: { resource_owner: owner, workspace: '/work', host: {} } }, store, { foreground: true });
  runtime.active.set(store.id, store);
  const api = createAPI(runtime, 0), other = createAPI(runtime, 1);
  api.registerCommand('existing', { description: 'original', handler() {} });
  runtime.loaded = true;
  t.after(() => { runtime.active.clear(); });
  return { runtime, store, api, other, calls, run: fn => runtime.scope.run(store, fn) };
}

test('session_start publishes late command metadata synchronously before the local handler', t => {
  const h = harness(t), handler = () => 'ok';
  h.runtime.transport.requestSync = (method, params, options) => {
    h.calls.push({ method, params, options });
    if (params.commands[0].name === 'late') assert.equal(h.runtime.commands.has('late'), false);
    else assert.equal(h.runtime.commands.get('existing').definition.description, 'original');
    return { commands: ['existing', 'late'] };
  };
  h.run(() => h.api.registerCommand('late', { handler, prompt: 'ignored' }));
  assert.equal(h.runtime.commands.get('late').definition.handler, handler);
  assert.deepEqual(h.calls[0].params, { parent_request_id: 7, resource_owner: owner, commands: [{ name: 'late', description: 'late' }] });
  assert.equal(h.calls[0].method, 'commands/register');
  assert.equal(h.calls[0].options.parent, 7);
  assert.equal(h.calls[0].options.signal, h.store.controller.signal);
  h.run(() => h.api.registerCommand('existing', { description: 'updated', handler }));
  assert.equal(h.runtime.commands.get('existing').definition.description, 'updated');
});

test('late commands require both negotiated features and never commit on host refusal or malformed ACK', t => {
  const h = harness(t), original = h.runtime.commands.get('existing');
  for (const feature of ['runtime_commands', 'runtime_command_registration_v1']) {
    h.runtime.features.delete(feature);
    assert.throws(() => h.run(() => h.api.registerCommand('late', { handler() {} })), /unsupported_feature/);
    h.runtime.features.add(feature);
  }
  h.runtime.transport.requestSync = () => { throw Error('native refusal'); };
  assert.throws(() => h.run(() => h.api.registerCommand('existing', { description: 'changed', handler() {} })), /native refusal/);
  assert.equal(h.runtime.commands.get('existing'), original);
  h.runtime.transport.requestSync = () => ({ commands: [] });
  assert.throws(() => h.run(() => h.api.registerCommand('late', { handler() {} })), /host did not publish registered command/);
  assert.equal(h.runtime.commands.has('late'), false);
});

test('late command registration fences different factories, retired owners, cancellation and draining', t => {
  const h = harness(t);
  assert.throws(() => h.run(() => h.other.registerCommand('existing', { handler() {} })), /belongs to another factory/);
  h.store.state.alive = false;
  assert.throws(() => h.run(() => h.api.registerCommand('late', { handler() {} })), /not_foreground_owner/);
  h.store.state.alive = true; h.runtime.stopping = true;
  assert.throws(() => h.run(() => h.api.registerCommand('late', { handler() {} })), /host is draining/);
  h.runtime.stopping = false; h.store.controller.abort(Error('cancelled origin'));
  assert.throws(() => h.run(() => h.api.registerCommand('late', { handler() {} })), /cancelled origin/);
  assert.deepEqual(h.calls, []);
});

test('late registration from a retained callback carries owner but never a stale live parent', t => {
  const h = harness(t);
  h.store.live = false; h.runtime.active.clear();
  h.run(() => h.api.registerCommand('late', { handler() {} }));
  assert.equal(h.calls[0].options.parent, undefined);
  assert.deepEqual(h.calls[0].params.resource_owner, owner);
});

test('new command feature cannot negotiate without runtime_commands', async t => {
  const h = harness(t);
  await assert.rejects(h.runtime.initialize({ api_version: '0.4', protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: ['runtime_command_registration_v1'] } }, h.store), /runtime_commands/);
});
