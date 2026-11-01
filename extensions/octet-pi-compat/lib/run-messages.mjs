// Pi message observations over the native whole-run and model-turn boundaries.
// This bounded mirror never writes the session or feeds provider context.
import { bounded, facade, strict, unsupported } from './errors.mjs';
import { messageEntry } from './model-turns.mjs';

const MAX_MESSAGES = 8192, MAX_BYTES = 4 * 1024 * 1024;
export const runMessageMethods = new Set(['turn/started', 'turn/settled', 'message/started', 'message/updated', 'message/settled']);
const copy = value => JSON.parse(JSON.stringify(value)); // Also snapshots strict message proxies.
const size = value => Buffer.byteLength(JSON.stringify(value));
function check(run, bytes, count = 1) {
  if (run.invalid || run.bytes + bytes > MAX_BYTES || run.messages.length + count > MAX_MESSAGES) {
    run.invalid = true;
    unsupported('run message history', 'bounded mirror exceeded; no truncated transcript is returned');
  }
}
function append(run, message) {
  const bytes = size(message); check(run, bytes);
  run.bytes += bytes; run.messages.push(copy(message));
}
export function assistantObservation(message) {
  if (message.role !== 'assistant') return message;
  if (Object.hasOwn(message, 'usage')) message.usage = strict(message.usage, 'assistant usage');
  return strict(message, 'assistant message');
}
async function emit(runtime, store, type, fields = {}) {
  // Later deltas or callbacks must not mutate an earlier observation/history.
  // Restore unavailable-field refusals after JSON snapshots strip the proxies.
  const snapshot = copy(fields);
  if (snapshot.message) snapshot.message = assistantObservation(snapshot.message);
  if (snapshot.messages) snapshot.messages = snapshot.messages.map(assistantObservation);
  if (snapshot.assistantMessageEvent?.partial) snapshot.assistantMessageEvent.partial = assistantObservation(snapshot.assistantMessageEvent.partial);
  await runtime.runEvent(type, facade({ type, ...snapshot }, `${type} event`), store);
}
export function rememberPrompt(store, payload) {
  if (!store.state || payload.prompt === undefined) return;
  store.state.pendingPrompt = { role: 'user', content: [{ type: 'text', text: bounded(payload.prompt, 'run prompt', 262144, { controls: true }) }], timestamp: Date.now() };
  if (payload.system_prompt !== undefined && store.state.observedSystemPrompt !== payload.system_prompt) {
    store.state.pendingSystem = { role: 'system', content: payload.system_prompt, timestamp: Date.now() };
    store.state.observedSystemPrompt = payload.system_prompt;
  }
}
export async function emitRunPrompt(runtime, store) {
  const run = store.state?.run;
  if (!run) return;
  if (run.system) {
    const message = run.system; run.system = undefined;
    append(run, message);
    await emit(runtime, store, 'message_start', { message });
    await emit(runtime, store, 'message_end', { message });
  }
  if (!run.prompt) return;
  const message = run.prompt; run.prompt = undefined;
  append(run, message);
  await emit(runtime, store, 'message_start', { message });
  await emit(runtime, store, 'message_end', { message });
}
async function startPartial(runtime, store) {
  const run = store.state.run;
  if (run.partial) return;
  await emitRunPrompt(runtime, store);
  const host = store.state.host, model = host.model_view;
  const partial = { role: 'assistant', content: [], timestamp: Date.now(),
    ...(host.model ? { model: host.model } : {}), ...(model ? { api: model.api, provider: model.provider } : {}),
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: 'pending' };
  check(run, size(partial)); run.partial = partial;
  await emit(runtime, store, 'message_start', { message: partial });
}
export async function settleStreamedPartial(runtime, store) {
  const run = store.state?.run;
  if (!run?.partial) return;
  const message = run.partial; run.partial = undefined;
  // Pi initializes a stream with zero usage and pending, but these are not
  // terminal facts. Neither selected-model state nor whole-run outcomes supply
  // per-response accounting/identity or stop reasons (even a failed/cancelled
  // run can follow a successful provider response). Missing reads must refuse.
  for (const key of ['usage', 'stopReason', 'model', 'api', 'provider']) delete message[key];
  append(run, message);
  await emit(runtime, store, 'message_end', { message });
}
export async function settleCommittedAssistant(runtime, store, payload) {
  const run = store.state?.run;
  if (!run || run.assistantEntries?.has(payload.assistant_entry.id)) return;
  const [message] = messageEntry(payload.assistant_entry, new Map(), payload.assistant_metadata);
  await emitRunPrompt(runtime, store);
  if (!run.partial) await startPartial(runtime, store);
  for (const [contentIndex, part] of message.content.entries()) {
    if (part.type !== 'toolCall' || run.streamCalls?.size) continue;
    const partial = run.partial;
    partial.content[contentIndex] = { ...part, arguments: {} };
    await emit(runtime, store, 'message_update', { message: partial,
      assistantMessageEvent: { type: 'toolcall_start', contentIndex, partial } });
    const delta = JSON.stringify(part.arguments);
    partial.content[contentIndex] = copy(part);
    await emit(runtime, store, 'message_update', { message: partial,
      assistantMessageEvent: { type: 'toolcall_delta', contentIndex, delta, partial } });
    await emit(runtime, store, 'message_update', { message: partial,
      assistantMessageEvent: { type: 'toolcall_end', contentIndex, toolCall: part, partial } });
  }
  run.partial = undefined;
  (run.assistantEntries ??= new Set()).add(payload.assistant_entry.id);
  run.streamCalls = undefined;
  append(run, message);
  await emit(runtime, store, 'message_end', { message });
}

export async function settleToolResult(runtime, store, payload) {
  const run = store.state?.run;
  if (!run) return;
  const message = { role: 'toolResult', toolCallId: payload.tool_call_id, toolName: payload.tool_name,
    ...copy(payload.result), isError: payload.isError === true, timestamp: Date.now() };
  (run.toolResultIds ??= new Set()).add(message.toolCallId);
  append(run, message);
  await emit(runtime, store, 'message_start', { message });
  await emit(runtime, store, 'message_end', { message });
}

export async function settleRunTurn(runtime, store, event) {
  const run = store.state?.run;
  if (!run) return; // Auxiliary model hooks need not have a whole-run lifecycle.
  await emitRunPrompt(runtime, store);
  const messages = [event.message, ...event.toolResults];
  check(run, messages.reduce((bytes, message) => bytes + size(message), 0), messages.length);
  if (!run.assistantEntries?.has(event.messageEntryId)) {
    if (!run.partial) await emit(runtime, store, 'message_start', { message: event.message });
    run.partial = undefined;
    append(run, event.message);
    await emit(runtime, store, 'message_end', { message: event.message });
  }
  for (const message of event.toolResults) {
    if (run.toolResultIds?.has(message.toolCallId)) continue;
    append(run, message);
    await emit(runtime, store, 'message_start', { message });
    await emit(runtime, store, 'message_end', { message });
  }
}
export async function handleRunMessage(runtime, method, payload, store) {
  const state = store.state;
  if (method === 'turn/started') {
    state.run = { messages: [], bytes: 0, prompt: state.pendingPrompt, system: state.pendingSystem };
    state.pendingPrompt = undefined; state.pendingSystem = undefined;
    await emit(runtime, store, 'agent_start');
    // Pi emits the first turn_start before the user's message boundaries.
    if (!runtime.metadata().hooks.includes('model_turn_start')) await emitRunPrompt(runtime, store);
    return;
  }
  if (method === 'turn/settled') {
    try {
      await emitRunPrompt(runtime, store);
      await settleStreamedPartial(runtime, store);
      if (state.run) check(state.run, 0, 0);
      await emit(runtime, store, 'agent_end', { messages: state.run?.messages ?? [] });
    } finally { state.run = undefined; }
    return;
  }
  if (payload.assistant_entry) {
    runtime.require('committed_message_events_v1');
    await settleCommittedAssistant(runtime, store, payload);
    return;
  }
  if (payload.message) {
    // Native custom messages already have the Pi message shape. They may also
    // arrive outside a run; do not reinterpret them as assistant stream text.
    if (method === 'message/settled' && state.run) append(state.run, payload.message);
    await emit(runtime, store, method === 'message/started' ? 'message_start' : 'message_end', { message: payload.message });
    return;
  }
  const run = state.run;
  if (!run) return;
  check(run, 0, 0);
  if (method === 'message/settled') {
    // A single host stream spans multiple iterations. Durable entries, when
    // subscribed, supply each assistant boundary and its paired tool results.
    if (!runtime.metadata().hooks.includes('model_turn_end')) await settleStreamedPartial(runtime, store);
    return;
  }
  await startPartial(runtime, store);
  if (payload.tool_call) {
    runtime.require('committed_message_events_v1');
    const { phase, index, id, name, delta } = payload.tool_call;
    const partial = run.partial;
    const calls = run.streamCalls ??= new Map();
    if (phase === 'start') {
      const contentIndex = partial.content.length;
      calls.set(index, { contentIndex, text: '' });
      partial.content.push({ type: 'toolCall', id, name, arguments: {} });
      await emit(runtime, store, 'message_update', { message: partial, assistantMessageEvent: { type: 'toolcall_start', contentIndex, partial } });
    } else {
      const call = calls.get(index);
      if (!call) return;
      const { contentIndex } = call;
      if (phase === 'delta') {
        call.text += delta;
        try { partial.content[contentIndex].arguments = JSON.parse(call.text); } catch { /* provider JSON is incomplete */ }
        await emit(runtime, store, 'message_update', { message: partial, assistantMessageEvent: { type: 'toolcall_delta', contentIndex, delta, partial } });
      } else {
        await emit(runtime, store, 'message_update', { message: partial, assistantMessageEvent: { type: 'toolcall_end', contentIndex, toolCall: partial.content[contentIndex], partial } });
      }
    }
    return;
  }
  if (method === 'message/updated') {
    const delta = bounded(payload.delta, 'message delta', 8192, { controls: true });
    const partial = run.partial;
    if (!partial.content.length) partial.content.push({ type: 'text', text: '' });
    partial.content[0].text += delta;
    check(run, size(partial));
    await emit(runtime, store, 'message_update', { message: partial,
      assistantMessageEvent: { type: 'text_delta', contentIndex: 0, delta, partial } });
  }
}
