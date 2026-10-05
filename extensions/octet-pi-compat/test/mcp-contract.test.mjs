import test, { afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { mcpAPI, startMcpSession, retireMcpSession, validateMcpServerConfig } from '../lib/mcp.mjs';

import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

function nativeWireFixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'pi-mcp-wire-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const path = join(directory, 'factory.mjs');
  writeFileSync(path, `export default pi => {
    pi.registerMcpServer('wire-proof', {command: '/reviewed/local-server', exposure: 'direct'});
    pi.registerCommand('remove', {handler: () => pi.unregisterMcpServer('wire-proof')});
  };`);
  return launch(t, [path], { hold: ['mcp/replace'] });
}

test('real adapter API wires load-time registration and synchronous removal to owner-scoped native snapshots', async t => {
  const h = nativeWireFixture(t);
  assert.ok(h.metadata.hooks.includes('session_start'));
  await h.init(['mcp_registration_v1']);
  const start = h.request('hook/run', {hook: 'session_start', payload: {binding: owner}, context: h.context()});
  const replace = await h.wait(frame => frame.method === 'mcp/replace');
  assert.equal(replace.params.parent_request_id, start.id);
  assert.deepEqual(replace.params.resource_owner, owner);
  assert.equal(replace.params.servers[0].name, 'wire-proof');
  h.send({jsonrpc: '2.0', id: replace.id, result: {changes: {}, errors: [], shadowed: []}});
  assert.ok((await start.response).result);
  const remove = h.command('remove');
  const withdrawal = await h.wait(frame => frame.method === 'mcp/replace');
  assert.deepEqual(withdrawal.params.servers, []);
  assert.equal(withdrawal.params.parent_request_id, remove.id);
  h.send({jsonrpc: '2.0', id: withdrawal.id, result: {changes: {}, errors: [], shadowed: []}});
  assert.ok((await remove.response).result);
  await h.close();
});

test('real adapter does not claim native startup without the negotiated registration feature', async t => {
  const h = nativeWireFixture(t);
  await h.init([]);
  const result = await h.request('hook/run', {hook: 'session_start', payload: {binding: owner}, context: h.context()}).response;
  assert.equal(result.error.code, -32601);
  assert.ok(!h.seen.some(frame => frame.method === 'mcp/replace'));
  await h.close();
});

const runtimes = [];
afterEach(() => { for (const runtime of runtimes.splice(0)) runtime.uninstallChildren(); });

function fixture({ custom = false } = {}) {
  const calls = [], errors = [], events = [];
  const runtime = new Runtime({ extensions: ['/reviewed/a.mjs', '/reviewed/b.mjs'] }, {});
  runtimes.push(runtime);
  runtime.features.add('mcp_registration_v1');
  runtime.hostCall = async (method, params, store) => {
    calls.push({ method, params: structuredClone(params), store });
    return { changes: { added: [], changed: [], removed: [] }, errors: [], shadowed: [] };
  };
  runtime.backgroundError = error => errors.push(error);
  if (custom) runtime.events.set('mcp_servers_change', [{ factory: 1, handler: event => { events.push(structuredClone(event)); } }]);
  const state = { alive: true, workspace: '/workspace', owner: {
    session_id: 'session', extension_instance_id: 'instance', process_generation: 1,
  }, host: {} };
  const store = { id: 1, live: true, state, controller: new AbortController(), pending: new Set(), errors: [] };
  runtime.foreground = state; runtime.active.set(store.id, store);
  const a = mcpAPI(runtime, 0), b = mcpAPI(runtime, 1);
  const run = work => runtime.scope.run(store, work);
  const flush = () => runtime.flush(store);
  return { runtime, state, store, calls, errors, events, a, b, run, flush };
}
const stdio = command => ({ command, args: ['a b', '$(not a shell)'], exposure: 'direct' });

test('load-time methods are synchronous, validated, cloned and inert', async () => {
  const f = fixture(), config = stdio('/server');
  assert.equal(f.a.registerMcpServer('Docs-1', config), undefined);
  config.args.push('caller mutation');
  const records = f.b.getMcpServers();
  assert.deepEqual(records, [{ name: 'Docs-1', config: stdio('/server'), extensionPath: '/reviewed/a.mjs' }]);
  records[0].config.args.push('getter mutation');
  assert.deepEqual(f.a.getMcpServers(), [{ name: 'Docs-1', config: stdio('/server'), extensionPath: '/reviewed/a.mjs' }]);
  assert.equal(f.calls.length, 0);
  f.runtime.loaded = true;
  await startMcpSession(f.runtime, f.store);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].method, 'mcp/replace');
  assert.deepEqual(f.calls[0].params.resource_owner, f.state.owner);
  assert.equal(f.events.length, 0);
});

test('replacement preserves order; foreign names and namespaces are rejected; foreign removal is inert', () => {
  const f = fixture();
  f.a.registerMcpServer('docs-one', stdio('/one'));
  f.b.registerMcpServer('second', stdio('/second'));
  assert.throws(() => f.b.registerMcpServer('docs-one', stdio('/stolen')), /owned/);
  assert.throws(() => f.b.registerMcpServer('docs_one', stdio('/collision')), /collision/);
  assert.equal(f.b.unregisterMcpServer('docs-one'), undefined);
  f.a.registerMcpServer('docs-one', stdio('/replacement'));
  assert.deepEqual(f.a.getMcpServers().map(server => server.name), ['docs-one', 'second']);
  assert.equal(f.a.getMcpServers()[0].config.command, '/replacement');
  f.a.unregisterMcpServer('docs-one');
  f.a.unregisterMcpServer('missing');
  assert.deepEqual(f.a.getMcpServers().map(server => server.name), ['second']);
});

test('late void mutations publish every snapshot in order through real Runtime.track/flush', async () => {
  const f = fixture(); f.runtime.loaded = true;
  f.run(() => {
    assert.equal(f.a.registerMcpServer('first', stdio('/one')), undefined);
    f.a.registerMcpServer('first', stdio('/replacement'));
    f.b.registerMcpServer('second', stdio('/second'));
    f.a.unregisterMcpServer('first');
    f.a.unregisterMcpServer('missing');
  });
  await f.flush();
  assert.equal(f.calls.length, 4);
  assert.deepEqual(f.calls.map(call => call.params.servers.map(server => server.name)), [
    ['first'], ['first'], ['first', 'second'], ['second'],
  ]);
  assert.equal(f.calls[1].params.servers[0].config.command, '/replacement');
});

test('a Pi MCP consumer receives all records, not a competing native connection', async () => {
  const f = fixture({ custom: true });
  f.a.registerMcpServer('seed', { url: 'https://example.invalid/mcp', oauth: { clientId: 'reviewed' } });
  f.runtime.loaded = true;
  await startMcpSession(f.runtime, f.store);
  assert.equal(f.events.length, 0); // Pi reads load-time registrations at session_start.
  f.run(() => f.a.registerMcpServer('late', { command: '/server' }));
  await f.flush();
  assert.equal(f.calls.length, 0);
  assert.equal(f.events.length, 1);
  assert.equal(f.events[0].type, 'mcp_servers_change');
  assert.deepEqual(f.events[0].servers.map(server => server.name), ['seed', 'late']);
});

test('native refusal is observed at the live boundary, not optimistic connection success', async () => {
  const f = fixture(); f.runtime.loaded = true;
  f.runtime.hostCall = async () => { throw new Error('resident octet-mcp is not enabled'); };
  f.run(() => f.a.registerMcpServer('late', stdio('/server')));
  await assert.rejects(f.flush(), /not enabled/);
  // Pi still stores the registration; connection failure does not erase it.
  assert.equal(f.run(() => f.a.getMcpServers()).length, 1);
});

test('unsupported native configs have redacted extension diagnostics, without erasing public records', async () => {
  const f = fixture(); f.runtime.loaded = true;
  f.runtime.hostCall = async () => ({ changes: {}, shadowed: [], errors: [
    { name: 'late', code: 'unsupported_env_expansion', message: 'MUST_NOT_LOG_THE_SECRET' },
  ] });
  f.run(() => f.a.registerMcpServer('late', { command: '/server', env: { TOKEN: '${SECRET}' } }));
  await f.flush();
  assert.equal(f.errors.length, 1);
  assert.equal(f.errors[0].message, 'MCP server late: unsupported_env_expansion');
  assert.equal(f.run(() => f.a.getMcpServers())[0].config.env.TOKEN, '${SECRET}');
});

test('retired owners cannot mutate or publish queued work; a replacement starts with load-time seeds only', async () => {
  const f = fixture();
  f.a.registerMcpServer('seed', stdio('/seed'));
  f.runtime.loaded = true;
  f.run(() => f.a.registerMcpServer('late', stdio('/late')));
  f.state.alive = false; retireMcpSession(f.state);
  await assert.rejects(f.flush(), /not_foreground_owner/);
  assert.equal(f.calls.length, 0);
  assert.throws(() => f.run(() => f.a.getMcpServers()), /not_foreground_owner/);
  const next = { ...f.state, alive: true, piMcpServers: undefined, piMcpTail: undefined, owner: { ...f.state.owner, session_id: 'replacement' } };
  f.runtime.foreground = next;
  const nextStore = { ...f.store, state: next, errors: [], pending: new Set() };
  assert.deepEqual(f.runtime.scope.run(nextStore, () => f.a.getMcpServers()).map(server => server.name), ['seed']);
});

test('Pi exposure aliases, unknown config fields and HTTP transport inference survive validation', () => {
  const value = validateMcpServerConfig('docs', {
    url: 'https://example.invalid/mcp', type: 'streamable-http', exposure: 'codemode-deferred',
    toolExposure: { 'read_*': 'codemode-deferred', delete: 'hidden' }, extra: { value: 1 },
  });
  assert.equal(value.exposure, 'codemode');
  assert.equal(value.toolExposure['read_*'], 'codemode');
  assert.deepEqual(value.extra, { value: 1 });
});

test('pinned Pi config and OAuth validation rejects the same malformed shapes', () => {
  for (const config of [
    {}, { type: 'sse', url: 'https://example.invalid' }, { command: '/server', args: [1] },
    { command: '/server', env: { TOKEN: 1 } }, { command: '/server', enabled: 0 },
    { command: '/server', exposure: 'all' }, { command: '/server', toolExposure: [] },
    { url: 'file:///secret' }, { url: 'https://example.invalid', headers: { x: 1 } },
    { url: 'http://example.invalid', auth: { provider: 'token' } },
    { url: 'https://example.invalid', oauth: { callbackPort: 65536 } },
    { url: 'https://example.invalid', oauth: { callbackUrl: 'https://localhost/callback' } },
    { url: 'https://example.invalid', oauth: { callbackUrl: 'http://localhost:5555/callback', callbackPort: 4444 } },
    { url: 'https://example.invalid', oauth: { clientRegistration: 'cimd', clientId: 'bad' } },
    { url: 'https://example.invalid', oauth: { clientRegistration: 'cimd', callbackUrl: 'http://[::1]/callback' } },
    { url: 'https://example.invalid', oauth: { authServerMetadataUrl: 'http://example.invalid' } },
  ]) assert.throws(() => validateMcpServerConfig('valid', config));
  assert.throws(() => validateMcpServerConfig('invalid.name', stdio('/server')));
  assert.doesNotThrow(() => validateMcpServerConfig('valid', { url: 'http://localhost/mcp', auth: { provider: 'token' } }));
  assert.doesNotThrow(() => validateMcpServerConfig('valid', { url: 'https://example.invalid', oauth: {
    callbackUrl: 'http://localhost:4444/callback', callbackPort: 4444, clientRegistration: 'cimd',
  } }));
});
