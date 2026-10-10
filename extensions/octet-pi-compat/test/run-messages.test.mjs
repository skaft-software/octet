import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { launch, owner, root } from './helper.mjs';
import { handleRunMessage } from '../lib/run-messages.mjs';

const plain = join(root, 'test/fixtures/run-messages.ts');
const turns = join(root, 'test/fixtures/run-messages-turns.ts');
const user = { id: 'user', parent: null, timestamp_unix_ms: 1000, value: { type: 'message', User: { content: [{ Text: 'question' }] } } };
const assistant = { id: 'assistant', parent: 'user', timestamp_unix_ms: 1100, value: { type: 'message', Assistant: { model: 'native-model', protocol: 'open_ai_chat', content: [{ Text: 'answer' }] } } };
const start = index => ({ kind: 'model_turn_start', run_id: 'run:user', turn_index: index, timestamp_ms: 1050 + index });
const end = (entry = assistant, results = [], index = 0, stopReason = 'end_turn') => ({ kind: 'model_turn_end', run_id: 'run:user', turn_index: index, timestamp_ms: 1200 + index, assistant_entry: entry,
  assistant_metadata: { assistant_entry_id: entry.id, model: entry.value.Assistant.model, stop_reason: stopReason,
    usage: { input_tokens: 9, output_tokens: 7, cache_read_tokens: 2, cache_write_tokens: 0, cache_write_1h_tokens: 0, reasoning_tokens: 1, total_tokens: 18 },
    cost: { input: 9, output: 6, reasoning: 1, cache_read: 2, cache_write: 0, total: 18, total_picodollars_remainder: 0 } },
  tool_result_entries: results });
function params(peer, payload) {
  const entries = payload.kind === 'model_turn_start' ? [user] : [user, payload.assistant_entry, ...payload.tool_result_entries];
  const head = entries.at(-1).id;
  return { hook: payload.kind, payload, context: peer.context({ session_entries: entries, session_branch: entries, session_leaf_id: head }),
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'run-message:2', owner, expected_head: head } };
}
function transcript(peer) {
  return peer.seen.filter(f => f.method === 'notification' && f.params.message?.startsWith('evt:')).map(f => JSON.parse(f.params.message.slice(4)));
}
async function event(peer, type) {
  const frame = await peer.wait(f => f.method === 'notification' && f.params.message?.startsWith(`evt:{"type":"${type}"`));
  return JSON.parse(frame.params.message.slice(4));
}
async function open(t, fixture = plain) {
  const peer = launch(t, [fixture]);
  await peer.init(['remote_ui', 'session_entries', 'lifecycle_events', 'lifecycle_events_v2', 'before_prompt_state_v1']);
  await peer.start();
  const reply = await peer.request('hook/run', { hook: 'before_prompt', payload: { prompt: 'question', system_prompt: 'system' }, context: peer.context() }).response;
  assert.ok(reply.result, JSON.stringify(reply));
  return peer;
}
function wire(peer, method, extra = {}) { peer.notify(method, { resource_owner: owner, ...extra }); }
function stream(peer, text = 'streamed') {
  wire(peer, 'message/started', { message_id: 'assistant-1' });
  wire(peer, 'message/updated', { message_id: 'assistant-1', delta: text, deltas: 1 });
  wire(peer, 'message/settled', { message_id: 'assistant-1' });
}
async function hook(peer, payload) {
  const reply = await peer.request('hook/run', params(peer, payload)).response;
  assert.ok(reply.result, JSON.stringify(reply));
}
test('overflow refuses a truncated agent_end and stops retaining later deltas', async () => {
  const events = [], runtime = { metadata: () => ({ hooks: [] }), runEvent: async (type, value) => events.push(value) };
  const store = { state: { host: {} } };
  await handleRunMessage(runtime, 'turn/started', {}, store);
  await handleRunMessage(runtime, 'message/started', {}, store);
  store.state.run.bytes = 4 * 1024 * 1024;
  await assert.rejects(handleRunMessage(runtime, 'message/updated', { delta: 'x' }, store), /bounded mirror exceeded/);
  const text = store.state.run.partial.content[0].text;
  await assert.rejects(handleRunMessage(runtime, 'message/updated', { delta: 'more' }, store), /bounded mirror exceeded/);
  assert.equal(store.state.run.partial.content[0].text, text);
  await assert.rejects(handleRunMessage(runtime, 'turn/settled', {}, store), /bounded mirror exceeded/);
  assert.equal(events.some(e => e.type === 'agent_end'), false);
  assert.equal(store.state.run, undefined);
  await handleRunMessage(runtime, 'turn/started', {}, store);
  store.state.run.messages.length = 8192;
  await assert.rejects(handleRunMessage(runtime, 'message/settled', { message: { role: 'custom', content: 'overflow' } }, store), /bounded mirror exceeded/);
});

test('callback mutation and custom messages cannot corrupt the run mirror', async () => {
  const events = [], runtime = { metadata: () => ({ hooks: [] }), runEvent: async (type, value) => {
    events.push(JSON.parse(JSON.stringify(value)));
    if ('message' in value) value.message.content = 'changed by handler';
  } };
  const store = { state: { host: {} } };
  await handleRunMessage(runtime, 'turn/started', {}, store);
  const message = { role: 'custom', content: 'actual custom message', timestamp: 1 };
  await handleRunMessage(runtime, 'message/started', { message }, store);
  await handleRunMessage(runtime, 'message/settled', { message }, store);
  await handleRunMessage(runtime, 'message/updated', { delta: 'actual assistant' }, store);
  await handleRunMessage(runtime, 'turn/settled', {}, store);
  assert.deepEqual(events.at(-1).messages, [message, events.at(-2).message]);
  assert.deepEqual(events.at(-2).message.content, [{ type: 'text', text: 'actual assistant' }]);
});

const zeroUsage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
const committedUsage = { input: 9, output: 7, cacheRead: 2, cacheWrite: 0, totalTokens: 18, reasoning: 1,
  cost: { input: 9 / 1e6, output: 7 / 1e6, cacheRead: 2 / 1e6, cacheWrite: 0, total: 18 / 1e6 } };
function unknownFinalMetadata(message) {
  for (const key of ['usage', 'stopReason', 'model', 'api', 'provider']) assert.equal(Object.hasOwn(message, key), false, `${key} is not a committed fact`);
}

test('stream-only fallback preserves user and assistant text without fabricated final metadata', async t => {
  const peer = await open(t);
  wire(peer, 'turn/started');
  wire(peer, 'message/started', { message_id: 'assistant-1' });
  wire(peer, 'message/updated', { message_id: 'assistant-1', delta: 'Hello ', deltas: 1 });
  wire(peer, 'message/updated', { message_id: 'assistant-1', delta: '世界', deltas: 2 });
  wire(peer, 'message/settled', { message_id: 'assistant-1' });
  wire(peer, 'turn/settled', { outcome: 'completed' });
  await event(peer, 'agent_end');
  const events = transcript(peer);
  assert.deepEqual(events.map(e => e.type), ['agent_start', 'message_start', 'message_end', 'message_start', 'message_update', 'message_update', 'message_end', 'agent_end']);
  assert.deepEqual(events[1].message.content, [{ type: 'text', text: 'question' }]);
  assert.equal(events[1].message.role, 'user');
  assert.ok(Number.isSafeInteger(events[1].message.timestamp));
  assert.deepEqual(events[2].message, events[1].message);
  const initial = events[3].message;
  assert.deepEqual(initial, { role: 'assistant', content: [], model: 'test-model', api: 'openai-responses', provider: 'test', usage: zeroUsage, stopReason: 'pending', timestamp: initial.timestamp });
  assert.deepEqual(events[4].message.content, [{ type: 'text', text: 'Hello ' }]);
  assert.deepEqual(events[5].assistantMessageEvent, { type: 'text_delta', delta: '世界', contentIndex: 0, partial: events[5].message });
  assert.deepEqual(events[6].message.content, [{ type: 'text', text: 'Hello 世界' }]);
  unknownFinalMetadata(events[6].message);
  assert.deepEqual(events[7].messages, [events[2].message, events[6].message]);
  await peer.close();
});

test('model-turn commits settle messages before turn_end, including tool results and unstreamed iterations', async t => {
  const peer = await open(t, turns);
  wire(peer, 'turn/started');
  await hook(peer, start(0));
  stream(peer);
  const entry = structuredClone(assistant);
  entry.value.Assistant.content.push({ ToolCall: { id: 'one', name: 'tool-one', arguments_json: '{}' } });
  const result = { id: 'result', parent: 'assistant', timestamp_unix_ms: 1150, value: { type: 'message', User: { content: [{ ToolResult: { tool_call_id: 'one', content: [{ Text: 'real result' }], is_error: false } }] } } };
  await hook(peer, end(entry, [result], 0, 'tool_use'));
  await hook(peer, start(1));
  // The native message lifecycle has only one start per run, not per iteration.
  wire(peer, 'message/updated', { message_id: 'assistant-1', delta: 'second', deltas: 1 });
  await hook(peer, end(assistant, [], 1));
  await hook(peer, start(2));
  await hook(peer, end(assistant, [], 2, 'max_tokens'));
  wire(peer, 'turn/settled', { outcome: 'completed' });
  await event(peer, 'agent_end');
  const events = transcript(peer);
  assert.deepEqual(events.map(e => e.type), ['agent_start', 'turn_start', 'message_start', 'message_end', 'message_start', 'message_update', 'message_end', 'message_start', 'message_end', 'turn_end', 'turn_start', 'message_start', 'message_update', 'message_end', 'turn_end', 'turn_start', 'message_start', 'message_end', 'turn_end', 'agent_end']);
  assert.deepEqual(events[6].message, events[9].message);
  assert.equal(events[6].message.content[0].text, 'answer');
  const committed = events.filter(event => event.type === 'message_end' && event.message.role === 'assistant').map(event => event.message);
  assert.deepEqual(committed.map(message => message.stopReason), ['toolUse', 'stop', 'length']);
  for (const message of committed) {
    assert.equal(message.model, 'native-model');
    assert.deepEqual(message.usage, committedUsage);
  }
  assert.deepEqual(committed, events.filter(event => event.type === 'turn_end').map(event => event.message));
  assert.deepEqual(events[8].message, { role: 'toolResult', toolCallId: 'one', toolName: 'tool-one', content: [{ type: 'text', text: 'real result' }], isError: false, timestamp: 1150 });
  assert.deepEqual(events.at(-1).messages, events.filter(e => e.type === 'message_end').map(e => e.message));
  await peer.close();
});

test('failed turn projection settles the streamed partial without failing the host hook', async t => {
  const peer = await open(t, turns);
  wire(peer, 'turn/started');
  await hook(peer, start(0));
  stream(peer);
  const opaque = structuredClone(assistant);
  opaque.value.Assistant.content.push({ Reasoning: { text: 'reason', state: { secret: 'opaque' } } });
  await hook(peer, end(opaque));
  wire(peer, 'turn/settled', { outcome: 'completed' });
  await event(peer, 'agent_end');
  const events = transcript(peer);
  const ended = events.filter(e => e.type === 'message_end');
  assert.equal(ended.length, 2);
  assert.deepEqual(ended[1].message.content, [{ type: 'text', text: 'streamed' }]);
  unknownFinalMetadata(ended[1].message);
  assert.deepEqual(events.at(-1).messages, ended.map(e => e.message));
  assert.equal(events.some(e => e.type === 'turn_end'), false);
  assert.equal(peer.seen.filter(f => f.method === 'notification' && f.params.title === '[Extension issues]' && f.params.message.includes('turn_end: octet could not convert')).length, 1);
  await peer.close();
});

test('custom lifecycle messages pass through unchanged', async t => {
  const peer = await open(t);
  const message = { role: 'custom', customType: 'job', content: [{ type: 'text', text: 'done' }], display: false, timestamp: 123, details: { job: 1 } };
  wire(peer, 'message/started', { message_id: 'custom-1', message });
  wire(peer, 'message/settled', { message_id: 'custom-1', message });
  assert.deepEqual((await event(peer, 'message_start')).message, message);
  assert.deepEqual((await event(peer, 'message_end')).message, message);
  await peer.close();
});

test('run settlement closes an unfinished stream and the next run starts with no old messages', async t => {
  const peer = await open(t, turns);
  wire(peer, 'turn/started');
  await hook(peer, start(0));
  wire(peer, 'message/started', { message_id: 'assistant-1' });
  wire(peer, 'message/updated', { message_id: 'assistant-1', delta: 'partial', deltas: 1 });
  wire(peer, 'turn/settled', { outcome: 'cancelled' });
  const first = await event(peer, 'agent_end');
  assert.equal(first.messages.length, 2);
  assert.deepEqual(first.messages[1].content, [{ type: 'text', text: 'partial' }]);
  unknownFinalMetadata(first.messages[1]);
  wire(peer, 'turn/started');
  wire(peer, 'turn/settled', { outcome: 'completed' });
  assert.deepEqual((await event(peer, 'agent_end')).messages, []);
  await peer.close();
});

test('native custom-message fixture records only Pi public events and durable message facts', async t => {
  const source = readFileSync(join(root, '../../crates/octet-coding-agent/src/modes/interactive/tests/pi_messages_tests.rs'), 'utf8');
  const wrapper = source.match(/let factory = format!\(\s*r#"([\s\S]*?)"#,\s*factory\.replacen/);
  assert.ok(wrapper, 'use the native acceptance fixture callback, not a duplicate');
  const directory = mkdtempSync(join(tmpdir(), 'octet-pi-native-message-fixture-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const trace = join(directory, 'trace.jsonl'), factory = join(directory, 'fixture.mjs');
  const code = wrapper[1].replaceAll('{{', '{').replaceAll('}}', '}')
    .replace('{}', `const TRACE = ${JSON.stringify(trace)}; const fixtureFactory = () => {};`);
  writeFileSync(factory, code);
  const handlers = new Map();
  (await import(pathToFileURL(factory).href)).default({ on: (type, handler) => handlers.set(type, handler) });
  const runtime = { runEvent: async (type, event) => handlers.get(type)?.(event) }, store = { state: {} };
  const messages = [
    { role: 'custom', customType: 'first', content: 'durable first', display: false, timestamp: 111, details: { private: 'retained details' } },
    { role: 'custom', customType: 'second', content: [{ type: 'text', text: 'durable second' }], display: true, timestamp: 222 },
  ];
  for (const [index, message] of messages.entries()) {
    const payload = { message_id: `host-entry-${index}`, message };
    await handleRunMessage(runtime, 'message/started', payload, store);
    await handleRunMessage(runtime, 'message/settled', payload, store);
  }
  const events = readFileSync(trace, 'utf8').trim().split('\n').map(line => JSON.parse(line));
  assert.deepEqual(events, messages.flatMap(message => ['message_start', 'message_end'].map(type => ({ type, message }))));
});
