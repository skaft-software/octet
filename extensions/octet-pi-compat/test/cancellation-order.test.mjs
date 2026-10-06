import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';

test('cancelled ordered hook holds its queue and active slot until callback settles', async t => {
  const replies = [];
  const transport = { closed: false, send: async message => replies.push(message), settleParent() {}, cancel() {} };
  const runtime = new Runtime({ extensions: [] }, transport);
  runtime.initialized = true;
  t.after(() => runtime.uninstallChildren());

  let release, entered;
  const gate = new Promise(resolve => { release = resolve; });
  const started = new Promise(resolve => { entered = resolve; });
  let live = 0, peak = 0;
  runtime.events.set('after_response', [{ factory: 0, handler: async event => {
    live++;
    peak = Math.max(peak, live);
    if (event.test === 1) { entered(); await gate; }
    live--;
  } }]);
  const owner = { session_id: 'owner', extension_instance_id: 'instance', process_generation: 1 };
  const message = id => ({ jsonrpc: '2.0', id, method: 'hook/run', params: {
    hook: 'after_response', payload: { test: id },
    context: { resource_owner: owner, workspace: '/tmp', host: { has_ui: false } },
  } });

  try {
    const first = runtime.receive(message(1));
    await started;
    runtime.cancel(1);
    const second = runtime.receive(message(2));
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(replies.length, 0, 'cancellation must not send a terminal response while the callback is running');
    assert.equal(live, 1, 'the callback should still be running after cancellation');
    assert.equal(runtime.active.has(1), true, 'the cancelled request must retain its active slot until the callback settles');
    assert.equal(peak, 1, 'ordered callbacks must not overlap after cancellation');

    release();
    await Promise.all([first, second]);
    assert.equal(replies.find(reply => reply.id === 1)?.error?.code, -32800);
    assert.equal(runtime.active.has(1), false, 'the active slot is released after callback settlement');
    assert.equal(peak, 1, 'ordered callbacks must not overlap after cancellation');
  } finally {
    release();
  }
});
