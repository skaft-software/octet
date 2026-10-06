// Actual model iterations, not whole-run lifecycle notification aliases.
import { bounded, facade, fields, invalid, ownerKey, plainJSON, strict, unsupported } from './errors.mjs';
import { canonicalToPi, cancellable } from './provider-context.mjs';
import { assistantObservation, emitRunPrompt, settleRunTurn, settleStreamedPartial } from './run-messages.mjs';

export const modelTurnHooks = ['model_turn_start', 'model_turn_end'];

function assistantMetadata(message, entry, data) {
  fields(data, ['assistant_entry_id', 'model', 'usage', 'cost', 'stop_reason'], 'assistant metadata');
  if (data.assistant_entry_id !== entry.id) invalid('assistant metadata entry mismatch');
  if (data.model != null && data.model !== message.model) invalid('assistant metadata model mismatch');
  const count = (value, label) => {
    if (!Number.isSafeInteger(value) || value < 0) invalid(`assistant ${label} must be a nonnegative safe integer`);
    return value;
  };
  const usageFields = ['input_tokens', 'output_tokens', 'cache_read_tokens', 'cache_write_tokens', 'cache_write_1h_tokens', 'reasoning_tokens', 'total_tokens'];
  fields(data.usage, usageFields, 'assistant usage');
  for (const key of usageFields) count(data.usage[key], `usage.${key}`);
  const usage = data.usage;
  if (usage.reasoning_tokens > usage.output_tokens || usage.cache_write_1h_tokens > usage.cache_write_tokens) invalid('assistant usage subset');
  message.usage = { input: usage.input_tokens, output: usage.output_tokens, cacheRead: usage.cache_read_tokens,
    cacheWrite: usage.cache_write_tokens, totalTokens: usage.total_tokens };
  // Native zero cannot distinguish an unreported optional split from a reported zero.
  if (usage.reasoning_tokens) message.usage.reasoning = usage.reasoning_tokens;
  if (usage.cache_write_1h_tokens) message.usage.cacheWrite1h = usage.cache_write_1h_tokens;
  if (data.cost != null) {
    const costFields = ['input', 'output', 'reasoning', 'cache_read', 'cache_write', 'total', 'total_picodollars_remainder'];
    fields(data.cost, costFields, 'assistant cost');
    for (const key of costFields) count(data.cost[key], `cost.${key}`);
    const cost = data.cost;
    if (cost.total_picodollars_remainder >= 1e6) invalid('assistant cost remainder');
    // Native categories are microdollars; reasoning cost is separate from output.
    // Preserve the request's exact total remainder, not session accounting carry.
    message.usage.cost = { input: cost.input / 1e6, output: count(cost.output + cost.reasoning, 'cost.output') / 1e6,
      cacheRead: cost.cache_read / 1e6, cacheWrite: cost.cache_write / 1e6,
      total: cost.total / 1e6 + cost.total_picodollars_remainder / 1e12 };
  }
  if (data.stop_reason == null) return; // Legacy ledger: known counts, unknown outcome.
  switch (data.stop_reason) {
    case 'end_turn': case 'stop_sequence': case 'pause_turn': message.stopReason = 'stop'; break;
    case 'max_tokens': message.stopReason = 'length'; break;
    case 'tool_use': message.stopReason = 'toolUse'; break;
    case 'refusal':
      message.stopReason = 'error'; message.errorMessage = 'The model refused to complete the request'; break;
    default: unsupported('assistant stop reason', 'native outcome has no faithful Pi terminal message binding');
  }
}

function messageEntry(entry, calls, assistantFacts) {
  if (entry?.value?.type !== 'message') invalid('model turn requires durable message entries');
  bounded(entry.id, 'model turn entry id', 256);
  if (entry.parent !== null) bounded(entry.parent, 'model turn parent id', 256);
  const { type, ...canonical } = entry.value;
  const messages = canonicalToPi([canonical], calls, { observation: true });
  const metadata = entry.metadata?.tool_output?.metadata;
  if (metadata && Object.hasOwn(metadata, 'pi_details')) {
    if (messages.length !== 1 || messages[0].role !== 'toolResult') unsupported('model turn batched Pi details', 'native entry does not identify which result owns these details');
    messages[0].details = plainJSON(metadata.pi_details, 'Pi tool details');
  }
  // These are messages, not invented Pi Session entries: one native tool-result
  // batch can faithfully supply several messages with its actual commit time.
  if (entry.timestamp_unix_ms !== undefined) {
    if (!Number.isSafeInteger(entry.timestamp_unix_ms) || entry.timestamp_unix_ms < 0) invalid('model turn entry timestamp');
    for (const message of messages) message.timestamp = entry.timestamp_unix_ms;
  }
  if (assistantFacts != null) {
    if (messages.length !== 1 || messages[0].role !== 'assistant') invalid('assistant metadata requires one assistant');
    assistantMetadata(messages[0], entry, assistantFacts);
  }
  return messages.map(message => message.role === 'assistant' ? assistantObservation(message) : strict(message, 'model turn message'));
}

export async function modelTurn(runtime, params, store) {
  runtime.require('session_entries');
  const hook = params.hook;
  if (!modelTurnHooks.includes(hook) || !runtime.metadata().hooks.includes(hook)) invalid('undeclared model turn hook');
  ownerKey(params.context?.resource_owner);
  const body = params.payload, signal = store.controller.signal;
  fields(body, hook === 'model_turn_start' ? ['kind', 'run_id', 'turn_index', 'timestamp_ms']
    : ['kind', 'run_id', 'turn_index', 'timestamp_ms', 'assistant_entry', 'assistant_metadata', 'tool_result_entries'], 'model turn payload');
  if (body.kind !== hook) invalid('model turn kind');
  bounded(body.run_id, 'model turn run id', 256);
  if (!Number.isSafeInteger(body.turn_index) || body.turn_index < 0) invalid('model turn index');
  if (!Number.isSafeInteger(body.timestamp_ms) || body.timestamp_ms < 0) invalid('model turn timestamp');
  runtime.bind(params, store, { requireLeaf: 'model turn hook' });
  store.providerContext = true; // Existing awaited-leaf retirement cancellation.
  const live = () => { signal.throwIfAborted(); runtime.assertSessionOwner(store); };
  const reply = { disposition: { action: 'continue' }, context: [], notifications: [], session_operation: { action: 'continue' } };
  return cancellable(runtime.queued(store, async () => {
    live();
    let event;
    // An observation Pi cannot be shown is reported, never a failed turn.
    try {
      event = turnEvent(hook, body);
      if (hook === 'model_turn_end') await cancellable(settleRunTurn(runtime, store, event), signal);
    }
    catch (error) {
      signal.throwIfAborted();
      if (error?.code === -32800 || error?.code === -32002) throw error;
      if (hook === 'model_turn_end' && !store.state.run?.invalid) await cancellable(settleStreamedPartial(runtime, store), signal);
      await cancellable(runtime.flush(store), signal); live();
      runtime.reportObservationError(hook === 'model_turn_start' ? 'turn_start' : 'turn_end');
      return reply;
    }
    await cancellable(runtime.runEvent(event.type, facade(event, `${event.type} event`), store), signal);
    if (hook === 'model_turn_start') await cancellable(emitRunPrompt(runtime, store), signal);
    await cancellable(runtime.flush(store), signal); live();
    return reply;
  }), signal);
}

function turnEvent(hook, body) {
    const event = { type: hook === 'model_turn_start' ? 'turn_start' : 'turn_end', turnIndex: body.turn_index };
    if (hook === 'model_turn_start') event.timestamp = body.timestamp_ms;
    else {
      const calls = new Map(), assistant = messageEntry(body.assistant_entry, calls, body.assistant_metadata);
      if (assistant.length !== 1 || assistant[0].role !== 'assistant') invalid('model turn assistant entry');
      if (!Array.isArray(body.tool_result_entries) || body.tool_result_entries.length > 8192) invalid('model turn tool result entries');
      const entries = new Set([body.assistant_entry.id]), results = new Set(), toolResults = [];
      for (const entry of body.tool_result_entries) {
        if (entries.has(entry?.id)) invalid('duplicate model turn entry');
        entries.add(entry?.id);
        for (const message of messageEntry(entry, calls)) {
          if (message.role !== 'toolResult') unsupported('model turn mixed user/tool entry', 'cannot discard non-result content');
          if (results.has(message.toolCallId)) invalid('duplicate model turn tool result');
          results.add(message.toolCallId); toolResults.push(message);
        }
      }
      if (results.size !== calls.size) invalid('model turn contains unsettled tool calls');
      event.message = assistant[0]; event.toolResults = toolResults;
    }
    return event;
}
