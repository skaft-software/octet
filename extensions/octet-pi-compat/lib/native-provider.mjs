import inventory from '../shims/pi-1.0.2/models.json' with { type: 'json' };
import { AsyncLocalStorage } from 'node:async_hooks';
import { isDeepStrictEqual } from 'node:util';
import { bounded, fields, invalid, plainJSON, unsupported } from './errors.mjs';
import { canonicalToPi, piToCanonical } from './provider-context.mjs';
import { createAssistantMessageEventStream, calculateCost } from './ai-public-helpers.mjs';
import { clampThinkingLevel } from '../shims/pi-1.0.2/ai/models.mjs';
import { localFunction } from '../shims/local-pi.mjs';
import { customOpenAiChatProfile, prepareCustomProviderHttp, closeCustomProviderHttp } from './custom-provider-http.mjs';

const scopeKey = Symbol.for('octet.pi.nativeProvider.scope');
const scope = globalThis[scopeKey] ??= new AsyncLocalStorage();
export const withNativeProvider = (runtime, job, work) => scope.run({ runtime, job }, work);
// Unfold a provider context exactly as the installed Pi would: normalize any
// Context fields into its transcript, then replay every system message into the
// one prompt and tool set the request carries. The host request has fields for
// those, so this is the same request Pi's own provider builds.
const piTranscript = name => localFunction('ai', 'utils/transcript.js', name);
const piNormalizeContext = piTranscript('normalizeContext'), piCurrentSystem = piTranscript('getCurrentSystemMessage');
const piSystemText = localFunction('ai', 'utils/text.js', 'getSystemMessageText');
function fromTranscript(context) {
  if (!context || typeof context !== 'object' || !Array.isArray(context.messages)) return context;
  const { messages } = piNormalizeContext(context);
  const head = piCurrentSystem(messages);
  const text = head ? piSystemText(head) : '';
  return { ...(text ? { systemPrompt: text } : {}), messages: messages.filter(message => message.role !== 'system'), ...(head?.toolsAdded?.length ? { tools: head.toolsAdded } : {}) };
}
export function nativeProviderDelegate(model, context, options = {}) {
  context = fromTranscript(context);
  const bound = scope.getStore();
  if (!bound) unsupported('native provider stream', 'an admitted provider request is required');
  const { runtime, job } = bound;
  runtime.require('provider_inheritance_v1');
  if (model.api === 'google-generative-ai') runtime.require('provider_google_generative_ai_v1');
  if (!job.entry.inherited || !isDeepStrictEqual(model, job.model.pi)) unsupported('native provider stream', 'the delegate is bound to the selected inherited model');
  if (job.entry.config.baseUrl || job.entry.apiKey !== undefined) unsupported('native provider stream', 'native endpoint and authentication remain host-owned');
  fields(options, ['signal', 'apiKey', 'maxTokens', 'temperature', 'cacheRetention', 'sessionId', 'reasoning', 'onPayload', 'onResponse', 'transformHeaders', 'timeoutMs', 'websocketConnectTimeoutMs', 'maxRetries', 'maxRetryDelayMs'], 'native provider options');
  if (options.onResponse !== undefined || options.transformHeaders !== undefined) unsupported('native provider callbacks', 'package response and header callbacks require real wire forwarding');
  if (options.signal !== job.controller.signal || options.apiKey !== job.options.apiKey || options.reasoning !== job.options.reasoning
      || options.cacheRetention !== job.options.cacheRetention || options.sessionId !== job.options.sessionId) unsupported('native provider stream', 'authentication, reasoning, cache and session authority cannot change');
  fields(context, ['systemPrompt', 'messages', 'tools'], 'native provider context');
  if (!isDeepStrictEqual(context.tools, fromTranscript(job.context).tools)) unsupported('native provider stream', 'the authoritative tool catalog cannot change');
  if (options.onPayload !== undefined && typeof options.onPayload !== 'function') invalid('native onPayload');
  return delegatedStream(runtime, job, model, context, options, false);
}
export function customProviderDelegate(model, context, options = {}) {
  context = fromTranscript(context);
  const bound = scope.getStore();
  if (!bound) unsupported('custom provider stream', 'an admitted provider request is required');
  const { runtime, job } = bound;
  runtime.require('provider_inheritance_v1');
  job.controller.signal.throwIfAborted();
  if (!job.entry.custom || job.entry.inherited || !['openai-completions', 'openai-responses'].includes(model.api)) unsupported('custom provider stream', 'the selected custom OpenAI provider is required');
  for (const key of ['id', 'provider', 'api', 'baseUrl']) if (model[key] !== job.model.pi[key]) unsupported('custom provider model', 'the selected model identity and endpoint cannot change');
  if (typeof model.baseUrl !== 'string' || !model.baseUrl) invalid('custom provider endpoint');
  fields(options, ['signal', 'apiKey', 'headers', 'env', 'fetch', 'maxTokens', 'temperature', 'cacheRetention', 'sessionId', 'reasoning', 'reasoningEffort', 'onPayload', 'onResponse', 'transformHeaders', 'timeoutMs', 'websocketConnectTimeoutMs', 'maxRetries', 'maxRetryDelayMs'], 'custom provider options');
  if (options.onResponse !== undefined || options.transformHeaders !== undefined) unsupported('custom provider callbacks', 'package response and header callbacks require real wire forwarding');
  if (options.signal !== job.controller.signal || options.cacheRetention !== job.options.cacheRetention || options.sessionId !== job.options.sessionId
      || options.temperature !== job.options.temperature || (options.reasoning !== undefined && options.reasoning !== job.options.reasoning)) unsupported('custom provider stream', 'admitted request authority cannot change');
  const clamped = job.options.reasoning ? clampThinkingLevel(job.model.pi, job.options.reasoning) : undefined;
  const effort = clamped === 'off' ? undefined : clamped;
  if (options.reasoningEffort !== undefined && options.reasoningEffort !== effort) unsupported('custom provider reasoning', 'the admitted thinking selection cannot change');
  fields(context, ['systemPrompt', 'messages', 'tools'], 'custom provider context');
  if (!isDeepStrictEqual(context.tools, fromTranscript(job.context).tools)) unsupported('custom provider tools', 'the authoritative tool catalog cannot change');
  if (options.onPayload !== undefined && typeof options.onPayload !== 'function') invalid('custom onPayload');
  if (job.delegated) unsupported('custom provider stream', 'a provider attempt can delegate only once');
  const profile = customOpenAiChatProfile(model);
  prepareCustomProviderHttp(job, model, options);
  return delegatedStream(runtime, job, model, context, options, true, profile);
}
function openaiDelegate(api, model, context, options) {
  if (model.api !== api) unsupported('OpenAI provider model API');
  return scope.getStore()?.job.entry.inherited ? nativeProviderDelegate(model, context, options) : customProviderDelegate(model, context, options);
}
export const openaiCompletionsDelegate = (model, context, options) => openaiDelegate('openai-completions', model, context, options);
export const openaiResponsesDelegate = (model, context, options) => openaiDelegate('openai-responses', model, context, options);
function delegatedStream(runtime, job, model, context, options, customHttp, customOpenaiChat) {
  if (job.delegated) unsupported('native provider stream', 'a provider attempt can delegate only once');
  job.delegated = true;
  job.onPayload = options.onPayload;
  const request = { ...job.request, system: context.systemPrompt ?? null, messages: piToCanonical(context.messages) };
  if (context.systemPrompt !== undefined) bounded(context.systemPrompt, 'provider system', 786432, { controls: true });
  if (options.maxTokens !== undefined) {
    if (!Number.isSafeInteger(options.maxTokens) || options.maxTokens <= 0 || (request.max_output_tokens != null && options.maxTokens > request.max_output_tokens)) invalid('native provider token limit');
    request.max_output_tokens = options.maxTokens;
  }
  if (options.temperature !== undefined) {
    if (!Number.isFinite(options.temperature) || options.temperature < 0 || options.temperature > 2) invalid('native provider temperature');
    request.temperature = options.temperature;
  }
  const stream = createAssistantMessageEventStream();
  job.nativeStream = stream;
  job.nativePartial = { role: 'assistant', model: model.id, provider: model.provider, api: model.api, content: [] };
  job.nativeReasoning = new Set();
  job.nativeArguments = new Map();
  (async () => {
    try {
      const result = await runtime.transport.request('provider/delegate', { stream_id: job.id, request: plainJSON(request, 'native provider request', 786432), on_payload: !!job.onPayload, ...(customHttp ? { custom_http: true, ...(customOpenaiChat ? { custom_openai_chat: customOpenaiChat } : {}) } : {}) });
      job.controller.signal.throwIfAborted();
      fields(result, ['message', 'usage', 'stop_reason', 'timestamp'], 'native provider response');
      const message = canonicalToPi([{ Assistant: result.message }], new Map(), { replay: true })[0];
      message.model = model.id;
      message.provider = model.provider;
      message.timestamp = result.timestamp;
      const usage = result.usage;
      message.usage = { input: usage.input_tokens, output: usage.output_tokens, cacheRead: usage.cache_read_tokens, cacheWrite: usage.cache_write_tokens, totalTokens: usage.total_tokens };
      if (model.cost) { message.usage.cost = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 }; calculateCost(model, message.usage); }
      const reason = { end_turn: 'stop', max_tokens: 'length', tool_use: 'toolUse', stop_sequence: 'stop', pause_turn: 'stop' }[result.stop_reason];
      if (!reason) unsupported('native provider stop reason');
      message.stopReason = reason;
      job.nativeMessage = structuredClone(message);
      if (job.nativeStarted) {
        const partial = job.nativePartial;
        if (partial.content.length !== message.content.length) invalid('native streamed content');
        for (const contentIndex of job.nativeReasoning) {
          const part = message.content[contentIndex], previous = partial.content[contentIndex];
          if (part.type !== 'thinking' || !part.thinking.startsWith(previous.thinking)) invalid('native reasoning content');
          const delta = part.thinking.slice(previous.thinking.length);
          partial.content[contentIndex] = part;
          if (delta) stream.push({ type: 'thinking_delta', contentIndex, delta, partial });
          stream.push({ type: 'thinking_end', contentIndex, content: part.thinking, partial });
        }
      } else {
      const partial = { ...message, content: [] };
      stream.push({ type: 'start', partial });
      for (const [contentIndex, part] of message.content.entries()) {
        partial.content.push(part);
        const kind = { text: 'text', thinking: 'thinking', toolCall: 'toolcall' }[part.type];
        if (!kind) unsupported('native provider response content');
        stream.push({ type: `${kind}_start`, contentIndex, partial });
        stream.push({ type: `${kind}_delta`, contentIndex, delta: kind === 'toolcall' ? JSON.stringify(part.arguments) : part.text ?? part.thinking, partial });
        stream.push({ type: `${kind}_end`, contentIndex, ...(kind === 'toolcall' ? { toolCall: part } : { content: part.text ?? part.thinking }), partial });
      }
      }
      stream.push({ type: 'done', reason, message });
      stream.end(message);
    } catch {
      const error = { role: 'assistant', content: [], api: model.api, provider: model.provider, model: model.id, stopReason: 'error', errorMessage: 'native provider attempt failed' };
      stream.push({ type: 'error', reason: 'error', error });
      stream.end(error);
    } finally {
      if (customHttp) closeCustomProviderHttp(job);
    }
  })();
  return stream;
}
function anthropicDelegate(model, context, options) {
  if (model.api !== 'anthropic-messages') unsupported('native Anthropic model API');
  return nativeProviderDelegate(model, context, options);
}
export const anthropicMessagesApi = () => ({ streamSimple: anthropicDelegate, stream: anthropicDelegate });

const facadeGetters = globalThis[Symbol.for('octet.pi.nativeProvider.facadeGetters')] ??= new WeakMap();
export const isNativeProviderFacade = provider => facadeGetters.get(provider.getModels) === provider.id;
export function nativeProviderModels() {
  const providers = new Map(Object.entries(inventory.MODELS).map(([id, models]) => {
    const getModels = () => structuredClone(Object.values(models));
    facadeGetters.set(getModels, id);
    return [id, {
      id, name: id, auth: { apiKey: { resolve() { unsupported('provider auth', 'authentication remains host-owned'); } } },
      getModels, stream: nativeProviderDelegate, streamSimple: nativeProviderDelegate,
    }];
  }));
  return {
    getProvider: id => providers.get(id),
    setProvider: provider => { providers.set(provider.id, provider); },
    getProviders: () => [...providers.values()],
    getModels: provider => provider ? providers.get(provider)?.getModels() ?? [] : [...providers.values()].flatMap(provider => provider.getModels()),
    stream: nativeProviderDelegate, streamSimple: nativeProviderDelegate,
  };
}

export function ingestNativeProviderEvent(job, params) {
  fields(params, ['stream_id', 'kind', 'payload'], 'native provider event');
  if (!job?.delegated || !job.nativeStream) unsupported('native provider event', 'no live native delegate');
  job.controller.signal.throwIfAborted();
  const partial = job.nativePartial, stream = job.nativeStream;
  const payload = plainJSON(params.payload, 'native provider event', 131072);
  if (params.kind === 'started') {
    if (job.nativeStarted) invalid('duplicate native stream start');
    job.nativeStarted = true;
    stream.push({ type: 'start', partial });
    return { accepted: true };
  }
  if (!job.nativeStarted) invalid('native event before start');
  const matched = /^(text|reasoning|tool_call)_(start|delta|args_delta|end)$/.exec(params.kind);
  if (!matched || !Number.isSafeInteger(payload.index) || payload.index < 0 || payload.index >= 1024) invalid('native content event');
  const contentIndex = payload.index, kind = { text: 'text', reasoning: 'thinking', tool_call: 'toolcall' }[matched[1]], phase = matched[2];
  if (phase === 'start') {
    if (contentIndex !== partial.content.length) invalid('native part order');
    const part = kind === 'text' ? { type: 'text', text: '' } : kind === 'thinking' ? { type: 'thinking', thinking: '' }
      : { type: 'toolCall', id: bounded(payload.id, 'native tool id', 256), name: bounded(payload.name, 'native tool name', 256), arguments: {} };
    partial.content.push(part);
    if (kind === 'thinking') job.nativeReasoning.add(contentIndex);
    if (kind === 'toolcall') job.nativeArguments.set(contentIndex, '');
    stream.push({ type: `${kind}_start`, contentIndex, partial });
  } else {
    const part = partial.content[contentIndex];
    if (!part || part.type !== {text:'text',thinking:'thinking',toolcall:'toolCall'}[kind]) invalid('native part kind');
    if (phase === 'delta' || phase === 'args_delta') {
      const delta = bounded(payload.delta, 'native delta', 131072, { controls: true });
      if (kind === 'toolcall') job.nativeArguments.set(contentIndex, bounded(job.nativeArguments.get(contentIndex) + delta, 'native tool arguments', 786432, { controls: true }));
      else { const key = kind === 'text' ? 'text' : 'thinking'; part[key] = bounded(part[key] + delta, 'native block', 786432, { controls: true }); }
      stream.push({ type: `${kind}_delta`, contentIndex, delta, partial });
    } else if (kind !== 'thinking') {
      if (kind === 'toolcall') { part.arguments = JSON.parse(job.nativeArguments.get(contentIndex) || '{}'); stream.push({ type: 'toolcall_end', contentIndex, toolCall: part, partial }); }
      else stream.push({ type: 'text_end', contentIndex, content: part.text, partial });
    }
  }
  return { accepted: true };
}
