import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';

const factory = join(root, 'test/fixtures/model-turns.ts');
const user = { id: 'user', parent: null, timestamp_unix_ms: 1000, value: { type: 'message', User: { content: [{ Text: 'question' }] } } };
const assistant = { id: 'assistant', parent: 'user', timestamp_unix_ms: 1100, value: { type: 'message', Assistant: { model: 'native-model', protocol: 'open_ai_chat', content: [{ Text: 'answer' }] } } };
const start = (index = 0) => ({ kind: 'model_turn_start', run_id: 'run:user', turn_index: index, timestamp_ms: 1050 + index });
const end = (entry = assistant, results = []) => ({ kind: 'model_turn_end', run_id: 'run:user', turn_index: 0, timestamp_ms: 1200, assistant_entry: entry, tool_result_entries: results });
function params(peer, payload, mode = 'Synthetic') {
  const entries = payload.kind === 'model_turn_start' ? [user] : [user, payload.assistant_entry, ...payload.tool_result_entries];
  const head = entries.at(-1).id;
  return { hook: payload.kind, payload, context: peer.context({ session_name: mode, session_entries: entries, session_branch: entries, session_leaf_id: head }),
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'model-turn:2', owner, expected_head: head } };
}
const notification = (peer, prefix) => peer.wait(frame => frame.method === 'notification' && frame.params.message.startsWith(prefix));
const expected = { disposition: { action: 'continue' }, context: [], notifications: [], session_operation: { action: 'continue' } };

test('per-model hooks carry real iteration and entry timestamps; whole-run notifications stay separate', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries', 'lifecycle_events']);
  assert.deepEqual(peer.metadata.hooks, ['model_turn_end', 'model_turn_start']);
  for (let index = 0; index < 2; index++) {
    assert.deepEqual((await peer.request('hook/run', params(peer, start(index))).response).result, expected);
    assert.equal((await notification(peer, 'model-start:')).params.message, `model-start:${index}:${1050 + index}`);
    const payload = end(); payload.turn_index = index;
    assert.deepEqual((await peer.request('hook/run', params(peer, payload)).response).result, expected);
    const value = JSON.parse((await notification(peer, 'model-end:')).params.message.slice('model-end:'.length));
    assert.deepEqual(value, { index, message: { role: 'assistant', model: 'native-model', api: 'openai-completions', content: [{ type: 'text', text: 'answer' }], timestamp: 1100 }, tools: [] });
  }
  peer.notify('turn/started', { resource_owner: owner });
  await notification(peer, 'whole-run-start');
  peer.notify('turn/settled', { resource_owner: owner });
  await notification(peer, 'whole-run-end');
  assert.equal(peer.seen.filter(frame => frame.method === 'notification' && frame.params.message.startsWith('model-start:')).length, 2);
  assert.equal(peer.seen.filter(frame => frame.method === 'notification' && frame.params.message.startsWith('model-end:')).length, 2);
  await peer.close();
});

test('one durable native tool-result batch supplies every Pi tool result without fabricated entry IDs', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries']);
  const entry = structuredClone(assistant);
  entry.value.Assistant.content.push(...['one', 'two'].map(id => ({ ToolCall: { id, name: `tool-${id}`, arguments_json: '{}' } })));
  const result = { id: 'results', parent: entry.id, timestamp_unix_ms: 1150, value: { type: 'message', User: { content: ['one', 'two'].map(id => ({ ToolResult: { tool_call_id: id, content: [{ Text: `result-${id}` }], is_error: id === 'two' } })) } } };
  assert.deepEqual((await peer.request('hook/run', params(peer, end(entry, [result]))).response).result, expected);
  const value = JSON.parse((await notification(peer, 'model-end:')).params.message.slice('model-end:'.length));
  assert.deepEqual(value.tools, ['one', 'two'].map(id => ({ role: 'toolResult', toolCallId: id, toolName: `tool-${id}`, content: [{ type: 'text', text: `result-${id}` }], isError: id === 'two', timestamp: 1150 })));
  const missing = structuredClone(result); missing.value.User.content.pop();
  assert.match((await peer.request('hook/run', params(peer, end(entry, [missing]))).response).error.message, /unsettled tool calls/);
  await peer.close();
});

test('model-end preserves the Pi tool details already stored in the durable native result', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries']);
  const entry = structuredClone(assistant);
  entry.value.Assistant.content.push({ ToolCall: { id: 'one', name: 'tool-one', arguments_json: '{}' } });
  for (const details of [{ source: 'actual-tool', nested: [1, false, 'é'] }, null]) {
    const result = { id: 'result', parent: entry.id, timestamp_unix_ms: 1150,
      value: { type: 'message', User: { content: [{ ToolResult: { tool_call_id: 'one', content: [{ Text: 'real result' }], is_error: false } }] } },
      metadata: { tool_output: { metadata: { pi_details: details } } } };
    assert.deepEqual((await peer.request('hook/run', params(peer, end(entry, [result]))).response).result, expected);
    const value = JSON.parse((await notification(peer, 'model-end:')).params.message.slice('model-end:'.length));
    assert.deepEqual(value.tools[0].details, details);
  }
  await peer.close();
});

test('model-end synchronous private append awaits the actual leaf receipt before hook success', async t => {
  const peer = launch(t, [factory], { hold: ['session/append_entry'] }); await peer.init(['session_entries']);
  const request = peer.request('hook/run', params(peer, end(), 'append'));
  const append = await peer.wait(frame => frame.method === 'session/append_entry');
  assert.deepEqual(append.params.data, { index: 0, text: 'answer' });
  assert.deepEqual(append.params.resource_owner, owner);
  assert.equal(append.params.parent_request_id, request.id);
  assert.equal(peer.seen.some(frame => frame.id === request.id && !frame.method), false);
  peer.send({ jsonrpc: '2.0', id: append.id, result: { entry_id: 'durable-observation', head: 'durable-observation', successor: null } });
  assert.deepEqual((await request.response).result, expected);
  await peer.close();
});

test('turn observations refuse veto, absent native usage, opaque content and missing leaf consumers', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries']);
  assert.match((await peer.request('hook/run', params(peer, start(), 'veto')).response).error.message, /unsupported_feature turn_start result/);
  assert.match((await peer.request('hook/run', params(peer, end(), 'usage')).response).error.message, /model turn message.usage/);
  const opaque = structuredClone(assistant); opaque.value.Assistant.content.push({ Reasoning: { text: 'reason', state: { secret: 'opaque' } } });
  assert.match((await peer.request('hook/run', params(peer, end(opaque))).response).error.message, /opaque reasoning/);
  const noLeaf = params(peer, start()); delete noLeaf.session_leaf;
  assert.match((await peer.request('hook/run', noLeaf).response).error.message, /actual native session_leaf consumer required/);
  await peer.close();
});

test('cancelled model hook cannot publish a late successful turn reply', async t => {
  const peer = launch(t, [factory], { hold: ['confirmation/request'] }); await peer.init(['session_entries']);
  const request = peer.request('hook/run', params(peer, start(), 'wait'));
  const barrier = await peer.wait(frame => frame.method === 'confirmation/request');
  peer.notify('$/cancelRequest', { id: request.id });
  assert.equal((await request.response).error.code, -32800);
  peer.send({ jsonrpc: '2.0', id: barrier.id, result: { confirmed: true } });
  assert.deepEqual((await peer.request('hook/run', params(peer, start(1))).response).result, expected);
  assert.equal(peer.seen.filter(frame => frame.id === request.id && !frame.method).length, 1);
  assert.equal(peer.seen.some(frame => frame.method === 'notification' && frame.params.message === 'model-start:0:1050'), false);
  await peer.close();
});

test('registered per-model hooks refuse initialization without their native session-entry consumer', async t => {
  const peer = launch(t, [factory]);
  const reply = await peer.request('initialize', { api_version: '0.4', contributes: { tools: [], commands: [], hooks: peer.metadata.hooks, tool_renderers: [] },
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: [] } }).response;
  assert.match(reply.error.message, /unsupported_feature session_entries/);
  await peer.close();
});
