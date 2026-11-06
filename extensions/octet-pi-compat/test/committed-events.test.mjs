import test from 'node:test';
import assert from 'node:assert/strict';
import { handleRunMessage, rememberPrompt, settleRunTurn, settleToolResult } from '../lib/run-messages.mjs';
import { toolLifecyclePayload } from '../lib/tool-events.mjs';

const assistant = { id: 'assistant', parent: 'user', timestamp_unix_ms: 1100, value: { type: 'message', Assistant: {
  model: 'native-model', protocol: 'open_ai_chat', content: [{ ToolCall: { id: 'one', name: 'probe', arguments_json: '{"label":"fast"}' } }],
} } };
const plain = value => JSON.parse(JSON.stringify(value));
function fixture() {
  const events = [];
  const store = { state: { host: {}, workspace: '/workspace' } };
  const runtime = { require(feature) { assert.equal(feature, 'committed_message_events_v1'); },
    metadata: () => ({ hooks: [] }), runEvent: async (type, event) => events.push(plain(event)) };
  return { store, runtime, events, notify: (method, payload = {}) => handleRunMessage(runtime, method, payload, store) };
}

test('committed-message profile emits system/user, live toolcall deltas, assistant before execution, and no duplicate at turn end', async () => {
  const f = fixture();
  rememberPrompt(f.store, { prompt: 'question', system_prompt: 'native prompt' });
  await f.notify('turn/started');
  for (const tool_call of [{ phase: 'start', index: 0, id: 'one', name: 'probe' },
    { phase: 'delta', index: 0, delta: '{"label":' }, { phase: 'delta', index: 0, delta: '"fast"}' }, { phase: 'end', index: 0 }]) {
    await f.notify('message/updated', { tool_call });
  }
  await f.notify('message/settled', { assistant_entry: assistant });
  const start = toolLifecyclePayload({ tool_call_id: 'one', tool_name: 'probe', args: { label: 'fast' }, session_id: 'private', duration_ms: 9 }, f.store);
  await f.runtime.runEvent('tool_execution_start', { type: 'tool_execution_start', ...start });
  const end = { tool_call_id: 'one', tool_name: 'probe', result: { content: [{ type: 'text', text: 'result' }], details: { ok: true } }, isError: false };
  await f.runtime.runEvent('tool_execution_end', { type: 'tool_execution_end', ...toolLifecyclePayload(end, f.store) });
  await settleToolResult(f.runtime, f.store, end);
  await settleRunTurn(f.runtime, f.store, { messageEntryId: assistant.id, message: plain(f.events.find(e => e.type === 'message_end' && e.message.role === 'assistant').message),
    toolResults: [{ role: 'toolResult', toolCallId: 'one', toolName: 'probe', content: [{ type: 'text', text: 'result' }], isError: false, timestamp: 1110 }] });
  await f.notify('message/settled', { message_id: 'whole-stream' });
  await f.notify('turn/settled');
  const boundaries = f.events.filter(e => ['message_end', 'tool_execution_start', 'tool_execution_end'].includes(e.type));
  assert.deepEqual(boundaries.map(e => e.type === 'message_end' ? e.message.role : e.type), ['system', 'user', 'assistant', 'tool_execution_start', 'tool_execution_end', 'toolResult']);
  const updates = f.events.filter(e => e.type === 'message_update');
  assert.deepEqual(updates.map(e => e.assistantMessageEvent.type), ['toolcall_start', 'toolcall_delta', 'toolcall_delta', 'toolcall_end']);
  assert.deepEqual(updates.at(-1).assistantMessageEvent.toolCall.arguments, { label: 'fast' });
  assert.deepEqual(f.events.at(-1).messages.map(m => m.role), ['system', 'user', 'assistant', 'toolResult']);
  assert.equal('session_id' in start, false);
  assert.equal('durationMs' in start, false);
});

test('tool-only finished provider responses still create assistant start, update, end before any execution', async () => {
  const f = fixture(); await f.notify('turn/started');
  await f.notify('message/settled', { assistant_entry: assistant });
  assert.deepEqual(f.events.map(e => e.type), ['agent_start', 'message_start', 'message_update', 'message_update', 'message_update', 'message_end']);
  assert.equal(f.events.at(-1).message.content[0].arguments.label, 'fast');
});

test('Pi execution payloads omit native routing fields and preserve partial result, caller args and supported duration', () => {
  const f = fixture();
  const args = { label: 'fast' };
  const start = toolLifecyclePayload({ tool_call_id: 'one', tool_name: 'probe', args, resource_owner: {}, session_id: 'secret', outcome: 'started' }, f.store);
  assert.deepEqual(start, { toolCallId: 'one', toolName: 'probe', args });
  args.label = 'mutated';
  const partialResult = { content: [{ type: 'text', text: 'partial' }], details: { index: 1 } };
  assert.deepEqual(toolLifecyclePayload({ tool_call_id: 'one', tool_name: 'probe', partialResult }, f.store), {
    toolCallId: 'one', toolName: 'probe', args: { label: 'fast' }, partialResult,
  });
  assert.deepEqual(toolLifecyclePayload({ tool_call_id: 'one', tool_name: 'probe', result: partialResult, isError: true, duration_ms: 50, reason: 'native' }, f.store), {
    toolCallId: 'one', toolName: 'probe', result: partialResult, isError: true, durationMs: 50,
  });
});

test('a second model iteration streams text after the first committed tool-only assistant', async () => {
  const f = fixture(); await f.notify('turn/started');
  await f.notify('message/settled', { assistant_entry: assistant });
  await f.notify('message/updated', { delta: 'final answer' });
  assert.equal(f.events.at(-1).assistantMessageEvent?.type, 'text_delta');
  assert.equal(f.events.at(-1).message.content[0].text, 'final answer');
});
