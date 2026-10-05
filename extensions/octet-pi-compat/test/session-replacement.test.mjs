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
  const peer = launch(t, [path]);
  await peer.init(['session_control_v1', 'session_entries']);
  const reply = peer.command('probe');
  const request = await peer.wait(f => f.method === method);
  const { parent_request_id, resource_owner, ...params } = request.params;
  assert.deepEqual(params, expectParams);
  peer.send({ jsonrpc: '2.0', id: request.id, result: { session_id: 'new-session' } });
  const next = { ...owner, session_id: 'replacement-owner' };
  await peer.request('hook/run', { hook: 'session_start', payload: { binding: next }, context: { workspace: peer.context().workspace, resource_owner: next, host: { ...host, session_id: 'new-session' } } }).response;
  const append = await peer.wait(f => f.method === 'session/append_entry');
  assert.deepEqual(append.params.resource_owner, next);
  assert.equal(append.params.parent_request_id, parent_request_id);
  assert.equal(append.params.entry_type, 'probe-marker');
  assert.deepEqual(append.params.data, { id: 'new-session' });
  peer.send({ jsonrpc: '2.0', id: append.id, result: { entry_id: 'marker-id' } });
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
