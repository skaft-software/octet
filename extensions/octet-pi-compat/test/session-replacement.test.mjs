// Pi 1.0.2 command-context session replacement: newSession / fork / switchSession.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner, host } from './helper.mjs';
import { sessionMethods } from '../lib/session-methods.mjs';
import { sessionReplacement } from '../lib/session-operations.mjs';

async function replacement(t, call, method, expectParams) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-session-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'index.ts');
  await writeFile(path, `export default pi => {
    pi.on('session_start', () => {});
    pi.registerCommand('probe', { handler: async (_, ctx) => {
      const result = await ${call}({ withSession: async fresh => {
        pi.appendEntry('probe-marker', { id: fresh.sessionManager.getSessionId() });
        fresh.ui.setStatus('probe', 'replacement');
        fresh.ui.notify('fresh ' + fresh.sessionManager.getSessionId());
      } });
      let retired = false;
      try { ctx.sessionManager.getSessionId(); } catch { retired = true; }
      if (!retired) throw new Error('captured old ctx revived');
      pi.events.emit('done', result);
    } });
  };`);
  // This test supplies the durable ACK explicitly. The shared helper otherwise
  // auto-ACKs appends, producing a timing-dependent duplicate synchronous reply.
  const peer = launch(t, [path], { hold: ['session/append_entry'] });
  await peer.init(['session_control_v1', 'session_entries']);
  const reply = peer.command('probe');
  const request = await peer.wait(f => f.method === method);
  const { parent_request_id, resource_owner, ...params } = request.params;
  assert.deepEqual(params, expectParams);
  peer.send({ jsonrpc: '2.0', id: request.id, result: { session_id: 'new-session' } });
  const next = { ...owner, session_id: 'replacement-owner' };
  const binding = peer.request('hook/run', { hook: 'session_start', payload: { binding: next }, context: { workspace: peer.context().workspace, resource_owner: next, host: { ...host, session_id: 'new-session' } } }).response;
  const append = await peer.wait(f => f.method === 'session/append_entry');
  assert.deepEqual(append.params.resource_owner, next);
  assert.equal(append.params.parent_request_id, parent_request_id);
  assert.equal(append.params.entry_type, 'probe-marker');
  assert.deepEqual(append.params.data, { id: 'new-session' });
  peer.send({ jsonrpc: '2.0', id: append.id, result: { entry_id: 'marker-id' } });
  // The real host pumps pending session requests while start/command work is
  // live; our peer must service the synchronous append before awaiting both.
  assert.ok(!(await binding).error);
  assert.equal((await peer.wait(f => f.method === 'notification')).params.message, 'fresh new-session');
  const done = await reply.response;
  assert.ok(!done.error, JSON.stringify(done.error));
}

test('newSession: replaces the session and runs withSession with a fresh context', async t => {
  await replacement(t, 'ctx.newSession', 'session/create', {});
});

test('fork: forks at the entry with position and runs withSession', async t => {
  await replacement(t, `(o => ctx.fork('entry-7', { position: 'at', ...o }))`, 'session/fork', { entry_id: 'entry-7', position: 'at' });
});

test('switchSession: switches by session file and runs withSession', async t => {
  await replacement(t, `(o => ctx.switchSession('/sessions/abc.jsonl', o))`, 'session/switch', { session_id: 'abc' });
});

// These receipt unit tests do not qualify native cancellation hooks. The real
// host must supply the cancellation; the adapter must not turn it into success.
function receiptMethods(receipt) {
  const store = { method: 'command/execute', state: { owner }, controller: new AbortController() };
  const bindings = [];
  const runtime = {
    assertOwner: () => {}, require: () => {}, track: p => p,
    hostCall: async () => receipt,
    foregroundFor: async id => { bindings.push(id); return { owner: { ...owner, session_id: 'fresh-owner' } }; },
    scope: { run: (_store, callback) => callback() },
  };
  return { methods: sessionMethods(runtime, store, (_runtime, fresh, replaced) => ({ fresh, replaced })), bindings };
}

test('cancelled replacement never waits for a new owner or invokes withSession', async () => {
  const { methods, bindings } = receiptMethods({ cancelled: true });
  assert.deepEqual(await methods.newSession({ withSession: () => assert.fail('cancelled callback') }), { cancelled: true });
  assert.deepEqual(bindings, []);
});

test('replacement settles against the new binding even without withSession', async () => {
  const { methods, bindings } = receiptMethods({ session_id: 'new-session' });
  assert.deepEqual(await methods.newSession(), { cancelled: false });
  assert.deepEqual(bindings, ['new-session']);
});

test('withSession receives a fresh command context after the host receipt', async () => {
  const { methods, bindings } = receiptMethods({ session_id: 'new-session' });
  let observed;
  await methods.fork('entry-7', { position: 'at', withSession: ctx => { observed = ctx; } });
  assert.equal(observed.replaced, true);
  assert.equal(observed.fresh.state.owner.session_id, 'fresh-owner');
  assert.deepEqual(bindings, ['new-session']);
});

test('malformed cancellation is not accepted as a successful replacement', async () => {
  const { methods } = receiptMethods({ cancelled: 'yes' });
  await assert.rejects(methods.newSession(), /cancelled/);
});

// Adapter-only setup receipts. Native tests prove the real journal and driver.
function setupReceipts() {
  const controller = new AbortController(), calls = [];
  const store = { id: 17, live: true, method: 'command/execute', controller, state: { owner, alive: true, host } };
  const next = { ...owner, session_id: 'setup-owner' };
  const state = { owner: next, alive: true, workspace: '/native-workspace', host: {
    ...host, session_id: 'setup-id', session_file: '/native-sessions/setup-id.jsonl', session_entries: [], session_branch: [], session_leaf_id: null,
    session_header: { id: 'setup-id', cwd: '/native-workspace', timestamp_unix_ms: 1700000000000, parent_session: '/native-parent.jsonl' },
  } };
  const context = () => ({ resource_owner: next, workspace: state.workspace, host: structuredClone(state.host) });
  let count = 0;
  const runtime = {
    namespace: 'octet-pi-compat', active: new Map([[store.id, store]]),
    assertOwner: s => { assert.equal(s.state.alive, true); }, require: () => {}, track: p => p,
    scope: { run: (_store, callback) => callback() },
    foregroundFor: () => assert.fail('setup must bind its native creation receipt'),
    bindReplacement(value, original) { assert.deepEqual(value.resource_owner, next); original.state.alive = false; return { ...original, state }; },
    bind(params, fresh) { assert.deepEqual(params.context.resource_owner, next); fresh.state.host = params.context.host; },
    async hostCall(method, params, parent) {
      calls.push({ method, params, parent: parent.id });
      if (method === 'session/create') return { session_id: 'setup-id', context: context() };
      assert.equal(method, 'session/setup'); assert.deepEqual(params.mutation, { kind: 'complete' });
      calls.push('complete'); return { context: context() };
    },
    transport: { requestSync(method, params) {
      assert.equal(method, 'session/setup'); assert.equal(params.parent_request_id, store.id); assert.deepEqual(params.resource_owner, next);
      calls.push(params.mutation);
      const { entry } = params.mutation;
      const { canonical_message, custom_message, ...record } = entry;
      const id = `native-ack-${++count}`;
      state.host.session_entries.push({ ...record, id, parentId: state.host.session_leaf_id, timestamp: '2026-10-04T00:00:00.000Z' });
      state.host.session_branch = [...state.host.session_entries]; state.host.session_leaf_id = id;
      return { entry_id: id, context: context() };
    } },
  };
  const methods = sessionMethods(runtime, store, (_runtime, fresh) => ({ id: fresh.state.host.session_id }));
  return { methods, calls, state };
}

test('parentSession and awaited setup use native ACK identities before withSession', async () => {
  const { methods, calls, state } = setupReceipts();
  let captured, withCalled = false;
  const result = await methods.newSession({ parentSession: '/native-parent.jsonl', setup: async sm => {
    captured = sm;
    assert.deepEqual(sm.getHeader(), { type: 'session', version: 3, id: 'setup-id', cwd: '/native-workspace', timestamp: '2023-11-14T22:13:20.000Z', parentSession: '/native-parent.jsonl' });
    assert.equal(sm.getLeafId(), null);
    const first = sm.appendMessage({ role: 'user', content: 'seed', timestamp: 123 });
    assert.equal(first, 'native-ack-1'); assert.equal(typeof first, 'string');
    assert.equal(sm.getEntry(first).message.content, 'seed');
    assert.deepEqual(JSON.parse(JSON.stringify(calls[1].entry.canonical_message)), { User: { content: [{ Text: 'seed' }] } });
    const second = sm.appendCustomEntry('seed-state', { restored: true });
    assert.equal(second, 'native-ack-2'); assert.equal(sm.getEntry(second).parentId, first);
    await new Promise(resolve => setImmediate(resolve));
    calls.push('setup-await-finished'); assert.equal(withCalled, false);
  }, withSession: fresh => {
    withCalled = true; assert.equal(fresh.id, 'setup-id');
    assert.throws(() => captured.appendCustomEntry('late', {}), /setup completed/);
    calls.push('with');
  } });
  assert.deepEqual(result, { cancelled: false });
  assert.deepEqual(calls[0].params, { resource_owner: owner, parent_session: '/native-parent.jsonl', setup: true });
  assert.ok(calls.indexOf('setup-await-finished') < calls.indexOf('complete'));
  assert.ok(calls.indexOf('complete') < calls.indexOf('with'));
  assert.equal(state.host.session_entries.length, 2);
});

test('a throwing setup preserves committed ACKs, releases setup, and never invokes withSession', async () => {
  const { methods, calls, state } = setupReceipts();
  const error = new Error('setup failed');
  await assert.rejects(methods.newSession({ setup: sm => {
    sm.appendCustomEntry('committed-before-error', { kept: true }); throw error;
  }, withSession: () => assert.fail('withSession after setup failure') }), cause => cause === error);
  assert.equal(state.host.session_entries[0].customType, 'committed-before-error');
  assert.equal(calls.filter(call => call === 'complete').length, 1);
});

test('cancellation skips setup and invalid setup options are rejected before replacement', async () => {
  const { methods, bindings } = receiptMethods({ cancelled: true });
  assert.deepEqual(await methods.newSession({ parentSession: '/not-created', setup: () => assert.fail('cancelled setup') }), { cancelled: true });
  assert.deepEqual(bindings, []);
  assert.throws(() => methods.newSession({ setup: true }), /setup/);
  assert.throws(() => methods.newSession({ parentSession: 1 }), /parent session/);
});

// Adapter-only qualification of the awaited before-hook contract. Native App
// tests separately verify cancellation leaves persisted state unchanged.
async function beforeReplacement(hook, payload, handlers) {
  const store = { method: 'hook/run', controller: new AbortController(), state: { owner, host } };
  const runtime = {
    metadata: () => ({ hooks: [hook] }), bind: () => {}, assertOwner: () => {},
    queued: (_store, callback) => callback(), scope: { run: (_store, callback) => callback() },
    events: new Map([[hook, handlers.map(handler => ({ factory: 0, handler }))]]), flush: async () => {},
  };
  return sessionReplacement(runtime, { hook, payload, context: { resource_owner: owner } }, store);
}

test('before-switch awaits cancellation and stops subsequent handlers', async () => {
  let called = false;
  const result = await beforeReplacement('session_before_switch', { reason: 'new' }, [async (event, ctx) => {
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual({ ...event }, { type: 'session_before_switch', reason: 'new' });
    assert.equal(ctx.sessionManager.getSessionId(), host.session_id);
    called = true;
    return { cancel: true };
  }, () => assert.fail('ran after cancellation')]);
  assert.equal(called, true);
  assert.equal(result.disposition.action, 'deny');
});

test('before-switch resume exposes the exact host target file', async () => {
  const result = await beforeReplacement('session_before_switch', { reason: 'resume', targetSessionFile: '/sessions/exact.jsonl' }, [event => {
    assert.deepEqual({ ...event }, { type: 'session_before_switch', reason: 'resume', targetSessionFile: '/sessions/exact.jsonl' });
  }]);
  assert.equal(result.disposition.action, 'continue');
});

test('before-fork exposes entry and position and consumes cancellation', async () => {
  const result = await beforeReplacement('session_before_fork', { entryId: 'entry-7', position: 'before' }, [event => {
    assert.deepEqual({ ...event }, { type: 'session_before_fork', entryId: 'entry-7', position: 'before' });
    return { cancel: true, skipConversationRestore: true };
  }]);
  assert.equal(result.disposition.action, 'deny');
});

test('before-replacement rejects nonboolean cancellation', async () => {
  await assert.rejects(beforeReplacement('session_before_switch', { reason: 'new' }, [() => ({ cancel: 'yes' })]), /cancel/);
});
