import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { Runtime } from '../lib/runtime.mjs';
import { ExtensionIssues } from '../lib/issues.mjs';
import { commandCompletions } from '../lib/completions.mjs';

// These partial runtimes bypass the constructor to hold the initialize writer
// receipt. `receive()` flushes the issue publisher before every reply, so each
// one provides the same collaborator the constructor wires.
function runtimeStub(fields) {
  const runtime = Object.assign(Object.create(Runtime.prototype), fields);
  runtime.issues = new ExtensionIssues(runtime);
  return runtime;
}

// A writer receipt may lag bytes already delivered to the host. Exercise the
// actual inbound dispatcher and registration/query code with that receipt held.
test('an immediate post-initialize query waits for registration even before the writer receipt', async t => {
  const order = [], replies = [];
  let releaseReply, acknowledge;
  const writer = new Promise(resolve => { releaseReply = resolve; });
  const admission = new Promise(resolve => { acknowledge = resolve; });
  const runtime = runtimeStub({
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
  assert.deepEqual(order, ['reply:1'], 'registration cannot precede the initialize writer receipt');
  releaseReply(); await initialize;
  assert.deepEqual(order, ['reply:1', 'register']);
  assert.equal(replies.some(frame => frame.id === 2), false, 'an armed chain still waits for host admission');
  acknowledge({accepted: true});
  await query;
  assert.deepEqual(replies.find(frame => frame.id === 2).result, {
    prefix: '/complete', items: [{value: '/complete ', label: 'complete', description: 'Complete'}],
  });
});

test('post-initialize dispatch cannot overtake startup issues while the writer receipt is pending', async t => {
  let release;
  const writer = new Promise(resolve => { release = resolve; });
  const order = [];
  const runtime = runtimeStub({
    active: new Map(), scope: new AsyncLocalStorage(), maxConcurrent: 8,
    features: new Set(), commands: new Map(), foreground: null,
    async dispatch(message) { order.push(message.method); return {}; },
    reportStartupIssues() { order.push('issues'); },
    transport: {
      send(frame) { order.push(`reply:${frame.id}`); return frame.id === 1 ? writer : Promise.resolve(); },
      settleParent() {},
    },
  });
  const initialize = runtime.receive({id: 1, method: 'initialize', params: {}});
  let command;
  t.after(async () => { release(); await Promise.all([initialize, command]); });
  await new Promise(setImmediate);
  command = runtime.receive({id: 2, method: 'command/execute', params: {}});
  await new Promise(setImmediate);
  assert.deepEqual(order, ['initialize', 'reply:1']);
  release();
  await Promise.all([initialize, command]);
  assert.deepEqual(order, ['initialize', 'reply:1', 'issues', 'command/execute', 'reply:2']);
});
