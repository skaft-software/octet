// Actual model iterations, not whole-run lifecycle notification aliases.
import { bounded, fields, invalid, ownerKey, plainJSON, strict, unsupported } from './errors.mjs';
import { canonicalToPi, cancellable } from './provider-context.mjs';

export const modelTurnHooks = ['model_turn_start', 'model_turn_end'];

function messageEntry(entry, calls) {
  if (entry?.value?.type !== 'message') invalid('model turn requires durable message entries');
  bounded(entry.id, 'model turn entry id', 256);
  if (entry.parent !== null) bounded(entry.parent, 'model turn parent id', 256);
  const { type, ...canonical } = entry.value;
  const messages = canonicalToPi([canonical], calls);
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
  return messages.map(message => strict(message, 'model turn message'));
}

export async function modelTurn(runtime, params, store) {
  runtime.require('session_entries');
  const hook = params.hook;
  if (!modelTurnHooks.includes(hook) || !runtime.metadata().hooks.includes(hook)) invalid('undeclared model turn hook');
  ownerKey(params.context?.resource_owner); runtime.bind(params, store);
  if (!store.leaf?.grant) unsupported('model turn hook', 'actual native session_leaf consumer required');
  store.providerContext = true; // Existing awaited-leaf retirement cancellation.
  const body = params.payload, signal = store.controller.signal;
  fields(body, hook === 'model_turn_start' ? ['kind', 'run_id', 'turn_index', 'timestamp_ms']
    : ['kind', 'run_id', 'turn_index', 'timestamp_ms', 'assistant_entry', 'tool_result_entries'], 'model turn payload');
  if (body.kind !== hook) invalid('model turn kind');
  bounded(body.run_id, 'model turn run id', 256);
  if (!Number.isSafeInteger(body.turn_index) || body.turn_index < 0) invalid('model turn index');
  if (!Number.isSafeInteger(body.timestamp_ms) || body.timestamp_ms < 0) invalid('model turn timestamp');
  const live = () => { signal.throwIfAborted(); runtime.assertOwner(store); };
  return cancellable(runtime.queued(store, async () => {
    live();
    const event = { type: hook === 'model_turn_start' ? 'turn_start' : 'turn_end', turnIndex: body.turn_index };
    if (hook === 'model_turn_start') event.timestamp = body.timestamp_ms;
    else {
      const calls = new Map(), assistant = messageEntry(body.assistant_entry, calls);
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
    await cancellable(runtime.runEvent(event.type, strict(event, `${event.type} event`), store), signal);
    await cancellable(runtime.flush(store), signal); live();
    return { disposition: { action: 'continue' }, context: [], notifications: [], session_operation: { action: 'continue' } };
  }), signal);
}
