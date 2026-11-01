import { randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { bounded, fields, invalid, ownerKey, plainJSON, rpcError, unsupported } from './errors.mjs';
import { canonicalToPi, piToCanonical } from './provider-context.mjs';
import { ingestNativeProviderEvent } from './native-provider.mjs';

const runtimes = new WeakMap();
// Runtime.current() clones its store: the request's controller is the stable,
// private invocation identity. Never retain caller-mutable usage as a receipt.
const settledUsages = new WeakMap();
const efforts = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
const MAX_BYTES = 1024 * 1024;
class RegistryStream {
  queue = [];
  waiting = [];
  bytes = 0;
  done = false;
  closed = false;
  constructor(cancel) {
    this.cancel = cancel;
    this.final = new Promise(resolve => { this.resolve = resolve; });
  }
  push(event, terminal = false) {
    if (this.done || this.closed) return;
    const size = Buffer.byteLength(JSON.stringify(event));
    const waiter = this.waiting.shift();
    if (waiter) waiter({ value: event, done: false });
    else {
      if (!terminal && (this.queue.length >= 1024 || size > MAX_BYTES - this.bytes)) throw rpcError(-32012, 'nested inference event queue exceeded its bound');
      this.queue.push({ event, size });
      this.bytes += size;
    }
  }
  finish(event, result) {
    if (this.done) return;
    this.push(event, true);
    this.done = true;
    this.resolve(result);
    for (const waiter of this.waiting.splice(0)) waiter({ value: undefined, done: true });
  }
  next() {
    if (this.closed) return Promise.resolve({ value: undefined, done: true });
    const item = this.queue.shift();
    if (item) { this.bytes -= item.size; return Promise.resolve({ value: item.event, done: false }); }
    if (this.done) return Promise.resolve({ value: undefined, done: true });
    if (this.waiting.length >= 64) return Promise.reject(rpcError(-32012, 'nested inference iterator waiter limit exceeded'));
    return new Promise(resolve => this.waiting.push(resolve));
  }
  return() {
    this.closed = true;
    if (!this.done) this.cancel();
    this.queue = [];
    this.bytes = 0;
    for (const waiter of this.waiting.splice(0)) waiter({ value: undefined, done: true });
    return Promise.resolve({ value: undefined, done: true });
  }
  [Symbol.asyncIterator]() { return this; }
  result() { return this.final; }
}
function jobs(runtime) {
  let state = runtimes.get(runtime);
  if (!state) { state = new Map(); runtimes.set(runtime, state); }
  return state;
}
function identity(value) {
  bounded(value, 'nested model identifier', 256);
  if (!/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(value)) invalid('nested model identifier');
  return value;
}
function request(context, options) {
  fields(context, ['messages', 'systemPrompt', 'tools'], 'nested inference context');
  if (context.tools !== undefined && (!Array.isArray(context.tools) || context.tools.length)) unsupported('nested inference tools', 'nested inference cannot grant tool execution');
  fields(options, ['signal', 'maxTokens', 'temperature', 'reasoning'], 'nested inference options');
  if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) invalid('nested inference signal');
  if (options.reasoning !== undefined && !efforts.has(options.reasoning)) invalid('nested inference reasoning');
  if (options.maxTokens !== undefined && (!Number.isSafeInteger(options.maxTokens) || options.maxTokens <= 0)) invalid('nested inference token limit');
  if (options.temperature !== undefined && (!Number.isFinite(options.temperature) || options.temperature < 0 || options.temperature > 2)) invalid('nested inference temperature');
  const messages = piToCanonical(context.messages);
  return plainJSON({ messages, ...(context.systemPrompt === undefined ? {} : { system: bounded(context.systemPrompt, 'nested system prompt', 262144, { controls: true }) }),
    ...(options.reasoning === undefined ? {} : { reasoning: options.reasoning }),
    ...(options.maxTokens === undefined ? {} : { max_output_tokens: options.maxTokens }),
    ...(options.temperature === undefined ? {} : { temperature: options.temperature }) }, 'nested inference request', 262144);
}
function message(job, result) {
  fields(result, ['message', 'usage', 'cost', 'stop_reason', 'timestamp'], 'nested inference result');
  const output = canonicalToPi([{ Assistant: result.message }], new Map(), { replay: true })[0];
  if (!job.nativeStarted || output.content.length !== job.nativePartial.content.length) invalid('nested inference streamed content mismatch');
  for (const part of job.parts.values()) if (part.open) invalid('nested inference unfinished part');
  for (let index = 0; index < output.content.length; index++) {
    const final = output.content[index], partial = job.nativePartial.content[index];
    if (final.type !== partial.type || (final.text ?? final.thinking) !== (partial.text ?? partial.thinking)) invalid('nested inference final content mismatch');
    if (final.type === 'toolCall') unsupported('nested inference tool result');
    if (job.nativeReasoning.has(index)) job.nativeStream.push({ type: 'thinking_end', contentIndex: index, content: final.thinking, partial: output });
  }
  const count = value => { if (!Number.isSafeInteger(value) || value < 0) invalid('nested inference usage'); return value; };
  const usage = result.usage;
  fields(usage, ['input_tokens', 'output_tokens', 'cache_read_tokens', 'cache_write_tokens', 'cache_write_1h_tokens', 'reasoning_tokens', 'total_tokens'], 'nested inference usage');
  for (const value of Object.values(usage)) count(value);
  if (count(usage.reasoning_tokens) > count(usage.output_tokens) || count(usage.cache_write_1h_tokens) > count(usage.cache_write_tokens)) invalid('nested inference usage subsets');
  output.usage = { input: count(usage.input_tokens), output: usage.output_tokens, cacheRead: count(usage.cache_read_tokens), cacheWrite: usage.cache_write_tokens, totalTokens: count(usage.total_tokens) };
  if (usage.reasoning_tokens) output.usage.reasoning = usage.reasoning_tokens;
  if (usage.cache_write_1h_tokens) output.usage.cacheWrite1h = usage.cache_write_1h_tokens;
  if (result.cost != null) {
    const cost = result.cost;
    fields(cost, ['input', 'output', 'reasoning', 'cache_read', 'cache_write', 'total', 'total_picodollars_remainder'], 'nested inference cost');
    for (const value of Object.values(cost)) count(value);
    if (count(cost.total_picodollars_remainder) >= 1e6) invalid('nested inference cost remainder');
    output.usage.cost = { input: count(cost.input) / 1e6, output: count(cost.output + cost.reasoning) / 1e6,
      cacheRead: count(cost.cache_read) / 1e6, cacheWrite: count(cost.cache_write) / 1e6,
      total: count(cost.total) / 1e6 + cost.total_picodollars_remainder / 1e12 };
  }
  output.stopReason = { end_turn: 'stop', stop_sequence: 'stop', pause_turn: 'stop', max_tokens: 'length', refusal: 'error' }[result.stop_reason];
  if (!output.stopReason) unsupported('nested inference stop reason');
  if (output.stopReason === 'error') output.errorMessage = 'The model refused to complete the request';
  output.model = job.model.id;
  output.provider = job.model.provider;
  output.api = job.model.api ?? output.api;
  output.timestamp = count(result.timestamp);
  return output;
}
export function consumeRegistryUsageEcho(runtime, store, usage) {
  const receipts = settledUsages.get(runtime), receipt = receipts?.get(store.controller);
  if (!receipt || runtime.stopping || !store.live || store.method !== 'tool/call'
    || store.controller.signal.aborted || runtime.active.get(store.id)?.controller !== store.controller
    || receipt.id !== store.id || receipt.state !== store.state || receipt.owner !== ownerKey(store.state.owner)
    || !isDeepStrictEqual(usage, receipt.usage)) unsupported('tool result usage', 'only an exact settled native inference echo from this live invocation is supported');
  runtime.assertOwner(store);
  receipts.delete(store.controller);
}
export function streamRegistryModel(runtime, model, context, options = {}) {
  let job, launched = false, cancelled = false;
  const cancel = () => {
    cancelled = true;
    if (launched && !job.cancelSent) {
      job.cancelSent = true;
      runtime.transport.notify('provider/infer/cancel', { id: job.requestId }).catch(error => runtime.backgroundError(error));
    }
  };
  const stream = new RegistryStream(cancel);
  const fail = aborted => {
    const error = { role: 'assistant', content: [], ...(job ? { provider: job.model.provider, model: job.model.id } : {}), stopReason: aborted ? 'aborted' : 'error', errorMessage: aborted ? 'Nested inference cancelled' : 'Nested inference unavailable or failed' };
    stream.finish({ type: 'error', reason: error.stopReason, error }, error);
  };
  let store, external;
  try {
    runtime.require('nested_inference_v1');
    store = runtime.current();
    runtime.assertOwner(store);
    store.controller.signal.throwIfAborted();
    if (store.method !== 'tool/call') unsupported('nested inference parent', 'a live model-tool parent is required');
    const selected = { provider: identity(model?.provider), id: identity(model?.id) };
    const input = request(context, options);
    external = options.signal;
    if (external?.aborted) { fail(true); return stream; }
    if (jobs(runtime).size >= 8) throw rpcError(-32012, 'nested inference process limit exceeded');
    job = { id: randomUUID(), model: selected, controller: store.controller, store, owner: ownerKey(store.state.owner), delegated: true, sequence: 0, nativeStream: stream,
      nativePartial: { role: 'assistant', provider: selected.provider, model: selected.id, content: [] }, nativeReasoning: new Set(), nativeArguments: new Map(), parts: new Map() };
    jobs(runtime).set(job.id, job);
    external?.addEventListener('abort', cancel, { once: true });
    store.controller.signal.addEventListener('abort', cancel, { once: true });
    const work = runtime.hostCall('provider/infer', { inference_id: job.id, model: selected, request: input }, store, { signal: new AbortController().signal });
    job.requestId = `pi:${runtime.transport.childId}`;
    launched = true;
    runtime.track((async () => {
      try {
        const result = await work;
        if (job.failed) { fail(false); return; }
        if (cancelled || result?.cancelled === true || store.controller.signal.aborted) { fail(true); return; }
        const output = message(job, plainJSON(result, 'nested inference response', MAX_BYTES));
        let receipts = settledUsages.get(runtime);
        if (!receipts) { receipts = new WeakMap(); settledUsages.set(runtime, receipts); }
        receipts.set(store.controller, { id: store.id, state: store.state, owner: job.owner, usage: structuredClone(output.usage) });
        stream.finish({ type: 'done', reason: output.stopReason, message: output }, output);
      } catch (error) { fail(error?.code === -32800 || store.controller.signal.aborted || cancelled); }
      finally { jobs(runtime).delete(job.id); external?.removeEventListener('abort', cancel); store.controller.signal.removeEventListener('abort', cancel); }
    })(), store);
  } catch {
    if (job) jobs(runtime).delete(job.id);
    external?.removeEventListener('abort', cancel);
    store?.controller.signal.removeEventListener('abort', cancel);
    fail(store?.controller.signal.aborted ?? false);
  }
  return stream;
}
export function registryInferenceEvent(runtime, params) {
  fields(params, ['inference_id', 'sequence', 'kind', 'payload'], 'nested inference event');
  const job = jobs(runtime).get(params.inference_id);
  if (!job) unsupported('nested inference event', 'no live matching inference');
  runtime.assertOwner(job.store);
  try {
    if (params.sequence !== job.sequence++) invalid('nested inference event sequence');
    const bytes = Buffer.byteLength(JSON.stringify(params));
    job.eventBytes = (job.eventBytes ?? 0) + bytes;
    if (job.eventBytes > MAX_BYTES || job.sequence > 4096) throw rpcError(-32012, 'nested inference cumulative output limit exceeded');
    if (params.kind.startsWith('tool_call')) unsupported('nested inference tool output');
    const block = /^(text|reasoning)_(start|delta|end)$/.exec(params.kind);
    if (block) {
      const [, kind, phase] = block, index = params.payload?.index, previous = job.parts.get(index);
      if (phase === 'start') {
        if (previous) invalid('nested inference part state');
        job.parts.set(index, { kind, open: true });
      } else {
        if (!previous?.open || previous.kind !== kind) invalid('nested inference part state');
        if (phase === 'end') previous.open = false;
      }
    }
    return ingestNativeProviderEvent(job, { stream_id: job.id, kind: params.kind, payload: params.payload });
  } catch (error) {
    job.failed = true;
    if (!job.cancelSent) {
      job.cancelSent = true;
      runtime.transport.notify('provider/infer/cancel', { id: job.requestId }).catch(error => runtime.backgroundError(error));
    }
    throw error;
  }
}
