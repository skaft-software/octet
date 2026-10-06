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
    await first;
    assert.equal(replies[0]?.error?.code, -32800);
    assert.equal(live, 1, 'the callback should still be running after cancellation');

    await runtime.receive(message(2));
    assert.equal(peak, 1, 'ordered callbacks must not overlap after cancellation');
  } finally {
    release();
  }
});
