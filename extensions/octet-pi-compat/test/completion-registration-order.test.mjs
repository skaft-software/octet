import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';

// The host may read the initialize reply and immediately ask for completions.
// Nothing in that reply write is ordered against the host's next inbound
// request, so the chain has to exist before the write can settle. This drives
// the real Runtime with a held initialize reply, which makes the race
// deterministic instead of load-dependent.
function harness() {
  const sent = [];
  let releaseReply;
  const transport = {
    send(frame) {
      sent.push(frame);
      if (frame.id === 1 && frame.result) return new Promise(resolve => { releaseReply = resolve; });
      return Promise.resolve();
    },
    request(method, params) { sent.push({ method, params }); return Promise.resolve({ accepted: true }); },
    notify() { return Promise.resolve(); },
    settleParent() {}, cancel() {}, idle() { return Promise.resolve(); }, close() { return Promise.resolve(); },
    closed: false,
  };
  const runtime = new Runtime({ extensions: [] }, transport);
  runtime.commands.set('complete', { factory: 0, definition: {
    getArgumentCompletions: prefix => [{ value: `${prefix}✓`, label: 'choice' }],
  } });
  return { runtime, sent, release: () => releaseReply(), chain: () => runtime.autocompleteRegistration };
}

const initialize = { jsonrpc: '2.0', id: 1, method: 'initialize', params: {
  api_version: '0.4', workspace: '/tmp/u12', host: {},
  contributes: { tools: [], commands: ['complete'], hooks: ['session_start', 'session_end'] },
  flag_values: [],
  protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'],
    optional_features: ['autocomplete'], limits: { max_concurrent_requests: 8 } },
} };
const query = { jsonrpc: '2.0', id: 2, method: 'ui/autocomplete/complete',
  params: { text: '/complete a', cursor: 11, revision: 1 } };

test('a completion query that arrives with the initialize reply waits for the chain instead of refusing', async t => {
  const h = harness();
  t.after(() => h.runtime.uninstallChildren());
  const settling = h.runtime.receive(initialize);
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(h.sent.some(frame => frame.id === 1 && frame.result), 'initialize reply is written');
  assert.ok(h.chain(), 'the completion chain is armed before the reply write settles');
  const answering = h.runtime.receive(query);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(h.sent.some(frame => frame.id === 2), false, 'the query waits for real admission');
  h.release();
  await settling;
  await answering;
  const reply = h.sent.find(frame => frame.id === 2);
  assert.deepEqual(reply, { jsonrpc: '2.0', id: 2,
    result: { prefix: 'a', items: [{ value: 'a✓', label: 'choice' }] } });
  // The reply precedes the registration request, so the host has already resolved
  // the negotiated `autocomplete` feature when it answers the chain.
  assert.equal(h.sent.indexOf(h.sent.find(frame => frame.method === 'ui/autocomplete/register')),
    h.sent.findIndex(frame => frame.id === 1 && frame.result) + 1);
});

test('a refused or malformed admission fails the waiting query once, not as an unhandled rejection', async t => {
  const h = harness();
  t.after(() => h.runtime.uninstallChildren());
  h.runtime.transport.request = () => Promise.resolve({ accepted: 'yes' });
  const settling = h.runtime.receive(initialize);
  await new Promise(resolve => setImmediate(resolve));
  const answering = h.runtime.receive(query);
  h.release();
  await settling;
  await answering;
  const refusal = h.sent.find(frame => frame.id === 2);
  assert.equal(refusal.error.code, -32602);
  assert.match(refusal.error.message, /autocomplete registration accepted must be boolean/);
});
