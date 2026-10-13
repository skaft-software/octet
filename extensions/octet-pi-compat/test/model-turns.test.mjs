import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { host, launch, owner, root } from './helper.mjs';

const factory = join(root, 'test/fixtures/model-turns.ts');
// The exact durable entry octet commits for an image tool result on a
// chat-completions model (OpenAiChat lowers the call's media beside its result).
// crates/octet-agent/src/agent/model_turn/media_tests.rs pins the same fixture
// against a real agent run.
const imageResult = JSON.parse(readFileSync(join(root, 'test/fixtures/model-turn-image-entry.json'), 'utf8'));
const user = { id: 'user', parent: null, timestamp_unix_ms: 1000, value: { type: 'message', User: { content: [{ Text: 'question' }] } } };
const assistant = { id: 'assistant', parent: 'user', timestamp_unix_ms: 1100, value: { type: 'message', Assistant: { model: 'native-model', protocol: 'open_ai_chat', content: [{ Text: 'answer' }] } } };
const start = (index = 0) => ({ kind: 'model_turn_start', run_id: 'run:user', turn_index: index, timestamp_ms: 1050 + index });
const end = (entry = assistant, results = [], stopReason = 'end_turn') => ({ kind: 'model_turn_end', run_id: 'run:user', turn_index: 0, timestamp_ms: 1200, assistant_entry: entry,
  assistant_metadata: { assistant_entry_id: entry.id, model: entry.value.Assistant.model, stop_reason: stopReason,
    usage: { input_tokens: 9, output_tokens: 7, cache_read_tokens: 2, cache_write_tokens: 0, cache_write_1h_tokens: 0, reasoning_tokens: 1, total_tokens: 18 },
    cost: { input: 9, output: 6, reasoning: 1, cache_read: 2, cache_write: 0, total: 18, total_picodollars_remainder: 0 } },
  tool_result_entries: results });
function params(peer, payload, mode = 'Synthetic') {
  const entries = payload.kind === 'model_turn_start' ? [user] : [user, payload.assistant_entry, ...payload.tool_result_entries];
  const head = entries.at(-1).id;
  return { hook: payload.kind, payload, context: peer.context({ session_name: mode, session_entries: entries, session_branch: entries, session_leaf_id: head }),
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'model-turn:2', owner, expected_head: head } };
}
const notification = (peer, prefix) => peer.wait(frame => frame.method === 'notification' && frame.params.message.startsWith(prefix));
const expected = { disposition: { action: 'continue' }, context: [], notifications: [], session_operation: { action: 'continue' } };
const issueNotice = frame => frame.method === 'notification' && frame.params.title === '[Extension issues]';
const committedUsage = { input: 9, output: 7, cacheRead: 2, cacheWrite: 0, totalTokens: 18, reasoning: 1,
  cost: { input: 9 / 1e6, output: 7 / 1e6, cacheRead: 2 / 1e6, cacheWrite: 0, total: 18 / 1e6 } };

test('per-model hooks carry real iteration and entry timestamps; whole-run notifications stay separate', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries', 'lifecycle_events']); await peer.start();
  assert.deepEqual(peer.metadata.hooks, ['before_prompt', 'model_turn_end', 'model_turn_start', 'session_end', 'session_start']);
  assert.deepEqual(peer.metadata.events, ['agent_end', 'agent_start', 'turn_end', 'turn_start']);
  for (let index = 0; index < 2; index++) {
    assert.deepEqual((await peer.request('hook/run', params(peer, start(index))).response).result, expected);
    assert.equal((await notification(peer, 'model-start:')).params.message, `model-start:${index}:${1050 + index}`);
    const payload = end(); payload.turn_index = index;
    assert.deepEqual((await peer.request('hook/run', params(peer, payload)).response).result, expected);
    const value = JSON.parse((await notification(peer, 'model-end:')).params.message.slice('model-end:'.length));
    assert.deepEqual(value, { index, message: { role: 'assistant', model: 'native-model', api: 'openai-completions', content: [{ type: 'text', text: 'answer' }], timestamp: 1100, usage: committedUsage, stopReason: 'stop' }, tools: [] });
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
  const peer = launch(t, [factory]); await peer.init(['session_entries']); await peer.start();
  const entry = structuredClone(assistant);
  entry.value.Assistant.content.push(...['one', 'two'].map(id => ({ ToolCall: { id, name: `tool-${id}`, arguments_json: '{}' } })));
  const result = { id: 'results', parent: entry.id, timestamp_unix_ms: 1150, value: { type: 'message', User: { content: ['one', 'two'].map(id => ({ ToolResult: { tool_call_id: id, content: [{ Text: `result-${id}` }], is_error: id === 'two' } })) } } };
  assert.deepEqual((await peer.request('hook/run', params(peer, end(entry, [result], 'tool_use'))).response).result, expected);
  const value = JSON.parse((await notification(peer, 'model-end:')).params.message.slice('model-end:'.length));
  assert.equal(value.message.stopReason, 'toolUse');
  assert.deepEqual(value.message.usage, committedUsage);
  assert.deepEqual(value.tools, ['one', 'two'].map(id => ({ role: 'toolResult', toolCallId: id, toolName: `tool-${id}`, content: [{ type: 'text', text: `result-${id}` }], isError: id === 'two', timestamp: 1150 })));
  const missing = structuredClone(result); missing.value.User.content.pop();
  // Like Pi, a turn observation that cannot be shown is reported, never a failed turn.
  assert.deepEqual((await peer.request('hook/run', params(peer, end(entry, [missing], 'tool_use'))).response).result, expected);
  const issue = await peer.wait(issueNotice);
  assert.ok(issue.params.message.includes(factory));
  assert.match(issue.params.message, /turn_end: octet could not convert/);
  assert.match(issue.params.message, /Next:/);
  await peer.close();
});

test('chat-completions image tool results stay inside one Pi tool result and never skip the observation', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries']); await peer.start();
  const entry = structuredClone(assistant);
  entry.value.Assistant.content.push({ ToolCall: { id: 'frame-call', name: 'frame', arguments_json: '{}' } });
  const result = { id: 'results', parent: entry.id, timestamp_unix_ms: 1150, value: structuredClone(imageResult) };
  assert.deepEqual((await peer.request('hook/run', params(peer, end(entry, [result], 'tool_use'))).response).result, expected);
  const outcome = await Promise.race([notification(peer, 'model-end:'), peer.wait(issueNotice)]);
  assert.equal(outcome.params.title, undefined, `turn_end observation was skipped: ${JSON.stringify(outcome.params)}`);
  const value = JSON.parse(outcome.params.message.slice('model-end:'.length));
  assert.equal(value.message.stopReason, 'toolUse');
  // Pi 1.0.2 keeps tool-result images inside the ToolResultMessage content; its
  // openai-completions provider is the one that splits them onto the wire.
  assert.deepEqual(value.tools, [{ role: 'toolResult', toolCallId: 'frame-call', toolName: 'frame',
    content: [{ type: 'text', text: 'frame rendered' },
      { type: 'image', data: 'iVBORw0KGgpwaS1kb29tLWZyYW1l', mimeType: 'image/png' }],
    isError: false, timestamp: 1150 }]);
  assert.equal(peer.seen.some(issueNotice), false);
  await peer.close();
});

test('a mounted fullscreen extension surface stays up across image tool-result turns', async t => {
  const peer = launch(t, [join(root, 'test/fixtures/core.ts'), factory]);
  await peer.init(); // remote_ui and session_entries are in the offered default set
  const command = peer.command('surface');
  const open = await peer.wait(frame => frame.method === 'ui/open');
  assert.equal(open.params.placement, 'fullscreen');
  assert.ok((await command.response).result);
  const entry = structuredClone(assistant);
  entry.value.Assistant.content.push({ ToolCall: { id: 'frame-call', name: 'frame', arguments_json: '{}' } });
  const result = { id: 'results', parent: entry.id, timestamp_unix_ms: 1150, value: structuredClone(imageResult) };
  for (const [index, results, stop] of [[0, [result], 'tool_use'], [1, [], 'end_turn']]) {
    assert.deepEqual((await peer.request('hook/run', params(peer, start(index))).response).result, expected);
    const payload = end(entry, results, stop); payload.turn_index = index;
    assert.deepEqual((await peer.request('hook/run', params(peer, payload)).response).result, expected);
  }
  // The surface is not a turn observation: no turn may close it, and its
  // component must still render afterwards.
  assert.equal(peer.seen.filter(frame => frame.method === 'ui/close').length, 0);
  peer.notify('context/updated', { resource_owner: owner, host: { ...host, session_name: 'After Turns' } });
  await peer.wait(frame => frame.method === 'ui/frame' && frame.params.lines.some(line => line.includes('After Turns')));
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'q', kind: 'press', modifiers: [] });
  await peer.wait(frame => frame.method === 'ui/close');
  await peer.wait(frame => frame.method === 'notification' && frame.params.message === 'done:completed');
  await peer.close();
});

test('model-end preserves the Pi tool details already stored in the durable native result', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries']); await peer.start();
  const entry = structuredClone(assistant);
  entry.value.Assistant.content.push({ ToolCall: { id: 'one', name: 'tool-one', arguments_json: '{}' } });
  for (const details of [{ source: 'actual-tool', nested: [1, false, 'é'] }, null]) {
    const result = { id: 'result', parent: entry.id, timestamp_unix_ms: 1150,
      value: { type: 'message', User: { content: [{ ToolResult: { tool_call_id: 'one', content: [{ Text: 'real result' }], is_error: false } }] } },
      metadata: { tool_output: { metadata: { pi_details: details } } } };
    assert.deepEqual((await peer.request('hook/run', params(peer, end(entry, [result], 'tool_use'))).response).result, expected);
    const value = JSON.parse((await notification(peer, 'model-end:')).params.message.slice('model-end:'.length));
    assert.deepEqual(value.tools[0].details, details);
  }
  await peer.close();
});

test('model-end synchronous private append awaits the actual leaf receipt before hook success', async t => {
  const peer = launch(t, [factory], { hold: ['session/append_entry'] }); await peer.init(['session_entries']); await peer.start();
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

test('turn observations ignore results and report problems once per extension/event; a missing leaf consumer is refused', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries']); await peer.start();
  // Pi's emit ignores an observation handler's return value.
  assert.deepEqual((await peer.request('hook/run', params(peer, start(), 'veto')).response).result, expected);
  // Only explicitly absent native accounting makes this field unavailable.
  const unavailable = { ...end(), assistant_metadata: null };
  assert.deepEqual((await peer.request('hook/run', params(peer, unavailable, 'usage')).response).result, expected);
  const skipped = await peer.wait(issueNotice);
  assert.ok(skipped.params.message.includes(factory));
  assert.match(skipped.params.message, /turn_end callback.*API or event field.*not support/);
  const opaque = structuredClone(assistant); opaque.value.Assistant.content.push({ Reasoning: { text: 'reason', state: { secret: 'opaque' } } });
  assert.deepEqual((await peer.request('hook/run', params(peer, end(opaque))).response).result, expected);
  assert.deepEqual((await peer.request('hook/run', params(peer, unavailable, 'usage')).response).result, expected);
  assert.equal(peer.seen.filter(issueNotice).length, 1, 'the same extension/event was already reported');
  const noLeaf = params(peer, start()); delete noLeaf.session_leaf;
  assert.match((await peer.request('hook/run', noLeaf).response).error.message, /actual native session_leaf consumer required/);
  await peer.close();
});

test('cancelled model hook cannot publish a late successful turn reply', async t => {
  const peer = launch(t, [factory], { hold: ['confirmation/request'] }); await peer.init(['session_entries']); await peer.start();
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
