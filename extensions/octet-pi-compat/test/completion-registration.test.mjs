import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { Runtime } from '../lib/runtime.mjs';
import { commandCompletions } from '../lib/completions.mjs';

// A writer receipt may lag bytes already delivered to the host. Exercise the
// actual inbound dispatcher and registration/query code with that receipt held.
test('an immediate post-initialize query waits for registration even before the writer receipt', async t => {
  const order = [], replies = [];
  let releaseReply, acknowledge;
  const writer = new Promise(resolve => { releaseReply = resolve; });
  const admission = new Promise(resolve => { acknowledge = resolve; });
  const runtime = Object.assign(Object.create(Runtime.prototype), {
    active: new Map(), scope: new AsyncLocalStorage(), maxConcurrent: 8,
    features: new Set(['autocomplete']), commands: new Map([['complete', {
      factory: 'fixture', definition: {description: 'Complete'},
    }]]), foreground: null, autocompleteRegistration: null,
    require(feature) { assert.ok(this.features.has(feature)); },
    reportStartupIssues() {}, backgroundError(error) { throw error; },
    async dispatch(message, store) {
      if (message.method === 'initialize') return {};
      return commandCompletions(this, message.params, store);
    },
    transport: {
      send(frame) { replies.push(frame); order.push(`reply:${frame.id}`); return frame.id === 1 ? writer : Promise.resolve(); },
      request(method) { assert.equal(method, 'ui/autocomplete/register'); order.push('register'); return admission; },
      settleParent() {},
    },
  });
  const initialize = runtime.receive({id: 1, method: 'initialize', params: {}});
  let query;
  t.after(async () => { releaseReply(); acknowledge({accepted: true}); await Promise.all([initialize, query]); });
  await new Promise(setImmediate);
  query = runtime.receive({id: 2, method: 'ui/autocomplete/complete', params: {text: '/complete', cursor: 9, revision: 1}});
  await new Promise(setImmediate);
  assert.equal(replies.some(frame => frame.id === 2), false, 'query must await real host admission, not refuse an unassigned promise');
  assert.deepEqual(order, ['reply:1', 'register'], 'registration is queued after the initialize reply without awaiting its writer receipt');
  acknowledge({accepted: true});
  await query;
  assert.deepEqual(replies.find(frame => frame.id === 2).result, {
    prefix: '/complete', items: [{value: '/complete ', label: 'complete', description: 'Complete'}],
  });
  releaseReply(); await initialize;
});
