import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { canonicalToPi, piToCanonical } from '../lib/provider-context.mjs';
import { launch, owner, root } from './helper.mjs';
const fixture = join(root, 'test/fixtures/provider-context.ts');
const grant = { grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'provider-context:4', owner, expected_head: 'old-head' };
const user = text => ({ User: { content: [{ Text: text }] } });
const contextParams = (peer, mode = 'replace') => ({ hook: 'provider_context', session_leaf: grant,
  context: peer.context({ session_name: mode, session_leaf_id: 'old-head', session_entries: [], session_branch: [] }),
  payload: { request: { system: 'real system', messages: [user('original')], tools: [{ name: 'not-replaceable' }] },
    preparation: { resource_owner: owner.session_id, session_id: 'actual-session', head: 'old-head', tool_generation: 7 } } });

for (const mode of ['replace', 'in-place', 'throws']) test(`real process context ${mode}: ordered callbacks project only canonical messages and preserve real system`, async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const reply = await peer.request('hook/run', contextParams(peer, mode)).response;
  assert.deepEqual(reply.result.provider_context, { messages: [user(mode === 'replace' ? 'projected' : mode === 'in-place' ? 'mutated' : 'original'), user('second:real system')], system: 'real system' });
  assert.equal(reply.result.provider_context.tools, undefined);
  if (mode === 'throws') assert.match(peer.stderr(), /ordinary context failure/);
  await peer.close();
});

test('unchanged context contributes no unnecessary canonical rewrite', async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const reply = await peer.request('hook/run', contextParams(peer, 'unchanged')).response;
  assert.equal(reply.result.provider_context, undefined); await peer.close();
});
for (const mode of ['unknown-field', 'unknown-role', 'malformed']) test(`context refuses ${mode} rather than silently dropping fields/roles`, async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const reply = await peer.request('hook/run', contextParams(peer, mode)).response;
  assert.ok(reply.error); assert.equal(reply.result, undefined); await peer.close();
});
for (const mode of ['owner', 'head', 'unbound']) test(`context refuses invalid native preparation ${mode}`, async t => {
  const peer = launch(t, [fixture]); await peer.init(); const params = contextParams(peer);
  if (mode === 'owner') params.payload.preparation.resource_owner = 'not-owner';
  if (mode === 'head') params.payload.preparation.head = 'stale-head';
  if (mode === 'unbound') delete params.session_leaf;
  assert.ok((await peer.request('hook/run', params).response).error); await peer.close();
});

test('context append publishes projection only after synchronous known leaf ACK with exact successor', async t => {
  const peer = launch(t, [fixture], { hold: ['session/append_entry'] }); await peer.init();
  const request = peer.request('hook/run', contextParams(peer, 'append'));
  const call = await peer.wait(frame => frame.method === 'session/append_entry');
  assert.equal(call.params.parent_request_id, request.id); assert.deepEqual(call.params.resource_owner, owner);
  assert.deepEqual(call.params.session_leaf, { grant_id: grant.grant_id, activation_epoch: 4, operation_id: grant.operation_id });
  assert.deepEqual(call.params.data, { before: 'old-head', text: 'durable\ncheckpoint' });
  assert.ok(!peer.seen.some(frame => frame.id === request.id && !frame.method));
  peer.send({ jsonrpc: '2.0', id: call.id, result: { entry_id: 'committed', head: 'committed', successor: { ...grant, grant_id: 'b'.repeat(64), expected_head: 'committed' } } });
  assert.deepEqual((await request.response).result.provider_context.messages, [user('original'), user('known:committed:committed')]);
  await peer.close();
});

test('context cancellation refuses pending callback and cannot emit a late projection', async t => {
  const peer = launch(t, [fixture], { hold: ['confirmation/request'] }); await peer.init();
  const request = peer.request('hook/run', contextParams(peer, 'wait'));
  const call = await peer.wait(frame => frame.method === 'confirmation/request');
  peer.notify('$/cancelRequest', { id: request.id });
  assert.equal((await request.response).error.code, -32800);
  peer.send({ jsonrpc: '2.0', id: call.id, result: { confirmed: true } });
  const barrier = await peer.request('hook/run', contextParams(peer, 'unchanged')).response; assert.ok(barrier.result);
  assert.equal(peer.seen.filter(frame => frame.id === request.id && !frame.method).length, 1);
  await peer.close();
});

test('canonical tool calls/results retain actual IDs, arguments, role order and assistant model/protocol', () => {
  const messages = [user('question'), { Assistant: { model: 'historical-model', protocol: 'open_ai_responses', content: [{ Text: 'text' }, { ToolCall: { id: 'call-1', name: 'read', arguments_json: '{"path":"file"}' } }] } },
    { User: { content: [{ ToolResult: { tool_call_id: 'call-1', content: [{ Text: 'result' }], is_error: false } }] } }];
  const pi = canonicalToPi(messages); assert.equal(pi[1].timestamp, undefined); assert.equal(pi[1].usage, undefined); assert.equal(pi[1].provider, undefined);
  assert.equal(pi[2].toolName, 'read'); assert.deepEqual(piToCanonical(pi), messages);
});
test('opaque continuation/media/unsupported protocol explicitly refuse, never silently lose native parts', () => {
  for (const part of [{ ProviderMetadata: {} }, { Media: {} }, { Reasoning: { text: 'thinking', state: { kind: 'opaque' } } }]) assert.throws(() => canonicalToPi([{ Assistant: { model: 'model', protocol: 'open_ai_responses', content: [part] } }]), /unsupported_feature/);
  assert.throws(() => canonicalToPi([{ Assistant: { model: 'model', protocol: 'unknown', content: [] } }]), /unsupported_feature/);
});

test('waitForIdle waits for the negotiated real command receipt, not a locally resolved Promise', async t => {
  const peer = launch(t, [fixture], { hold: ['session/wait_for_idle'] }); await peer.init(['session_entries', 'session_control_v1']);
  const request = peer.command('idle-proof'); const call = await peer.wait(frame => frame.method === 'session/wait_for_idle');
  assert.deepEqual(call.params, { parent_request_id: request.id, resource_owner: owner });
  assert.ok(!peer.seen.some(frame => frame.method === 'notification' && frame.params.message === 'real idle receipt'));
  peer.send({ jsonrpc: '2.0', id: call.id, result: { session_id: 'actual-session' } });
  assert.ok((await request.response).result); await peer.wait(frame => frame.method === 'notification' && frame.params.message === 'real idle receipt'); await peer.close();
});
