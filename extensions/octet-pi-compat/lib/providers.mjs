// Public Pi model facts and process-owned custom streams. Native registry,
// route authorization, selection and stream assembly remain authoritative.
import { bounded, fields, invalid, plainJSON, rpcError, strict, unsupported } from './errors.mjs';
import { canonicalToPi } from './provider-context.mjs';
import { isDeepStrictEqual } from 'node:util';
import { createHash } from 'node:crypto';

const levels = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
const protocols = { 'openai-completions': 'openai_chat', 'openai-responses': 'openai_responses', 'anthropic-messages': 'anthropic_messages' };
const runtimes = new WeakMap();
function providers(runtime) {
  let value = runtimes.get(runtime);
  if (!value) { value = { desired: new Map(), committed: new Map(), jobs: new Map(), tail: Promise.resolve() }; runtimes.set(runtime, value); }
  return value;
}
function snapshot(host, key, api) {
  const value = host.pi_models?.[key];
  if (value === undefined || value === null) unsupported(api, `${key} snapshot not supplied by the host`);
  return value;
}
export function thinkingLevel(host, optional = false) {
  if (levels.has(host.reasoning)) return host.reasoning;
  if (optional) return undefined;
  unsupported('pi.getThinkingLevel', 'the current native reasoning selection has no portable Pi thinking level');
}
export function modelView(view) {
  if (!view) return undefined;
  return {
    id: view.id, ...(view.name ? { name: view.name } : {}), api: view.api,
    provider: view.provider, reasoning: view.reasoning, input: [...view.input],
    ...(view.base_url === undefined ? {} : { baseUrl: view.base_url }),
    contextWindow: view.context_window, maxTokens: view.max_tokens,
    ...(view.cost ? { cost: { input: view.cost.input / 1e6, output: view.cost.output / 1e6,
      cacheRead: view.cost.cache_read / 1e6, cacheWrite: view.cost.cache_write / 1e6 } } : {}),
  };
}
export function currentModel(host) {
  if (host.model_view) {
    const current = host.model_view;
    const view = host.pi_models?.available_models?.find(model => model.provider === current.provider && model.id === current.id);
    return modelView(view ?? current);
  }
  if (host.model_info) return structuredClone(host.model_info);
  return typeof host.model === 'object' && host.model !== null ? structuredClone(host.model) : undefined;
}
export function scopedModels(host) {
  return snapshot(host, 'scoped_models', 'ctx.scopedModels').map(entry => ({
    model: modelView(entry.model), ...(entry.thinkingLevel === undefined ? {} : { thinkingLevel: entry.thinkingLevel }),
  }));
}
export function modelRegistry(getHost) {
  const all = () => snapshot(getHost(), 'all_models', 'ctx.modelRegistry.getAll');
  return strict({
    getAvailable: () => snapshot(getHost(), 'available_models', 'ctx.modelRegistry.getAvailable').map(modelView),
    getAll: () => all().map(modelView),
    find: (provider, id) => modelView(all().find(model => model.provider === provider && model.id === id)),
    isUsingOAuth(model) {
      const facts = snapshot(getHost(), 'model_auth', 'ctx.modelRegistry.isUsingOAuth');
      const key = JSON.stringify([model.provider, model.id]);
      if (!Object.hasOwn(facts, key)) unsupported('ctx.modelRegistry.isUsingOAuth', 'authentication status for this model was not supplied by the host');
      return facts[key].using_oauth;
    },
    getApiKey() { unsupported('ctx.modelRegistry.getApiKey', 'provider credentials never cross the extension boundary'); },
  }, 'ctx.modelRegistry');
}
function identifier(value, label) {
  bounded(value, label, 256);
  if (!/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(value)) invalid(`${label} is not a native provider identifier`);
  return value;
}
function rate(value) {
  const exact = value * 1e6, rounded = Math.round(exact);
  if (!Number.isFinite(value) || value < 0 || !Number.isSafeInteger(rounded) || Math.abs(exact - rounded) > 1e-7) invalid('provider cost must be exactly representable in native microdollars');
  return rounded;
}
function declaration(name, config, factory) {
  bounded(name, 'provider name', 64);
  if (!/^[a-z][a-z0-9_-]*$/.test(name)) unsupported('provider name', 'native provider names must be lowercase ASCII identifiers');
  // Deliberately bounded first unit: no built-in overrides, OAuth, environment
  // key lookup, arbitrary API codecs or silently ignored compatibility options.
  fields(config, ['baseUrl', 'apiKey', 'api', 'models', 'streamSimple'], 'registerProvider config');
  if (typeof config.streamSimple !== 'function') unsupported('registerProvider', 'a custom streamSimple is required; native HTTP overrides are not supported by this unit');
  const baseUrl = bounded(config.baseUrl ?? '', 'provider baseUrl', 8192);
  if (baseUrl) {
    let url; try { url = new URL(baseUrl); } catch { invalid('provider baseUrl'); }
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) unsupported('provider baseUrl', 'credential/query/fragment URLs cannot enter host facts');
  }
  if (config.apiKey !== undefined) bounded(config.apiKey, 'explicit provider key', 16384, { controls: true });
  if (!Array.isArray(config.models) || !config.models.length || config.models.length > 256) invalid('provider models must contain 1..256 explicit definitions');
  const ids = new Set();
  const models = config.models.map(model => {
    fields(model, ['id', 'name', 'api', 'reasoning', 'input', 'cost', 'contextWindow', 'maxTokens'], 'provider model');
    bounded(model.id, 'model id', 64); if (!model.id) invalid('empty model id'); if (ids.has(model.id)) invalid('duplicate provider model id'); ids.add(model.id);
    bounded(model.name, 'model name', 128);
    const api = model.api ?? config.api;
    if (!protocols[api]) unsupported(`provider API ${api}`, 'only native chat/responses/anthropic identities are representable');
    if (typeof model.reasoning !== 'boolean') invalid('model reasoning must be boolean');
    if (!Array.isArray(model.input) || !model.input.includes('text') || model.input.length > 2 || new Set(model.input).size !== model.input.length || model.input.some(x => !['text', 'image'].includes(x))) invalid('model input must be text and optionally image');
    if (![model.contextWindow, model.maxTokens].every(x => Number.isSafeInteger(x) && x > 0) || model.maxTokens > model.contextWindow) invalid('model token limits');
    fields(model.cost, ['input', 'output', 'cacheRead', 'cacheWrite'], 'model cost');
    const pricing = { input: rate(model.cost.input), output: rate(model.cost.output), cache_read: rate(model.cost.cacheRead), cache_write_5m: rate(model.cost.cacheWrite), cache_write_1h: null, reasoning: null, tiers: [] };
    return { pi: { ...plainJSON(model, 'provider model'), api, provider: name, baseUrl }, wire: { id: `m-${createHash('sha256').update(model.id).digest('hex').slice(0, 32)}`, api_name: model.id, display_name: model.name, protocol: protocols[api], context_window: model.contextWindow, max_output_tokens: model.maxTokens,
      capabilities: { tools: true, parallel_tool_calls: true, structured_output: false, reasoning: model.reasoning }, pi_metadata: { base_url: baseUrl, input: [...model.input], pricing } } };
  });
  return { name, factory, stream: config.streamSimple, apiKey: config.apiKey, models,
    wire: { provider: { id: name, label: name, auth: { kind: 'none' } }, models: models.map(model => model.wire) } };
}
function enqueue(runtime, action) {
  const state = providers(runtime);
  const job = state.tail.then(action);
  // A rejected mutation must not poison subsequent independent mutations.
  state.tail = job.catch(() => {});
  return job;
}
async function commit(runtime, entry) {
  const state = providers(runtime);
  await runtime.transport.request(state.committed.has(entry.name) ? 'providers/update' : 'providers/register', entry.wire);
  for (const job of state.jobs.values()) if (job.entry.name === entry.name) job.controller.abort();
  state.committed.set(entry.name, entry);
}
export function registerProvider(runtime, factory, name, config) {
  const entry = declaration(name, config, factory), state = providers(runtime);
  if (!runtime.initialized) { state.desired.set(name, entry); return; }
  runtime.require('provider_proxy_v1');
  const store = runtime.current(factory); store.controller.signal.throwIfAborted();
  runtime.track(enqueue(runtime, () => commit(runtime, entry)), store);
}
export function unregisterProvider(runtime, factory, name) {
  identifier(name, 'provider name'); const state = providers(runtime);
  if (!runtime.initialized) { state.desired.delete(name); return; }
  runtime.require('provider_proxy_v1');
  const store = runtime.current(factory); store.controller.signal.throwIfAborted();
  runtime.track(enqueue(runtime, async () => {
    await runtime.transport.request('providers/unregister', { provider_id: name });
    state.committed.delete(name);
    for (const job of state.jobs.values()) if (job.entry.name === name) job.controller.abort();
  }), store);
}
export function validateProviderInitialization(runtime) {
  if (providers(runtime).desired.size) runtime.require('provider_proxy_v1');
}
export function startProviderRegistration(runtime) {
  if (!runtime.features.has('provider_proxy_v1')) return;
  // Reverse RPC starts after the initialize reply, never while the native host
  // is blocked awaiting it. Completion is the existing native startup barrier.
  enqueue(runtime, async () => {
    for (const entry of providers(runtime).desired.values()) await commit(runtime, entry);
    await runtime.transport.notify('providers/complete', {});
  }).catch(error => runtime.backgroundError(error));
}
function requestContext(request) {
  if (request.tool_choice !== 'auto' || request.stop?.length || request.responses != null || request.output_format?.type !== 'text' || (request.reasoning_mode !== undefined && request.reasoning_mode !== 'standard')) unsupported('provider request', 'tool constraints, stop sequences, Responses replay and structured output are not supported by streamSimple');
  const tools = (request.tools || []).map(tool => {
    if (tool.async || tool.constrained_sampling != null) unsupported('provider tool', 'async/constrained sampling cannot be dropped');
    return { name: tool.name, description: tool.description, parameters: tool.parameters };
  });
  return { systemPrompt: request.system ?? undefined, messages: canonicalToPi(request.messages), ...(tools.length ? { tools } : {}) };
}
export async function prepareProviderStream(runtime, params, store) {
  runtime.require('provider_proxy_v1');
  fields(params, ['stream_id', 'provider_id', 'model_id', 'request', 'authorization_lease'], 'provider stream');
  bounded(params.stream_id, 'provider stream id', 256);
  if (params.authorization_lease != null) unsupported('provider authorization lease');
  const state = providers(runtime); await state.tail;
  const entry = state.committed.get(params.provider_id), model = entry?.models.find(model => model.wire.id === params.model_id);
  if (!model || state.jobs.has(params.stream_id) || state.jobs.size >= 8) throw rpcError(-32002, 'provider route unavailable or stream limit reached');
  const context = requestContext(params.request), controller = new AbortController();
  const options = { signal: controller.signal, apiKey: entry.apiKey, maxTokens: params.request.max_output_tokens ?? undefined,
    temperature: params.request.temperature ?? undefined, cacheRetention: params.request.cache_retention, sessionId: params.request.session_id ?? undefined };
  const reasoning = params.request.reasoning;
  if (reasoning?.type === 'effort' && levels.has(reasoning.value)) options.reasoning = reasoning.value;
  else if (reasoning?.type !== 'off') unsupported('provider reasoning', 'only portable effort controls are supported');
  state.jobs.set(params.stream_id, { entry, model, context, options, controller, store, sequence: 0 });
  return { stream_id: params.stream_id, accepted: true };
}
export function cancelProviderStream(runtime, id, owner) {
  for (const [key, job] of providers(runtime).jobs) if ((id === undefined || id === key) && (owner === undefined || job.store.state === owner)) job.controller.abort();
}
export function startProviderStream(runtime, id) {
  const state = providers(runtime), job = state.jobs.get(id); if (!job) return;
  const emit = (kind, payload) => {
    job.controller.signal.throwIfAborted();
    const value = plainJSON(payload, 'provider event', 65536);
    return runtime.transport.notify('provider/event', { stream_id: id, sequence: job.sequence++, kind, payload: value });
  };
  const run = async () => {
    job.controller.signal.throwIfAborted();
    const blocks = new Map(); let started = false, finished = false;
    const stream = job.entry.stream(structuredClone(job.model.pi), job.context, strict(job.options, 'provider stream options'));
    if (!stream?.[Symbol.asyncIterator]) invalid('streamSimple must return an async iterable');
    const iterator = stream[Symbol.asyncIterator]();
    const abort = new Promise((_, reject) => job.controller.signal.addEventListener('abort', () => reject(rpcError(-32800, 'provider stream cancelled')), { once: true }));
    // Always observe rejection, including abort after the final event.
    abort.catch(() => {});
    try {
      while (!finished) {
        job.controller.signal.throwIfAborted();
        const next = await Promise.race([iterator.next(), abort]);
        if (next.done) invalid('provider stream missing terminal event');
        const event = next.value;
        if (event.partial?.content?.some(part => part.thinkingSignature || part.textSignature || part.redacted)) unsupported('provider opaque replay signatures');
        if (event.type === 'error') throw rpcError(-32002, 'custom provider returned an error');
        if (event.type === 'start') {
          if (started) invalid('duplicate provider start'); started = true; await emit('started', {}); continue;
        }
        if (!started) invalid('provider event before start');
        if (event.type === 'done') {
          if ([...blocks.values()].some(block => !block.ended)) invalid('provider has open content blocks');
          const message = event.message;
          if (!Array.isArray(message?.content) || message.content.length !== blocks.size) invalid('provider final content differs from stream');
          for (let index = 0; index < message.content.length; index++) {
            const part = message.content[index], block = blocks.get(index);
            if (!block || part.thinkingSignature || part.textSignature || part.redacted) unsupported('provider final content/signatures');
            if (block.kind === 'toolcall' ? !isDeepStrictEqual(part, block.final) : part.type !== (block.kind === 'thinking' ? 'thinking' : 'text') || (part.text ?? part.thinking) !== block.text) invalid('provider final content differs from stream');
          }
          const usage = message.usage;
          for (const key of ['input', 'output', 'cacheRead', 'cacheWrite', 'totalTokens']) if (!Number.isSafeInteger(usage?.[key]) || usage[key] < 0) invalid('provider usage');
          const reason = { stop: 'end_turn', length: 'max_tokens', toolUse: 'tool_use' }[event.reason];
          if (!reason) unsupported(`provider stop reason ${event.reason}`);
          await emit('usage', { input_tokens: usage.input, output_tokens: usage.output, cache_read_tokens: usage.cacheRead, cache_write_tokens: usage.cacheWrite, total_tokens: usage.totalTokens });
          await emit('finished', { stop_reason: reason }); finished = true; continue;
        }
        const match = /^(text|thinking|toolcall)_(start|delta|end)$/.exec(event.type);
        if (!match || !Number.isSafeInteger(event.contentIndex) || event.contentIndex < 0 || event.contentIndex >= 1024) invalid('provider content event');
        const [, kind, phase] = match, index = event.contentIndex, native = { text: 'text', thinking: 'reasoning', toolcall: 'tool_call' }[kind];
        if (phase === 'start') {
          if (blocks.has(index)) invalid('duplicate provider content block');
          const block = { kind, text: '', ended: false }; blocks.set(index, block);
          const part = event.partial?.content?.[index];
          await emit(`${native}_start`, { index, ...(kind === 'toolcall' ? { id: identifier(part?.id, 'tool call id'), name: identifier(part?.name, 'tool name') } : {}) });
        } else {
          const block = blocks.get(index); if (!block || block.kind !== kind || block.ended) invalid('unbalanced provider content event');
          if (phase === 'delta') { const delta = bounded(event.delta, 'provider delta', 131072, { controls: true }); block.text += delta; bounded(block.text, 'provider block', 786432, { controls: true }); await emit(kind === 'toolcall' ? 'tool_call_args_delta' : `${native}_delta`, { index, delta }); }
          else {
            if (kind === 'toolcall') {
              const final = plainJSON(event.toolCall, 'provider tool call');
              const args = JSON.parse(block.text || '{}');
              if (!isDeepStrictEqual(args, final.arguments)) invalid('provider final tool arguments differ from deltas');
              block.final = final;
            } else if (event.content !== block.text) invalid('provider final text differs from deltas');
            block.ended = true; await emit(`${native}_end`, { index });
          }
        }
      }
    } finally {
      // Never await a malicious/uncooperative iterator's return on cancellation.
      Promise.resolve(iterator.return?.()).catch(() => {});
    }
  };
  runtime.scope.run(job.store, run).catch(async () => {
    if (!job.controller.signal.aborted && !runtime.stopping) await emit('error', {}).catch(() => {});
  }).finally(() => state.jobs.delete(id));
}
