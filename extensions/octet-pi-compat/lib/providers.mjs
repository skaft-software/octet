// Public Pi model facts and process-owned custom streams. Native registry,
// route authorization, selection and stream assembly remain authoritative.
import { bounded, fields, invalid, plainJSON, rpcError, strict, unsupported, unsupportedAPI } from './errors.mjs';
import { canonicalToPi } from './provider-context.mjs';
import { providerPricing } from './provider-pricing.mjs';
import { streamRegistryModel } from './model-registry-stream.mjs';
import { nativeProviderDelegate, withNativeProvider, ingestNativeProviderEvent, isNativeProviderFacade } from './native-provider.mjs';
import { providerState, snapshotCustomProvider, abortProviderEntry, startCustomProviderRefresh, resolveCustomProviderAuth, hasCustomProvider, customProviderModels, getCustomProviderApiKey, getCustomProviderAuth } from './custom-provider.mjs';
import { openCustomProviderHttp, pullCustomProviderHttp, closeCustomProviderHttp } from './custom-provider-http.mjs';
import { isDeepStrictEqual } from 'node:util';
import { normalizeContext } from '../shims/pi-1.0.2/ai/transcript.mjs';
import { performance } from 'node:perf_hooks';
import { traceEnabled, tracePhase } from './startup-trace.mjs';
import { readPiSettings } from './pi-setup.mjs';
import { getSupportedThinkingLevels } from '../shims/pi-1.0.2/ai/models.mjs';
import { createHash } from 'node:crypto';

const levels = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
const protocols = { 'openai-completions': 'openai_chat', 'openai-responses': 'openai_responses', 'anthropic-messages': 'anthropic_messages', 'google-generative-ai': 'google_generative_ai' };
const needsGoogle = entry => entry?.google || entry?.config.api === 'google-generative-ai' || entry?.models?.some(model => model.pi.api === 'google-generative-ai');
const providers = providerState;
// Loading is sequential and no providers have been committed yet. Restore the
// preceding declarations, including ones a failing factory replaced or removed.
export function checkpointFactoryProviders(runtime) {
  const before = new Map(providers(runtime).desired), registered = new Map(providers(runtime).registered), nativeRegistered = new Map(providers(runtime).nativeRegistered);
  return () => {
    for (const entry of providers(runtime).desired.values()) if (before.get(entry.name) !== entry) abortProviderEntry(entry);
    for (const entry of before.values()) if (entry.controller?.signal.aborted) entry.controller = new AbortController();
    providers(runtime).desired = before; providers(runtime).registered = registered; providers(runtime).nativeRegistered = nativeRegistered;
  };
}
function snapshot(host, key, api) {
  const value = host.pi_models?.[key];
  if (value === undefined || value === null) unsupportedAPI(api, `${key} snapshot not supplied by the host`);
  return value;
}
export function thinkingLevel(host, optional = false) {
  if (levels.has(host.reasoning)) return host.reasoning;
  if (optional) return undefined;
  unsupportedAPI('pi.getThinkingLevel', 'the current native reasoning selection has no portable Pi thinking level');
}
// An extension-streamed model's own Pi `api` name (e.g. 'claude-bridge'); the
// host labels such routes 'extension-stream' and the adapter restores the name.
const extensionStreamApis = new Map();
const extensionStreamKey = (provider, id) => JSON.stringify([provider, id]);
export function modelView(view, runtime) {
  if (!view) return undefined;
  const declared = runtime && providers(runtime).committed.get(view.provider)?.models?.find(model => model.pi.id === view.id)?.pi;
  const api = view.api === 'extension-stream' ? extensionStreamApis.get(extensionStreamKey(view.provider, view.id)) ?? view.api : view.api;
  return {
    id: view.id, ...(view.name ? { name: view.name } : {}), api,
    provider: view.provider, reasoning: view.reasoning, input: [...view.input],
    ...(declared?.thinkingLevelMap === undefined ? {} : { thinkingLevelMap: structuredClone(declared.thinkingLevelMap) }),
    ...(view.base_url === undefined ? {} : { baseUrl: view.base_url }),
    contextWindow: view.context_window, maxTokens: view.max_tokens,
    ...(view.cost ? { cost: { input: view.cost.input / 1e6, output: view.cost.output / 1e6,
      cacheRead: view.cost.cache_read / 1e6, cacheWrite: view.cost.cache_write / 1e6 } } : {}),
  };
}
export function currentModel(host, runtime) {
  if (host.model_view) {
    const current = host.model_view;
    const view = host.pi_models?.available_models?.find(model => model.provider === current.provider && model.id === current.id);
    return modelView(view ?? current, runtime);
  }
  if (host.model_info) return structuredClone(host.model_info);
  return typeof host.model === 'object' && host.model !== null ? structuredClone(host.model) : undefined;
}
export function scopedModels(host, runtime) {
  return snapshot(host, 'scoped_models', 'ctx.scopedModels').map(entry => ({
    model: modelView(entry.model, runtime), ...(entry.thinkingLevel === undefined ? {} : { thinkingLevel: entry.thinkingLevel }),
  }));
}
export function modelRegistry(getHost, runtime = {}) {
  const all = () => [...snapshot(getHost(), 'all_models', 'ctx.modelRegistry.getAll')
    .filter(model => !hasCustomProvider(runtime, model.provider)).map(model => modelView(model, runtime)), ...customProviderModels(runtime)];
  const explicitProvider = provider => {
    const entry = providers(runtime).desired.get(provider);
    if (entry?.apiKey === undefined) return undefined;
    if (!runtime.initialized || runtime.stopping) unsupported('registered provider auth', 'an admitted invocation is required');
    runtime.current().controller.signal.throwIfAborted();
    entry.controller.signal.throwIfAborted();
    return entry;
  };
  const registry = {
    getRegisteredProviderIds: () => [...new Set([...providers(runtime).registered.keys(), ...providers(runtime).nativeRegistered.keys()])],
    getRegisteredProviderConfig: name => providers(runtime).registered.get(name),
    getRegisteredNativeProvider: name => providers(runtime).nativeRegistered.get(name),
    getAvailable: () => snapshot(getHost(), 'available_models', 'ctx.modelRegistry.getAvailable').flatMap(model => {
      if (!hasCustomProvider(runtime, model.provider)) return [modelView(model, runtime)];
      return customProviderModels(runtime, model.provider).filter(local => local.id === model.id);
    }),
    getAll: () => all(),
    find: (provider, id) => all().find(model => model.provider === provider && model.id === id),
    streamSimple: (model, context, options) => streamRegistryModel(runtime, model, context, options),
    // Pi: whether the model's provider authenticates with a subscription login.
    // The host states it, from the provider's own authentication resolver, for
    // every model it describes. A model it did not describe, or one whose
    // resolver does not classify itself, is refused rather than guessed.
    isUsingOAuth(model) {
      if (!model || typeof model !== 'object') invalid('modelRegistry model');
      const host = getHost();
      const views = [host.model_view, ...(host.pi_models?.available_models ?? []), ...(host.pi_models?.all_models ?? [])];
      const stated = views.find(view => view && view.provider === model.provider && view.id === model.id && typeof view.oauth === 'boolean');
      if (!stated) unsupported('ctx.modelRegistry.isUsingOAuth', 'the host did not state whether this provider uses OAuth');
      return stated.oauth;
    },
    hasConfiguredAuth(model) {
      if (!model || typeof model !== 'object') invalid('modelRegistry model');
      const configured = snapshot(getHost(), 'configured_auth', 'ctx.modelRegistry.hasConfiguredAuth');
      return configured[model.provider] === true;
    },
    async getApiKeyForProvider(provider) {
      try {
        identifier(provider, 'provider id');
        if (hasCustomProvider(runtime, provider)) return await getCustomProviderApiKey(runtime, provider);
        const explicit = explicitProvider(provider);
        if (explicit) return explicit.apiKey;
        runtime.require('provider_credentials');
        const host = getHost();
        const candidates = snapshot(host, 'available_models', 'ctx.modelRegistry.getApiKeyForProvider');
        const model = candidates.find(model => model.provider === provider && model.id === host.model_view?.id)
          ?? candidates.find(model => model.provider === provider);
        if (!model) return undefined;
        const result = await registry.getApiKeyAndHeaders(modelView(model, runtime));
        return result.ok ? result.apiKey : undefined;
      } catch {
        return undefined;
      }
    },
    async getApiKeyAndHeaders(model) {
      if (!model || typeof model !== 'object') invalid('modelRegistry model');
      const provider = identifier(model.provider, 'provider id'), id = identifier(model.id, 'model id');
      if (hasCustomProvider(runtime, provider)) {
        try {
          const local = customProviderModels(runtime, provider).find(value => value.id === id);
          if (!local) return { ok: false, error: 'Custom provider model unavailable' };
          const resolution = await getCustomProviderAuth(runtime, provider);
          if (!resolution) return { ok: false, error: 'Custom provider authentication unavailable' };
          return { ok: true, ...resolution.auth, headers: { ...resolution.auth.headers, ...local.headers },
            ...(resolution.env === undefined ? {} : { env: resolution.env }) };
        } catch {
          return { ok: false, error: 'Custom provider authentication unavailable' };
        }
      }
      const explicit = explicitProvider(provider);
      if (explicit) {
        if (!explicit.models?.some(model => model.pi.id === id)) return { ok: false, error: 'Registered provider model unavailable' };
        return { ok: true, apiKey: explicit.apiKey, headers: {} };
      }
      runtime.require('provider_credentials');
      const store = runtime.current();
      const result = await runtime.hostCall('provider/credentials', { provider, model: id }, store);
      fields(result, ['ok', 'apiKey', 'headers', 'baseUrl', 'env', 'error'], 'provider credentials');
      if (typeof result.ok !== 'boolean') invalid('provider credentials result');
      if (!result.ok) return { ok: false, error: bounded(result.error, 'provider credential error', 512) };
      if (result.apiKey !== undefined) bounded(result.apiKey, 'provider credential', 65536, { controls: true });
      if (result.headers !== undefined) plainJSON(result.headers, 'provider credential headers', 16384);
      if (result.baseUrl !== undefined) bounded(result.baseUrl, 'provider baseUrl', 8192);
      if (result.env !== undefined) plainJSON(result.env, 'provider credential environment', 16384);
      return { ok: true, ...(result.apiKey === undefined ? {} : { apiKey: result.apiKey }),
        ...(result.headers === undefined ? {} : { headers: result.headers }),
        ...(result.baseUrl === undefined ? {} : { baseUrl: result.baseUrl }),
        ...(result.env === undefined ? {} : { env: result.env }) };
    },
    // Pi 1.4's classifier API (a credentialed provider request) is outside the pinned Pi 1.0.2 surface and octet's
    // host contract. Feature probes (pi-mcp-adapter checks it on every `mcp search`) must read it as absent, the
    // "no classifier models" case, rather than throw; semantic search then reports itself unavailable.
    classify: undefined,
    findOfType: undefined,
  };
  return strict(registry, 'ctx.modelRegistry');
}
function identifier(value, label) {
  bounded(value, label, 256);
  if (!/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(value)) invalid(`${label} is not a native provider identifier`);
  return value;
}
function providerBaseUrl(value) {
  const baseUrl = bounded(value ?? '', 'provider baseUrl', 8192);
  if (!baseUrl || /^[A-Za-z][A-Za-z0-9_-]{0,63}$/.test(baseUrl)) return baseUrl;
  let url; try { url = new URL(baseUrl); } catch { invalid('provider baseUrl'); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) unsupported('provider baseUrl', 'credential/query/fragment URLs cannot enter host facts');
  return baseUrl;
}
function declaration(name, config, factory, inherited = false) {
  bounded(name, 'provider name', 64);
  if (!/^[a-z][a-z0-9_-]*$/.test(name)) unsupported('provider name', 'native provider names must be lowercase ASCII identifiers');
  fields(config, ['baseUrl', 'apiKey', 'api', 'models', 'streamSimple'], 'registerProvider config');
  if (config.streamSimple !== undefined && typeof config.streamSimple !== 'function') invalid('provider streamSimple');
  if (!inherited && config.streamSimple && !config.api) invalid('provider api is required with streamSimple');
  // A provider API Pi itself does not ship is legal in Pi when the extension
  // brings its own streamSimple: the stream is the extension's, and the host
  // carries it only as an extension-owned route.
  const ownStream = typeof config.streamSimple === 'function' && !inherited;
  const protocolFor = api => protocols[api] ?? (ownStream && typeof api === 'string' && /^[a-z][a-z0-9._-]{0,63}$/.test(api) ? 'extension_stream' : undefined);
  if (config.api !== undefined && !protocolFor(config.api)) unsupported(`provider API ${config.api}`);
  const baseUrl = providerBaseUrl(config.baseUrl);
  if (config.apiKey !== undefined) bounded(config.apiKey, 'explicit provider key', 16384, { controls: true });
  if (config.models === undefined) return { name, factory, config, stream: config.streamSimple, apiKey: config.apiKey, inherited: true };
  if (typeof config.streamSimple !== 'function') unsupported('registerProvider', 'a custom streamSimple is required for explicit models');
  if (!Array.isArray(config.models) || !config.models.length || config.models.length > 256) invalid('provider models must contain 1..256 explicit definitions');
  const ids = new Set();
  const models = config.models.map(model => {
    fields(model, ['id', 'name', 'api', 'provider', 'baseUrl', 'reasoning', 'thinkingLevelMap', 'input', 'cost', 'contextWindow', 'maxTokens'], 'provider model');
    const { thinkingLevelMap, ...definition } = model;
    model = definition;
    if (thinkingLevelMap !== undefined) {
      const map = plainJSON(thinkingLevelMap, 'model thinkingLevelMap', 4096, { omitUndefined: true });
      fields(map, [...levels], 'model thinkingLevelMap');
      for (const value of Object.values(map)) if (value !== null) bounded(value, 'model thinkingLevelMap value', 256);
      model.thinkingLevelMap = map;
    }
    const modelBaseUrl = providerBaseUrl(model.baseUrl ?? baseUrl);
    bounded(model.id, 'model id', inherited ? 256 : 64); if (!model.id) invalid('empty model id'); if (ids.has(model.id)) invalid('duplicate provider model id'); ids.add(model.id);
    bounded(model.name, 'model name', 128);
    const api = model.api ?? config.api;
    if (!protocolFor(api)) unsupported(`provider API ${api}`, 'only negotiated native provider identities or extension-owned streams are representable');
    if (!protocols[api]) extensionStreamApis.set(extensionStreamKey(name, model.id), api);
    if (typeof model.reasoning !== 'boolean') invalid('model reasoning must be boolean');
    if (!Array.isArray(model.input) || !model.input.includes('text') || model.input.length > 2 || new Set(model.input).size !== model.input.length || model.input.some(x => !['text', 'image'].includes(x))) invalid('model input must be text and optionally image');
    if (![model.contextWindow, model.maxTokens].every(x => Number.isSafeInteger(x) && x > 0) || model.maxTokens > model.contextWindow) invalid('model token limits');
    const pricing = inherited && model.cost == null ? null : providerPricing(model.cost);
    const reasoningOptions = model.reasoning && model.thinkingLevelMap !== undefined ? { values: getSupportedThinkingLevels(model), default: null } : undefined;
    if (reasoningOptions?.values.length === 0) unsupported('model thinkingLevelMap', 'no selectable reasoning level');
    return { pi: { ...plainJSON(model, 'provider model'), api, provider: name, baseUrl: modelBaseUrl }, wire: { id: `m-${createHash('sha256').update(model.id).digest('hex').slice(0, 32)}`, api_name: model.id, display_name: model.name, protocol: protocolFor(api), context_window: model.contextWindow, max_output_tokens: model.maxTokens,
      capabilities: { tools: true, parallel_tool_calls: true, structured_output: false, reasoning: model.reasoning }, pi_metadata: { base_url: modelBaseUrl, input: [...model.input], pricing,
        ...(reasoningOptions === undefined ? {} : { reasoning_options: reasoningOptions }) } } };
  });
  return { name, factory, config, stream: config.streamSimple, apiKey: config.apiKey, models,
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
  if (needsGoogle(entry)) runtime.require('provider_google_generative_ai_v1');
  if (entry.inherited) {
    runtime.require('provider_inheritance_v1');
    const result = await runtime.transport.request('providers/inherit', { provider_id: entry.name });
    fields(result, ['models'], 'inherited provider');
    if (!Array.isArray(result.models) || result.models.length > 256) invalid('inherited provider models');
    const models = result.models.filter(model => !entry.nativeModelIds || entry.nativeModelIds.has(model.id)).map(({ provider, baseUrl, ...model }) => model);
    if (models.some(model => model.api === 'google-generative-ai')) runtime.require('provider_google_generative_ai_v1');
    if (models.length) {
      const inherited = declaration(entry.name, { ...entry.config, models, streamSimple: entry.stream ?? nativeProviderDelegate }, entry.factory, true);
      entry = { ...entry, ...inherited, inherited: true, config: entry.config, native: entry.native };
      for (const model of entry.wire.models) model.pi_metadata.inherit_provider = entry.name;
    } else {
      entry = { ...entry, models: [], wire: { provider: { id: entry.name, label: entry.name, auth: { kind: 'none' } }, models: [] } };
    }
  }
  await runtime.transport.request(state.committed.has(entry.name) ? 'providers/update' : 'providers/register', entry.wire);
  for (const job of state.jobs.values()) if (job.entry.name === entry.name) job.controller.abort();
  state.committed.set(entry.name, entry);
  if (entry.inherited && state.desired.get(entry.name)?.controller === entry.controller) state.desired.set(entry.name, entry);
  refreshProvider(runtime, entry);
}
function refreshProvider(runtime, entry) {
  startCustomProviderRefresh(runtime, entry, () => snapshotCustomProvider(entry.custom, entry.factory, declaration), action => enqueue(runtime, action));
}
function localRegistration(state, name, entry) {
  state.registered.delete(name);
  state.nativeRegistered.delete(name);
  if (!entry) { state.desired.delete(name); return; }
  state.desired.set(name, entry);
  if (entry.nativeObject) state.nativeRegistered.set(name, entry.nativeObject);
  else state.registered.set(name, entry.config);
}
function mutation(runtime, factory, name, entry) {
  const state = providers(runtime);
  let store;
  if (runtime.initialized) {
    runtime.require('provider_proxy_v1');
    if (entry?.inherited) runtime.require('provider_inheritance_v1');
    if (needsGoogle(entry)) runtime.require('provider_google_generative_ai_v1');
    store = runtime.current(factory);
    store.controller.signal.throwIfAborted();
  }
  const revision = Symbol();
  state.revisions.set(name, revision);
  abortProviderEntry(state.desired.get(name));
  for (const job of state.jobs.values()) if (job.entry.name === name) job.controller.abort();
  if (entry) entry.controller = new AbortController();
  localRegistration(state, name, entry);
  if (!runtime.initialized) return;
  runtime.track(enqueue(runtime, async () => {
    try {
      if (entry) await commit(runtime, entry);
      else {
        await runtime.transport.request('providers/unregister', { provider_id: name });
        state.committed.delete(name);
      }
    } catch (error) {
      if (state.revisions.get(name) === revision) {
        abortProviderEntry(entry);
        const previous = state.committed.get(name);
        if (previous) { previous.controller = new AbortController(); previous.refreshStarted = false; }
        localRegistration(state, name, previous);
        if (previous) refreshProvider(runtime, previous);
      }
      throw error;
    }
  }), store);
}
export function registerProvider(runtime, factory, name, config) {
  const state = providers(runtime);
  if (typeof name !== 'string') {
    const provider = name;
    if (!provider || typeof provider !== 'object' || Array.isArray(provider)) invalid('native provider must be an object');
    const id = identifier(provider.id, 'native provider id');
    if (id.length > 64 || !/^[a-z][a-z0-9_-]*$/.test(id)) invalid('native provider id');
    if (typeof provider.getModels !== 'function' || typeof provider.streamSimple !== 'function' || typeof provider.stream !== 'function' || !provider.auth || typeof provider.auth !== 'object') invalid('native provider contract');
    let entry;
    if (isNativeProviderFacade(provider)) {
      const models = provider.getModels();
      if (!Array.isArray(models) || models.length > 256) invalid('native provider models');
      const nativeModelIds = new Set(models.map(model => identifier(model?.id, 'native model id')));
      entry = { name: id, factory, config: {}, stream: provider.streamSimple.bind(provider), inherited: true, native: true, nativeModelIds, google: models.some(model => model.api === 'google-generative-ai') };
    } else entry = snapshotCustomProvider(provider, factory, declaration);
    entry.nativeObject = provider;
    mutation(runtime, factory, id, entry);
    return;
  }
  fields(config, ['baseUrl', 'apiKey', 'api', 'models', 'streamSimple'], 'registerProvider config');
  if (config.streamSimple && !config.api) invalid('provider api is required with streamSimple');
  const effective = { ...state.registered.get(name) };
  for (const [key, value] of Object.entries(config)) if (value !== undefined) effective[key] = value;
  const entry = declaration(name, effective, factory);
  mutation(runtime, factory, name, entry);
}
export function unregisterProvider(runtime, factory, name) {
  identifier(name, 'provider name');
  mutation(runtime, factory, name);
}
export function validateProviderInitialization(runtime) {
  if ([...providers(runtime).desired.values()].some(needsGoogle)) runtime.require('provider_google_generative_ai_v1');
  if (providers(runtime).desired.size) runtime.require('provider_proxy_v1');
  if ([...providers(runtime).desired.values()].some(entry => entry.inherited)) runtime.require('provider_inheritance_v1');
}
export function startProviderRegistration(runtime) {
  if (!runtime.features.has('provider_proxy_v1')) return;
  // Reverse RPC starts after the initialize reply, never while the native host
  // is blocked awaiting it. Completion is the existing native startup barrier.
  enqueue(runtime, async () => {
    const state = providers(runtime);
    for (const entry of [...state.desired.values()]) {
      try { await commit(runtime, entry); }
      catch (error) {
        if (state.desired.get(entry.name)?.controller === entry.controller) {
          abortProviderEntry(entry);
          localRegistration(state, entry.name, state.committed.get(entry.name));
        }
        throw error;
      }
    }
    await runtime.transport.notify('providers/complete', {});
  }).catch(error => runtime.backgroundError(error));
}
function requestContext(request, runtime) {
  if ((request.tool_choice !== 'auto' && !(request.tool_choice === 'none' && !request.tools?.length)) || request.stop?.length || request.responses != null || request.output_format?.type !== 'text' || (request.reasoning_mode !== undefined && request.reasoning_mode !== 'standard')) unsupported('provider request', 'tool constraints, stop sequences, Responses replay and structured output are not supported by streamSimple');
  const tools = (request.tools || []).map(tool => {
    if (tool.async || tool.constrained_sampling != null) unsupported('provider tool', 'async/constrained sampling cannot be dropped');
    // A Pi extension's own tool reaches the package's stream as Pi passes it:
    // the definition the extension registered (JSON round-trip, as Pi's
    // toToolDeclaration), not the host's normalized copy of its schema.
    const own = runtime?.tools?.get(tool.name)?.definition;
    if (own) return { name: tool.name, description: own.description, parameters: JSON.parse(JSON.stringify(own.parameters)) };
    return { name: tool.name, description: tool.description, parameters: tool.parameters };
  });
  // Pi's agent hands providers a transcript context (the system prompt and tools
  // ride in a leading system message). Use the pinned pure helper, not an
  // installed runtime. Projection is O(B + M + T) time/space for payload bytes,
  // messages and tools; normalization adds only O(M) message references.
  return normalizeContext({ systemPrompt: request.system ?? undefined, messages: canonicalToPi(request.messages), ...(tools.length ? { tools } : {}) });
}
// Pure parser from Pi 1.0.2 core/http-dispatcher.ts (MIT; ../LICENSE.pi).
// O(S) time/space for a string of S characters, O(1) for a numeric value.
// Do not import its dispatcher/undici graph to parse a setting.
function parseHttpIdleTimeoutMs(value) {
  if (typeof value === 'string') {
    const trimmed = value.trim();
    if (trimmed.toLowerCase() === 'disabled') return 0;
    if (trimmed.length === 0) return undefined;
    return parseHttpIdleTimeoutMs(Number(trimmed));
  }
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return undefined;
  return Math.floor(value);
}
// The transport options Pi's own buildRequestOptions adds from the user's Pi
// settings (timeout, retries, websocket connect timeout), read the way Pi does.
function piRequestOptions(runtime, store) {
  let settings = {};
  try {
    settings = Object.hasOwn(store?.state?.host ?? {}, 'settings') ? store.state.host.settings
      : readPiSettings({ agentDir: runtime.config.pi_agent_dir, cwd: store?.state?.workspace, projectTrusted: store?.state?.host?.project_trusted === true }).settings;
  } catch {}
  const idle = parseHttpIdleTimeoutMs(settings?.httpIdleTimeoutMs) ?? 300000;
  const provider = settings?.retry?.provider ?? {};
  const websocket = parseHttpIdleTimeoutMs(settings?.websocketConnectTimeoutMs);
  return { timeoutMs: provider.timeoutMs ?? (idle === 0 ? 2147483647 : idle), websocketConnectTimeoutMs: websocket,
    maxRetries: provider.maxRetries, maxRetryDelayMs: provider.maxRetryDelayMs ?? 60000 };
}
// Pi gives providers its session id (a UUID). Octet's owner key is not one, so
// derive a stable, per-session RFC 9562 version-8 UUID from it.
export function piSessionId(id) {
  if (id == null) return undefined;
  const h = createHash('sha256').update(`octet-pi-session:${id}`).digest('hex');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-8${h.slice(13, 16)}-${(8 | (parseInt(h[16], 16) & 3)).toString(16)}${h.slice(17, 20)}-${h.slice(20, 32)}`;
}
function providerRequestCallbacks(runtime, id, controller) {
  const call = async (phase, body) => {
    controller.signal.throwIfAborted();
    const result = await runtime.transport.request(`provider/hooks/${phase}`, { stream_id: id, ...body });
    controller.signal.throwIfAborted();
    return result;
  };
  return {
    async onPayload(payload) {
      const result = await call('payload', { payload: plainJSON(payload, 'provider payload', 786432, { omitUndefined: true }) });
      fields(result, ['payload'], 'provider payload reply');
      return plainJSON(result.payload, 'provider payload', 786432);
    },
    async transformHeaders(headers) {
      const result = await call('headers', { headers: plainJSON(headers, 'provider headers', 65536) });
      fields(result, ['headers'], 'provider headers reply');
      return plainJSON(result.headers, 'provider headers', 65536);
    },
    async onResponse(response) {
      fields(response, ['status', 'headers'], 'provider response');
      const result = await call('response', { status: response.status, headers: plainJSON(response.headers, 'provider response headers', 65536) });
      if (result?.accepted !== true || Object.keys(result).length !== 1) invalid('provider response reply');
    },
  };
}
export async function prepareProviderStream(runtime, params, store) {
  runtime.require('provider_proxy_v1');
  fields(params, ['stream_id', 'provider_id', 'model_id', 'request', 'authorization_lease', 'request_hooks'], 'provider stream');
  bounded(params.stream_id, 'provider stream id', 256);
  if (params.authorization_lease != null) unsupported('provider authorization lease');
  const state = providers(runtime); await state.tail;
  const entry = state.committed.get(params.provider_id), model = entry?.models.find(model => model.wire.id === params.model_id);
  if (!model || state.jobs.has(params.stream_id) || state.jobs.size >= 8) throw rpcError(-32002, 'provider route unavailable or stream limit reached');
  if (params.request_hooks !== undefined) {
    if (params.request_hooks !== true) invalid('provider request hooks');
    runtime.require('provider_request_hooks_v1');
  }
  const preparedAt = traceEnabled ? performance.now() : 0;
  const context = requestContext(params.request, runtime), controller = new AbortController();
  // Pi's agent passes no maxTokens to provider streams; the model's own limit
  // applies inside the package's code exactly as under Pi.
  const options = { signal: controller.signal, apiKey: entry.apiKey, maxTokens: undefined,
    temperature: params.request.temperature ?? undefined, cacheRetention: params.request.cache_retention, sessionId: piSessionId(params.request.session_id),
    reasoning: undefined, onPayload: undefined, onResponse: undefined, transformHeaders: undefined, ...piRequestOptions(runtime, store),
    ...(params.request_hooks ? providerRequestCallbacks(runtime, params.stream_id, controller) : {}) };
  const reasoning = params.request.reasoning;
  if (reasoning?.type === 'effort' && levels.has(reasoning.value)) options.reasoning = reasoning.value;
  else if (reasoning?.type !== 'off') unsupported('provider reasoning', 'only portable effort controls are supported');
  state.jobs.set(params.stream_id, { id: params.stream_id, request: params.request, entry, model, context, options, controller, store, sequence: 0 });
  if (traceEnabled) tracePhase('pi-compat.provider.prepare', `took_us=${Math.round((performance.now() - preparedAt) * 1000)} messages=${params.request.messages.length} tools=${params.request.tools?.length ?? 0}`);
  return { stream_id: params.stream_id, accepted: true };
}
export function cancelProviderStream(runtime, id, owner) {
  const state = providers(runtime);
  for (const [key, job] of state.jobs) if ((id === undefined || id === key) && (owner === undefined || job.store.state === owner)) job.controller.abort();
  if (id === undefined && owner === undefined) for (const entry of state.desired.values()) abortProviderEntry(entry);
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
    let model = structuredClone(job.model.pi);
    if (job.entry.custom) {
      const signal = AbortSignal.any([job.controller.signal, job.entry.controller.signal]);
      const resolution = await resolveCustomProviderAuth(job.entry, job.options, signal);
      signal.throwIfAborted();
      if (!resolution) unsupported('custom provider auth', 'provider is not configured');
      if (resolution?.auth.baseUrl) model.baseUrl = resolution.auth.baseUrl;
      job.options = { ...job.options, apiKey: resolution?.auth.apiKey, headers: { ...resolution?.auth.headers, ...model.headers }, env: resolution?.env };
    }
    if (job.options.transformHeaders) {
      const { transformHeaders, ...options } = job.options;
      const headers = await transformHeaders(options.headers ?? {});
      job.controller.signal.throwIfAborted();
      job.options = { ...options, headers };
    }
    const callback = job.entry.inherited && !job.entry.native && job.entry.config.api !== job.model.pi.api ? nativeProviderDelegate : job.entry.stream;
    const stream = callback(model, job.context, strict(job.options, 'provider stream options'));
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
        if (event.partial?.content?.some((part, index) => (part.thinkingSignature || part.textSignature || part.redacted) && !isDeepStrictEqual(part, job.nativeMessage?.content[index]))) unsupported('provider opaque replay signatures');
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
            if (!block || ((part.thinkingSignature || part.textSignature || part.redacted) && !isDeepStrictEqual(part, job.nativeMessage?.content[index]))) unsupported('provider final content/signatures');
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
  runtime.scope.run(job.store, () => withNativeProvider(runtime, job, run)).catch(async () => {
    if (!job.controller.signal.aborted && !runtime.stopping) await emit('error', {}).catch(() => {});
  }).finally(() => { closeCustomProviderHttp(job); state.jobs.delete(id); });
}

export async function nativeProviderPayload(runtime, params) {
  runtime.require('provider_inheritance_v1');
  fields(params, ['stream_id', 'payload'], 'native provider payload');
  const job = providers(runtime).jobs.get(params.stream_id);
  if (!job?.delegated || !job.onPayload) unsupported('native provider payload', 'no live delegate callback');
  job.controller.signal.throwIfAborted();
  const payload = plainJSON(params.payload, 'native provider payload', 786432);
  const before = job.customHttp ? structuredClone(payload) : undefined;
  const result = await runtime.scope.run(job.store, () => job.onPayload(payload, structuredClone(job.model.pi)));
  job.controller.signal.throwIfAborted();
  const after = plainJSON(result ?? payload, 'native provider payload', 786432);
  if (before) {
    const editable = new Set(['prompt_cache_key', ...['messages', 'input', 'instructions', 'system'].filter(key => Object.hasOwn(before, key))]);
    for (const key of new Set([...Object.keys(before), ...Object.keys(after)])) if (!editable.has(key) && !isDeepStrictEqual(before[key], after[key])) unsupported('custom provider payload', 'encoded request authority cannot change');
    if (after.prompt_cache_key !== undefined) bounded(after.prompt_cache_key, 'custom prompt cache key', 8192);
  }
  return { payload: after };
}

export function nativeProviderEvent(runtime, params) {
  runtime.require('provider_inheritance_v1');
  return ingestNativeProviderEvent(providers(runtime).jobs.get(params.stream_id), params);
}
export async function customProviderHttp(runtime, params) {
  runtime.require('provider_inheritance_v1');
  const job = providers(runtime).jobs.get(params.stream_id);
  if (!job) unsupported('custom provider HTTP', 'no live provider stream');
  return runtime.scope.run(job.store, () => openCustomProviderHttp(job, params));
}
export async function customProviderHttpChunk(runtime, params) {
  runtime.require('provider_inheritance_v1');
  const job = providers(runtime).jobs.get(params.stream_id);
  if (!job) unsupported('custom provider HTTP pull', 'no live provider stream');
  return runtime.scope.run(job.store, () => pullCustomProviderHttp(job, params));
}
