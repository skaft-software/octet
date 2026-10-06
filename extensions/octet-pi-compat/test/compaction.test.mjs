import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { fileURLToPath } from 'node:url';
import { Runtime } from '../lib/runtime.mjs';
import { Transport } from '../lib/transport.mjs';
import { createAPI, createContext } from '../lib/api.mjs';
import { requestCompaction, retireCompactions, settleCompactions } from '../lib/compaction.mjs';
import { rpcError } from '../lib/errors.mjs';
import { host, owner } from './helper.mjs';

const features = ['session_control_v1', 'session_compaction_v1', 'session_entries'];
const committed = { entry_id: 'actual-compaction', summary: 'Actual durable summary', first_kept: 'actual-kept' };
const tick = () => new Promise(resolve => setImmediate(resolve));
const context = (binding = owner) => ({ workspace: '/host/workspace', resource_owner: binding, host });

// Real Runtime.receive/hostCall/settlement and real Transport request/cancellation.
// Only the physical writer and host receipts are synthetic. No grace sleeps,
// production timeout changes, native executable or provider are involved.
async function setup(t, { offered = features, hold = () => false } = {}) {
  const frames = [], timeline = [], held = [], diagnostics = [], losses = [];
  let runtime, transport, next = 1;
  transport = new Transport({ output: new EventEmitter(), write(line, done) {
    const frame = JSON.parse(line); frames.push(frame); timeline.push(['write', frame]);
    const finish = error => { timeline.push([error ? 'write-failed' : 'written', frame]); done(error); };
    if (hold(frame)) held.push({ frame, finish }); else finish();
    if (frame.method === 'composer/get') queueMicrotask(() => transport.response({ id: frame.id, result: { text: 'native draft' } }));
  } }, { local: true, onMessage: message => runtime.receive(message), onLost: error => losses.push(error) });
  const settle = transport.settleParent.bind(transport);
  transport.settleParent = id => { timeline.push(['settle', id]); settle(id); };
  runtime = new Runtime({ extensions: [fileURLToPath(new URL('./fixtures/compaction.ts', import.meta.url))] }, transport);
  runtime.backgroundError = error => diagnostics.push(error);
  t.after(() => {
    runtime.stopping = true; retireCompactions(runtime); runtime.uninstallChildren();
    transport.fail(new Error('test transport closed'));
  });
  await runtime.load();
  // configure.mjs reserves all public hooks for real-host factory activation.
  runtime.events.set('session_compact', []);
  const metadata = runtime.metadata();
  await runtime.receive({ jsonrpc: '2.0', id: next++, method: 'initialize', params: {
    api_version: '0.4', workspace: '/host/workspace', host, extension: { name: 'test' },
    contributes: { commands: metadata.commands.map(command => command.name), tools: [], hooks: metadata.hooks, tool_renderers: [] },
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: offered, limits: { max_concurrent_requests: 8 } },
  } });
  assert.ok(frames[0].result, JSON.stringify(frames[0]));
  const events = {};
  for (const topic of ['return', 'captured', 'complete', 'error', 'barrier', 'callback-barrier']) {
    events[topic] = [];
    runtime.bus.facade(0).on(`compact:${topic}`, value => events[topic].push({ value, store: runtime.scope.getStore() }));
  }
  let callbackId = 1000;
  async function terminal(frame, response) {
    if (frame.method === 'session/compact' && frame.params.callback && transport.children.has(frame.id)) {
      const id = callbackId++;
      const outcome = response.error ? { error: response.error.message } : { result: response.result };
      await runtime.receive({ id, method: 'hook/run', params: {
        hook: 'session_compact', context: context(frame.params.resource_owner),
        session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: id, operation_id: `callback:${id}`, owner: frame.params.resource_owner, expected_head: 'actual-compaction' },
        payload: { kind: 'compaction_callback', parent_request_id: frame.params.parent_request_id, ...outcome },
      } });
    }
    transport.response({ id: frame.id, ...response });
  }
  function command(mode = '', binding = owner, id = next++) {
    const done = runtime.receive({ jsonrpc: '2.0', id, method: 'command/execute', params: { name: 'compact_test', arguments: [mode], context: context(binding) } });
    return { id, done };
  }
  const start = binding => runtime.receive({ id: callbackId++, method: 'hook/run', params: { hook: 'session_start', payload: { binding }, context: context(binding) } });
  return { runtime, transport, frames, timeline, held, diagnostics, losses, events, command, start,
    compacts: () => frames.filter(frame => frame.method === 'session/compact'),
    reply: id => frames.find(frame => frame.id === id && !frame.method),
    ack: (frame, result = committed) => terminal(frame, { result }),
    refuse: (frame, message = 'native compaction failed') => terminal(frame, { error: { code: -32002, message } }),
  };
}

test('synchronous undefined; send only after successful physical origin reply AND local parent settlement', async t => {
  const h = await setup(t, { hold: frame => frame.id === 2 && !frame.method });
  const command = h.command(); await tick();
  const origin = h.events.return[0].store;
  assert.equal(h.events.return[0].value.result, undefined);
  assert.equal(origin.pending.size, 0, 'compaction must never enter the live flush set');
  assert.equal(h.held.length, 1); assert.equal(h.compacts().length, 0);
  assert.equal(h.events.complete.length, 0);
  h.held.shift().finish(); await command.done;
  const call = h.compacts()[0]; assert.ok(call);
  const written = h.timeline.findIndex(([kind, frame]) => kind === 'written' && frame.id === command.id);
  const settled = h.timeline.findIndex(([kind, id]) => kind === 'settle' && id === command.id);
  const submitted = h.timeline.findIndex(([kind, frame]) => kind === 'write' && frame === call);
  assert.ok(written < settled && settled < submitted);
  assert.deepEqual(call.params, { parent_request_id: command.id, resource_owner: owner, custom_instructions: 'Keep the actual decisions.\n\tDo not invent metrics.', callback: true });
  assert.equal(h.transport.children.get(call.id).parent, undefined);
  assert.equal(h.events.complete.length, 0, 'submission is not a durable completion receipt');
  h.ack(call); await tick();
  assert.equal(h.events.complete.length, 1); assert.equal(h.events.error.length, 0);
  assert.deepEqual(h.events.complete[0].value, { summary: committed.summary, firstKeptEntryId: committed.first_kept });
  for (const key of ['tokensBefore', 'details', 'entryId']) assert.throws(() => h.events.complete[0].value[key], /unsupported_feature/);
  assert.notEqual(h.events.complete[0].store.id, command.id);
  assert.equal(h.events.complete[0].store.compactionOriginId, command.id);
  assert.notEqual(h.events.complete[0].store.pending, origin.pending);
  assert.equal(origin.pending.size, 0);
  assert.equal(h.diagnostics.length, 0);
});

test('default options remain void and send no invented instructions or metrics', async t => {
  const h = await setup(t); const command = h.command('default'); await command.done;
  assert.equal(h.events.return[0].value.result, undefined);
  assert.deepEqual(h.compacts()[0].params, { parent_request_id: command.id, resource_owner: owner });
  h.ack(h.compacts()[0]); await tick(); assert.equal(h.diagnostics.length, 0);
});

for (const mode of ['fail', 'hold']) test(`${mode === 'fail' ? 'failed' : 'cancelled'} origin discards compaction, with one onError only after settlement`, async t => {
  const h = await setup(t); const command = h.command(mode); await tick();
  if (mode === 'hold') {
    assert.equal(h.compacts().length, 0);
    await h.runtime.receive({ method: '$/cancelRequest', params: { id: command.id } });
    assert.equal(h.events.error.length, 0, 'failure callback must not run in a still-live origin');
    h.events.barrier[0].value.resolve();
  }
  await command.done; await tick();
  assert.equal(h.compacts().length, 0); assert.equal(h.events.complete.length, 0); assert.equal(h.events.error.length, 1);
  assert.equal(h.reply(command.id).error.code, mode === 'hold' ? -32800 : -32603);
  assert.match(h.events.error[0].value.message, mode === 'hold' ? /cancelled/ : /origin failed/);
  assert.equal(h.events.return[0].store.pending.size, 0);
  assert.throws(() => h.events.return[0].value.ctx.compact(), mode === 'hold' ? /cancelled/ : /origin failed/);
  await h.command('capture', owner, command.id).done;
  assert.throws(() => h.events.return[0].value.ctx.compact(), mode === 'hold' ? /cancelled/ : /origin failed/, 'numeric id reuse must not revive the old controller');
  assert.equal(h.diagnostics.length, 0);
});

test('failed terminal write is not permission to submit a retained compaction', async t => {
  const h = await setup(t, { hold: frame => frame.id === 2 && !frame.method });
  const command = h.command(); const failed = assert.rejects(command.done, /transport closed/); await tick();
  h.held.shift().finish(new Error('physical writer failed')); await failed; await tick();
  assert.equal(h.compacts().length, 0); assert.equal(h.events.complete.length, 0); assert.equal(h.events.error.length, 1);
  assert.match(h.events.error[0].value.message, /physical writer failed/);
});

test('cancellation while a success reply is physically pending still discards the compaction', async t => {
  const h = await setup(t, { hold: frame => frame.id === 2 && !frame.method });
  const command = h.command(); await tick();
  await h.runtime.receive({ method: '$/cancelRequest', params: { id: command.id } });
  assert.equal(h.events.error.length, 0); assert.equal(h.compacts().length, 0);
  h.held.shift().finish(); await command.done; await tick();
  assert.equal(h.events.complete.length, 0); assert.equal(h.events.error.length, 1);
  assert.equal(h.events.error[0].value.code, -32800); assert.equal(h.compacts().length, 0);
});

test('valid retained context submits immediately with its original id/owner, not the current caller', async t => {
  const h = await setup(t); const origin = h.command('capture'); await origin.done;
  const caller = h.command('retained'); await caller.done;
  const call = h.compacts()[0]; assert.equal(call.params.parent_request_id, origin.id);
  assert.deepEqual(call.params.resource_owner, owner);
  assert.ok(h.frames.indexOf(call) < h.frames.indexOf(h.reply(caller.id)));
  h.ack(call); await tick(); assert.equal(h.events.complete[0].store.compactionOriginId, origin.id);
});

for (const when of ['queued', 'request']) test(`owner retirement cancels ${when} work; stale callbacks never retarget a replacement`, async t => {
  const h = await setup(t); const command = h.command(when === 'queued' ? 'hold' : ''); await tick();
  if (when === 'request') await command.done;
  const captured = h.events.return[0].value.ctx, call = h.compacts()[0];
  const nextOwner = { ...owner, session_id: 'new-host-owner' };
  await h.start(nextOwner);
  const replacement = h.command('capture', nextOwner); await replacement.done;
  if (when === 'queued') { h.events.barrier[0].value.resolve(); await command.done; }
  if (call) {
    assert.equal(h.frames.filter(frame => frame.method === '$/cancelRequest' && frame.params.id === call.id).length, 1);
    h.ack(call);
  }
  await tick();
  assert.equal(h.events.complete.length, 0); assert.equal(h.events.error.length, 1);
  assert.throws(() => captured.compact(), /not_foreground_owner/);
  assert.equal(h.compacts().length, when === 'queued' ? 0 : 1);
  assert.equal(h.events.error[0].store.state.owner.session_id, owner.session_id);
});

for (const target of ['parent', 'child']) test(`explicit ${target} cancellation after origin settlement refuses late host success`, async t => {
  const h = await setup(t); const command = h.command(); await command.done;
  const call = h.compacts()[0];
  await h.runtime.receive({ method: '$/cancelRequest', params: { id: target === 'parent' ? command.id : call.id } });
  await tick(); h.ack(call); await tick();
  assert.equal(h.events.error.length, 1); assert.equal(h.events.complete.length, 0);
  assert.equal(h.frames.filter(frame => frame.method === '$/cancelRequest' && frame.params.id === call.id).length, 1);
  if (target === 'parent') assert.throws(() => h.events.return[0].value.ctx.compact(), /cancelled/);
});

for (const action of ['shutdown', 'lost']) test(`${action} aborts outstanding compaction before UI cleanup or process exit`, async t => {
  const h = await setup(t); const command = h.command(); await command.done;
  const call = h.compacts()[0], exits = [];
  t.mock.method(process, 'exit', code => exits.push(code));
  t.mock.method(h.runtime.ui, 'shutdown', async () => { assert.equal(h.transport.children.has(call.id), false); });
  if (action === 'shutdown') await h.runtime.shutdown(99);
  else h.runtime.lost(new Error('lost connection'), true);
  await tick(); h.ack(call); await tick();
  assert.equal(h.events.complete.length, 0); assert.equal(h.events.error.length, 1); assert.deepEqual(exits, [0]);
});

for (const mode of ['', 'throw-complete', 'throw-error']) test(`host failure/callback errors are single and non-recursive (${mode || 'onError'})`, async t => {
  const h = await setup(t); const command = h.command(mode); await command.done;
  if (mode === 'throw-complete') h.ack(h.compacts()[0]); else h.refuse(h.compacts()[0]);
  await tick();
  assert.equal(h.events.complete.length, mode === 'throw-complete' ? 1 : 0);
  assert.equal(h.events.error.length, mode === 'throw-complete' ? 0 : 1);
  assert.equal(h.diagnostics.length, mode ? 1 : 0);
  if (mode) assert.match(h.diagnostics[0].message, /callback threw/);
});

test('unhandled native refusal is diagnosed once and never retried', async t => {
  const h = await setup(t); const command = h.command('default'); await command.done;
  h.refuse(h.compacts()[0], 'after-hook failed after durable commit; do not retry'); await tick();
  assert.equal(h.diagnostics.length, 1); assert.match(h.diagnostics[0].message, /do not retry/); assert.equal(h.compacts().length, 1);
});

test('callback pi APIs AND captured ctx setters own a separate retained pending store and cancellation', async t => {
  const h = await setup(t, { offered: [...features, 'composer'] }); const command = h.command('callback-work'); await command.done;
  const origin = h.events.return[0].store; h.ack(h.compacts()[0]); await tick();
  const callbackStore = h.events.complete[0].store;
  assert.equal(callbackStore.pending.size, 2); assert.equal(origin.pending.size, 0);
  const writes = h.frames.filter(frame => ['session/set_name', 'composer/set'].includes(frame.method));
  assert.equal(writes.length, 2);
  for (const call of writes) {
    assert.equal(call.params.parent_request_id, callbackStore.id); assert.deepEqual(call.params.resource_owner, owner);
    assert.equal(h.transport.children.get(call.id).parent, callbackStore.id);
    assert.equal(h.transport.children.get(call.id).signal, callbackStore.controller.signal);
  }
  assert.throws(() => h.events.return[0].value.ctx.compact(), /outstanding compaction for owner/);
  for (const call of writes) h.ack(call, {});
  h.events['callback-barrier'][0].value.resolve(); await tick();
  assert.equal(callbackStore.pending.size, 0); assert.equal(origin.pending.size, 0); assert.equal(h.diagnostics.length, 0);
});

test('awaiting an already tracked failing callback setter does not diagnose the same error twice', async t => {
  const h = await setup(t); await h.command('capture').done;
  const store = { id: 2, state: h.runtime.foreground, factory: 0, controller: new AbortController(), pending: new Set(), errors: [], live: false };
  const pi = createAPI(h.runtime, 0); let errors = 0;
  requestCompaction(h.runtime, store, { onComplete: async () => { await pi.setSessionName('refused'); }, onError: () => { errors++; } });
  h.ack(h.compacts()[0]); await tick();
  h.refuse(h.frames.find(frame => frame.method === 'session/set_name'), 'setter refused'); await tick();
  assert.equal(errors, 0); assert.equal(h.diagnostics.length, 1); assert.match(h.diagnostics[0].message, /setter refused/);
});

test('strict receipt refuses queue-admission ACKs, invented fields, missing identities and wrong types', async t => {
  const h = await setup(t);
  for (const result of [{ accepted: true }, {}, { ...committed, details: {} }, { ...committed, entry_id: '' }, { ...committed, first_kept: null }, { ...committed, summary: 3 }]) {
    const errors = h.events.error.length, command = h.command(); await command.done;
    h.ack(h.compacts().at(-1), result); await tick();
    assert.equal(h.events.error.length, errors + 1); assert.equal(h.events.complete.length, 0);
  }
});

test('strict options, unsupported profiles, numeric origin and recursive hook contexts fail before sending', async t => {
  const h = await setup(t); await h.command('capture').done;
  const store = { id: 2, state: h.runtime.foreground, factory: 0, controller: new AbortController(), pending: new Set(), errors: [], live: false };
  const ctx = createContext(h.runtime, store);
  for (const options of [null, [], Object.create({ unknown: true }), { [Symbol('unknown')]: true }, Object.defineProperty({}, 'unknown', { value: true }), { details: {} }, { tokensBefore: 4 }, { customInstructions: null }, { customInstructions: '\u001b' }, { customInstructions: '\uD800' }, { customInstructions: 'é'.repeat(8193) }, { onComplete: true }, { onError: 'not a callback' }]) assert.throws(() => ctx.compact(options), /invalid_request|unsupported_feature|bounds_exceeded/);
  for (const feature of features.slice(0, 2)) {
    h.runtime.features.delete(feature); assert.throws(() => ctx.compact(), /unsupported_feature/); h.runtime.features.add(feature);
  }
  for (const id of [undefined, '2', -1, 1.5]) assert.throws(() => requestCompaction(h.runtime, { ...store, id }), /numeric parent_request_id/);
  for (const hook of ['session_before_compact', 'session_compact']) {
    assert.throws(() => requestCompaction(h.runtime, { ...store, hook }), /recursive compaction/);
    assert.throws(() => h.runtime.scope.run({ ...store, hook }, () => ctx.compact()), /recursive compaction/);
  }
  assert.equal(h.compacts().length, 0);
});

test('ordinary hook callback copies queue by their shared origin controller, not store object identity', async t => {
  const h = await setup(t, { hold: frame => frame.id === 20 && !frame.method });
  let origin, complete = 0;
  h.runtime.events.set('after_response', [{ factory: 0, handler: (_event, ctx) => {
    origin = h.runtime.scope.getStore();
    assert.notEqual(origin, h.runtime.active.get(20));
    assert.equal(ctx.compact({ onComplete: () => { complete++; } }), undefined);
  } }]);
  await h.start(owner);
  const running = h.runtime.receive({ id: 20, method: 'hook/run', params: { hook: 'after_response', payload: {}, context: context() } });
  await tick(); assert.equal(h.compacts().length, 0); assert.equal(origin.pending.size, 0);
  h.held.shift().finish(); await running;
  assert.equal(h.compacts()[0].params.parent_request_id, 20);
  h.ack(h.compacts()[0]); await tick(); assert.equal(complete, 1);
});

test('retirement during a running completion callback cancels its writes and prevents later captured-ctx/new-owner effects', async t => {
  const h = await setup(t, { offered: [...features, 'composer'] }); const command = h.command('callback-work'); await command.done;
  h.ack(h.compacts()[0]); await tick();
  const callbackStore = h.events.complete[0].store;
  const calls = h.frames.filter(frame => ['session/set_name', 'composer/set'].includes(frame.method));
  const nextOwner = { ...owner, session_id: 'replacement-owner' };
  await h.start(nextOwner);
  await h.command('capture', nextOwner).done;
  assert.equal(callbackStore.controller.signal.aborted, true);
  for (const call of calls) {
    assert.equal(h.transport.children.has(call.id), false);
    assert.equal(h.frames.filter(frame => frame.method === '$/cancelRequest' && frame.params.id === call.id).length, 1);
    h.ack(call, {});
  }
  assert.throws(() => h.runtime.scope.run(callbackStore, () => createAPI(h.runtime, 0).setSessionName('wrong owner')), /not_foreground_owner/);
  assert.throws(() => h.runtime.scope.run(callbackStore, () => h.events.return[0].value.ctx.ui.setEditorText('wrong owner')), /not_foreground_owner/);
  h.events['callback-barrier'][0].value.resolve(); await tick();
  assert.equal(h.events.error.length, 0, 'already-started completion callback must not turn into onError');
  assert.equal(callbackStore.pending.size, 0);
  assert.equal(h.frames.filter(frame => ['session/set_name', 'composer/set'].includes(frame.method)).length, calls.length);
});

test('captured ctx.signal cancels cooperative completion work on retirement and releases bounded slots', async t => {
  const h = await setup(t); const signals = [], settled = [];
  // Nine consecutive real owner bindings must not leak the eight global slots.
  for (let i = 0; i < 9; i++) {
    const binding = { ...owner, session_id: `signal-owner-${i}` };
    await h.start(binding);
    await h.command('capture', binding).done;
    const ctx = h.events.captured.at(-1).value;
    const ordinarySignal = ctx.signal;
    ctx.compact({ onComplete: async () => {
      const signal = ctx.signal; signals.push(signal);
      await new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }));
      settled.push(i);
    } });
    h.ack(h.compacts().at(-1)); await tick();
    assert.equal(signals.at(-1).aborted, false);
    await h.runtime.retire(h.runtime.foreground); await tick();
    assert.equal(signals.at(-1).aborted, true, 'captured ctx must expose the live callback cancellation signal');
    assert.equal(ordinarySignal.aborted, false, 'ordinary origin lifetime is not broadened');
    assert.deepEqual(settled, Array.from({ length: i + 1 }, (_, n) => n));
  }
  assert.equal(h.compacts().length, 9); assert.equal(h.diagnostics.length, 0);
});

test('actual hook/run root carries recursion marker into both before and after compaction callbacks', async t => {
  const h = await setup(t);
  const kept = { id: 'kept', parent: null, value: { type: 'message', User: { content: [{ Text: 'kept' }] } } };
  const grant = { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'operation:2', owner, expected_head: 'kept' };
  for (const [id, hook] of [[20, 'session_before_compact'], [21, 'session_compact']]) {
    h.runtime.events.set(hook, [{ factory: 0, handler: (_event, ctx) => ctx.compact() }]);
    const payload = hook === 'session_before_compact'
      ? { kind: 'before_compact', reason: 'manual', first_kept: 'kept', preparation: { messages: [], turn_prefix_messages: [], previous_summary: null }, branch_entries: [kept], custom_instructions: null }
      : { kind: 'compacted', reason: 'manual', entry: { id: 'compacted', parent: 'kept', value: { type: 'compaction', summary: 'real', first_kept: 'kept' } }, from_extension: true };
    await h.runtime.receive({ id, method: 'hook/run', params: { hook, payload, session_leaf: grant, context: context() } });
    assert.match(h.reply(id).error?.message, /recursive compaction/);
  }
  assert.equal(h.compacts().length, 0);
});

test('one outstanding owner slot and eight global slots, including queued live origins', async t => {
  const h = await setup(t); const origins = [];
  // Synthetic module-bound fixtures: independently model each foreground
  // admission, without claiming native concurrent-owner compaction authority.
  // No wire request is sent before root settlement.
  for (let i = 0; i < 8; i++) {
    const store = { id: 100 + i, state: { alive: true, owner: { ...owner, session_id: `owner-${i}` } }, controller: new AbortController(), pending: new Set(), errors: [], live: true };
    h.runtime.active.set(store.id, store); origins.push(store);
    h.runtime.foreground = store.state;
    assert.equal(requestCompaction(h.runtime, store, { onError() {} }), undefined);
  }
  h.runtime.foreground = origins[0].state;
  assert.throws(() => requestCompaction(h.runtime, origins[0]), /outstanding compaction for owner/);
  const ninth = { ...origins[0], id: 999, controller: new AbortController(), state: { alive: true, owner: { ...owner, session_id: 'owner-9' } } };
  h.runtime.foreground = ninth.state;
  assert.throws(() => requestCompaction(h.runtime, ninth), /bounds_exceeded outstanding compactions/);
  assert.equal(h.compacts().length, 0);
  for (const store of origins) {
    store.live = false; h.runtime.active.delete(store.id); h.transport.settleParent(store.id);
    settleCompactions(h.runtime, store, rpcError(-32800, 'cancelled test origin'));
  }
  await tick(); assert.equal(h.compacts().length, 0);
});

test('completion appends use a new live numeric host request and one-use native leaf successors', async t => {
  const h = await setup(t);
  await h.command('capture').done;
  const ctx = h.events.captured[0].value;
  const pi = createAPI(h.runtime, 0), calls = [];
  let saved;
  // requestSync consumes the local grant before dispatch. Echo only authenticated
  // host correlation fields and its known durable successor, never queue admission.
  t.mock.method(h.transport, 'requestSync', (method, params) => {
    calls.push({ method, params });
    const store = h.runtime.scope.getStore(); saved = store;
    assert.equal(h.runtime.active.get(store.id).controller, store.controller);
    assert.equal(params.parent_request_id, store.id);
    assert.notEqual(store.id, 2);
    const entry_id = `committed-callback-${calls.length}`;
    return { entry_id, head: entry_id, successor: {
      ...params.session_leaf, owner, grant_id: (calls.length === 1 ? 'b' : 'c').repeat(64), expected_head: entry_id,
    } };
  });
  ctx.compact({ onComplete: result => {
    assert.equal(ctx.signal, h.runtime.scope.getStore().controller.signal);
    pi.appendEntry('callback-one', { summary: result.summary });
    pi.appendEntry('callback-two', { kept: result.firstKeptEntryId });
  } });
  const call = h.compacts()[0];
  await h.ack(call); await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[0].params.session_leaf.grant_id, 'a'.repeat(64));
  assert.equal(calls[1].params.session_leaf.grant_id, 'b'.repeat(64));
  assert.equal(h.diagnostics.length, 0);
  assert.throws(() => h.runtime.scope.run(saved, () => pi.appendEntry('late', {})), /active numeric parent_request_id/);
});
