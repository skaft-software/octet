import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { Runtime } from '../lib/runtime.mjs';
import { inspect, launch, owner, root } from './helper.mjs';

const factory = join(root, 'test/fixtures/standalone-observation.ts');
const hooks = ['before_prompt', 'model_turn_end', 'session_end', 'session_start'];
function wire(peer, method, extra = {}) { peer.notify(method, { resource_owner: owner, ...extra }); }
async function settled(peer) {
  const frame = await peer.wait(f => f.method === 'notification' && f.params.message.startsWith('standalone:'));
  return JSON.parse(frame.params.message.slice('standalone:'.length));
}
async function commit(peer, assistant, results = [], index = 0) {
  const entries = [assistant, ...results], head = entries.at(-1).id;
  return peer.request('hook/run', {
    hook: 'model_turn_end', payload: { kind: 'model_turn_end', run_id: 'run:user', turn_index: index, timestamp_ms: 1200 + index,
      assistant_entry: assistant, tool_result_entries: results },
    context: peer.context({ session_entries: entries, session_branch: entries, session_leaf_id: head }),
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'observation:2', owner, expected_head: head },
  }).response;
}

test('pure agent_end registration captures internal observation hooks without inventing public Pi callbacks', () => {
  const metadata = inspect([factory]);
  assert.deepEqual(metadata.events, ['agent_end']);
  assert.deepEqual(metadata.hooks, hooks);
  assert.deepEqual(metadata.tools, []); assert.deepEqual(metadata.commands, []);
});

test('pure lifecycle notifications get owner bindings; message observers also get prompt and commit inputs', t => {
  const runtime = new Runtime({ extensions: [factory] }, { closed: false });
  t.after(() => runtime.uninstallChildren());
  for (const event of ['agent_start', 'agent_settled', 'tool_execution_start', 'tool_execution_end', 'model_select',
    'agent_end', 'message_start', 'message_update', 'message_end']) {
    runtime.events.clear(); runtime.events.set(event, [{ factory: 0, handler: () => {} }]);
    assert.deepEqual(runtime.metadata().events, [event]);
    const messages = ['agent_end', 'message_start', 'message_update', 'message_end'].includes(event);
    assert.deepEqual(runtime.metadata().hooks, messages ? hooks : ['session_end', 'session_start'], event);
  }
});

test('standalone agent_end receives the real user, committed assistants, and tool results without unrelated registrations', async t => {
  const peer = launch(t, [factory]);
  assert.deepEqual(peer.metadata.hooks, hooks);
  await peer.init(['session_entries', 'lifecycle_events', 'lifecycle_events_v2']);
  await peer.start();
  // An owner-only lifecycle is usable even before any prompt or command binds it.
  wire(peer, 'turn/started'); wire(peer, 'turn/settled');
  assert.deepEqual(await settled(peer), { type: 'agent_end', messages: [] });
  const prompt = await peer.request('hook/run', { hook: 'before_prompt', payload: { prompt: 'the real question' }, context: peer.context() }).response;
  assert.ok(prompt.result, JSON.stringify(prompt));
  wire(peer, 'turn/started');
  wire(peer, 'message/started', { message_id: 'assistant-1' });
  wire(peer, 'message/updated', { message_id: 'assistant-1', delta: 'partial', deltas: 1 });
  const assistant = { id: 'assistant', parent: 'user', timestamp_unix_ms: 1100, value: { type: 'message', Assistant: {
    model: 'native-model', protocol: 'open_ai_chat', content: [{ Text: 'committed answer' }, { ToolCall: { id: 'one', name: 'tool-one', arguments_json: '{}' } }],
  } } };
  const result = { id: 'result', parent: assistant.id, timestamp_unix_ms: 1150, value: { type: 'message', User: {
    content: [{ ToolResult: { tool_call_id: 'one', content: [{ Text: 'actual result' }], is_error: false } }],
  } } };
  assert.ok((await commit(peer, assistant, [result])).result);
  const last = { id: 'last', parent: result.id, timestamp_unix_ms: 1200, value: { type: 'message', Assistant: {
    model: 'native-model', protocol: 'open_ai_chat', content: [{ Text: 'unstreamed final answer' }],
  } } };
  assert.ok((await commit(peer, last, [], 1)).result);
  wire(peer, 'message/settled', { message_id: 'assistant-1' });
  wire(peer, 'turn/settled');
  const event = await settled(peer);
  assert.deepEqual(event.messages.map(message => message.role), ['user', 'assistant', 'toolResult', 'assistant']);
  assert.deepEqual(event.messages.map(message => message.content[0].text), ['the real question', 'committed answer', 'actual result', 'unstreamed final answer']);
  assert.deepEqual(event.messages.slice(1).map(message => message.timestamp), [1100, 1150, 1200]);
  assert.equal(event.messages[2].toolName, 'tool-one');
  assert.equal('usage' in event.messages[1], false, 'no invented final usage');
  assert.equal(peer.seen.some(f => f.method === 'notification' && f.params.title === '[Extension issues]'), false);
  await peer.close();
});

test('internal projection diagnostics name the actual message observer, not an unrelated adapter file', async t => {
  const peer = launch(t, [factory]); await peer.init(['session_entries', 'lifecycle_events']); await peer.start();
  wire(peer, 'turn/started');
  const privateText = 'private-native-reasoning-should-not-be-printed';
  const assistant = { id: 'assistant', parent: 'user', timestamp_unix_ms: 1100, value: { type: 'message', Assistant: {
    model: 'native-model', protocol: 'open_ai_chat', content: [{ Reasoning: { text: privateText, state: { secret: privateText } } }],
  } } };
  for (let i = 0; i < 2; i++) assert.ok((await commit(peer, assistant, [], i)).result);
  const notice = await peer.wait(f => f.method === 'notification' && f.params.title === '[Extension issues]');
  assert.ok(notice.params.message.includes(factory), notice.params.message);
  assert.match(notice.params.message, /turn_end: octet could not convert/);
  assert.match(notice.params.message, /Next:/);
  assert.equal(notice.params.message.includes(privateText), false);
  assert.equal(peer.stderr().includes(privateText), false);
  assert.equal(peer.seen.filter(f => f.method === 'notification' && f.params.title === '[Extension issues]').length, 1);
  assistant.value.Assistant.content = [{ Text: 'later real answer' }];
  assert.ok((await commit(peer, assistant, [], 2)).result);
  wire(peer, 'turn/settled');
  assert.deepEqual((await settled(peer)).messages.map(message => message.content[0].text), ['later real answer']);
  await peer.close();
});

test('internal model-end observation inputs still require the initialized native session-entry feature', async t => {
  const peer = launch(t, [factory]);
  const reply = await peer.request('initialize', { api_version: '0.4',
    contributes: { tools: [], commands: [], hooks: peer.metadata.hooks, tool_renderers: [] },
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: ['lifecycle_events'] },
  }).response;
  assert.match(reply.error?.message ?? '', /unsupported_feature session_entries/);
  await peer.close();
});
