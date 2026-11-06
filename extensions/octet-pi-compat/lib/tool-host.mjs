import { AsyncLocalStorage } from 'node:async_hooks';
import { isDeepStrictEqual } from 'node:util';
import { bounded, fields, invalid, plainJSON, unsupported } from './errors.mjs';
import { compositionReply, nativeOutcome, nativeResult } from './tools.mjs';

const scope = globalThis[Symbol.for('octet.pi.toolHost.scope')] ??= new AsyncLocalStorage();
export const withToolHost = (runtime, store, work) => scope.run({ runtime, store }, work);
export function currentToolHost() {
  const binding = scope.getStore();
  if (!binding || binding.store.method !== 'tool/call' || !binding.store.live) unsupported('tool execution', 'a live model-tool parent is required');
  const { runtime, store } = binding;
  runtime.assertOwner(store);
  store.controller.signal.throwIfAborted();
  runtime.require('tool_composition_v1');
  return binding;
}

async function request(binding, method, params, options = {}) {
  const { runtime, store } = binding;
  if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) invalid('tool signal');
  if (options.onUpdate !== undefined && typeof options.onUpdate !== 'function') invalid('tool onUpdate');
  const signal = options.signal ? AbortSignal.any([store.controller.signal, options.signal]) : store.controller.signal;
  signal.throwIfAborted();
  const reply = await runtime.hostCall(method, params, store, { signal,
    ...(options.onUpdate === undefined ? {} : { onUpdate: update => options.onUpdate(nativeResult(update)) }) });
  return compositionReply(runtime, reply, 'value');
}

export async function prepareHostTool(event, builtin, translate) {
  const binding = currentToolHost();
  binding.runtime.require('composition_preflight_v1');
  if (builtin) binding.runtime.require('composition_builtin_tools_v1');
  bounded(event.toolCallId, 'tool call id', 256);
  bounded(event.toolName, 'tool name', 128);
  const calls = binding.store.hostedToolCalls ??= new Map();
  if (calls.has(event.toolCallId)) invalid('tool call already prepared');
  if (calls.size >= 256) invalid('hosted tool call limit');
  const arguments_ = plainJSON(translate ? translate(event.input, binding.store.state.workspace) : event.input, 'tool arguments', 131072);
  const reply = await request(binding, 'composition/prepare', { name: event.toolName, arguments: arguments_, ...(builtin ? { builtin: true } : {}) });
  fields(reply, ['prepared_id', 'tool_call', 'block', 'reason', 'outcome'], 'tool preflight');
  if (reply.block === true) {
    const outcome = nativeOutcome(reply.outcome);
    calls.set(event.toolCallId, { name: event.toolName, builtin, outcome, used: true });
    return { block: true, reason: bounded(reply.reason, 'tool veto reason', 65536, { controls: true }) };
  }
  if (reply.block !== undefined || reply.outcome !== undefined || reply.reason !== undefined) invalid('tool preflight');
  bounded(reply.prepared_id, 'prepared tool receipt', 512);
  fields(reply.tool_call, ['id', 'name', 'arguments'], 'prepared tool call');
  if (reply.tool_call.name !== event.toolName || typeof reply.tool_call.id !== 'string') invalid('prepared tool identity');
  const finalArguments = plainJSON(reply.tool_call.arguments, 'prepared tool arguments', 131072);
  if (!event.input || typeof event.input !== 'object' || Array.isArray(event.input) || !finalArguments || typeof finalArguments !== 'object' || Array.isArray(finalArguments)) unsupported('tool preflight input', 'in-place Pi input requires object arguments');
  for (const key of Object.keys(event.input)) delete event.input[key];
  Object.assign(event.input, finalArguments);
  calls.set(event.toolCallId, { name: event.toolName, builtin, prepared: reply.prepared_id, arguments: finalArguments });
  return undefined;
}

export async function executeHostTool(name, args, options = {}) {
  const binding = currentToolHost();
  bounded(name, 'tool name', 128);
  const call = binding.store.hostedToolCalls?.get(options.toolCallId);
  if (call && (call.name !== name || call.used || !isDeepStrictEqual(call.arguments, args))) invalid('prepared tool receipt does not match execution');
  const builtin = call?.builtin ?? options.builtin === true;
  if (builtin) binding.runtime.require('composition_builtin_tools_v1');
  const arguments_ = call?.arguments ?? (options.translate ? options.translate(args, binding.store.state.workspace) : args);
  const params = { name, arguments: plainJSON(arguments_, 'tool arguments', 131072), full_outcome: true,
    ...(builtin ? { builtin: true } : {}), ...(call ? { prepared_id: call.prepared } : {}),
    ...(options.onUpdate === undefined ? {} : { updates: true }) };
  if (call) call.used = true;
  const reply = await request(binding, 'composition/call', params, options);
  const { builtin_output, ...canonical } = reply;
  if (builtin_output !== undefined && !builtin) invalid('native builtin output target');
  const outcome = nativeOutcome(canonical);
  if (outcome.toolCall.name !== name) invalid('native tool outcome identity');
  if (options.project) outcome.result = options.project(outcome.result, builtin_output);
  if (call) call.outcome = outcome;
  return outcome.result;
}

export function settledHostTool(event) {
  const { store } = currentToolHost();
  const call = store.hostedToolCalls?.get(event.toolCallId);
  if (!call || call.name !== event.toolName || !call.outcome || call.resultEmitted) invalid('tool result requires a completed native tool call');
  call.resultEmitted = true;
  return structuredClone(call.outcome.result);
}
