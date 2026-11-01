import { bounded, fields, invalid, plainJSON, unsupported } from './errors.mjs';

const runtimes = new WeakMap();
export function providerState(runtime) {
  let state = runtimes.get(runtime);
  if (!state) {
    state = { desired: new Map(), committed: new Map(), registered: new Map(), nativeRegistered: new Map(), jobs: new Map(), revisions: new Map(), tail: Promise.resolve() };
    runtimes.set(runtime, state);
  }
  return state;
}
export function hasCustomProvider(runtime, id) {
  return !!providerState(runtime).desired.get(id)?.custom;
}
export function customProviderModels(runtime, id) {
  const entries = id === undefined ? [...providerState(runtime).desired.values()] : [providerState(runtime).desired.get(id)];
  return entries.flatMap(entry => entry?.custom ? entry.models.map(model => structuredClone(model.pi)) : []);
}
export function snapshotCustomProvider(provider, factory, declare) {
  if (provider.refreshModels !== undefined && typeof provider.refreshModels !== 'function') invalid('provider refreshModels');
  const models = provider.getModels();
  if (!Array.isArray(models) || models.length > 256) invalid('native provider models');
  const snapshots = models.map(model => {
    plainJSON(model, 'native provider model', 65536, { omitUndefined: true });
    const copy = structuredClone(model);
    if (copy.provider !== provider.id) invalid('native model provider does not match registration');
    return copy;
  });
  const entries = snapshots.map(model => {
    const projected = Object.fromEntries(['id', 'name', 'api', 'reasoning', 'input', 'cost', 'contextWindow', 'maxTokens'].filter(key => model[key] !== undefined).map(key => [key, model[key]]));
    const entry = declare(provider.id, { baseUrl: model.baseUrl ?? provider.baseUrl, api: model.api, models: [projected], streamSimple: provider.streamSimple.bind(provider) }, factory, true);
    return { pi: { ...model, baseUrl: model.baseUrl ?? provider.baseUrl ?? '' }, wire: entry.models[0].wire };
  });
  if (new Set(entries.map(entry => entry.pi.id)).size !== entries.length) invalid('duplicate provider model id');
  return { name: provider.id, factory, config: {}, custom: provider, native: true, stream: provider.streamSimple.bind(provider), models: entries,
    wire: { provider: { id: provider.id, label: provider.id, auth: { kind: 'none' } }, models: entries.map(entry => entry.wire) } };
}
export function abortProviderEntry(entry) {
  entry?.controller?.abort();
}
export function startCustomProviderRefresh(runtime, entry, rebuild, enqueue) {
  if (!runtime.initialized || !entry.custom?.refreshModels || entry.refreshStarted || entry.controller.signal.aborted) return;
  entry.refreshStarted = true;
  const state = providerState(runtime), signal = entry.controller.signal;
  const active = () => !runtime.stopping && !signal.aborted && state.desired.get(entry.name) === entry && state.committed.get(entry.name) === entry;
  const publish = publication => enqueue(async () => {
    if (!active()) return false;
    fields(publication, ['persist', 'update'], 'provider model publication');
    if (publication.persist !== undefined) return false;
    if (publication.update !== undefined && typeof publication.update !== 'function') invalid('provider publication update');
    const result = publication.update?.();
    if (result?.then) { result.catch?.(() => {}); invalid('provider publication update must be synchronous'); }
    const next = rebuild();
    if (!active()) return false;
    await runtime.transport.request('providers/update', next.wire);
    if (!active()) return false;
    entry.models = next.models;
    entry.wire = next.wire;
    return true;
  });
  Promise.resolve().then(() => {
    if (!active()) return;
    return entry.custom.refreshModels({ allowNetwork: false, credential: undefined, stored: undefined, force: undefined, signal, publish });
  }).catch(() => { if (active()) runtime.backgroundError(new Error('custom provider offline model refresh failed')); });
}
async function abortable(work, signal) {
  signal.throwIfAborted();
  let onAbort;
  const aborted = new Promise((_, reject) => { onAbort = () => reject(signal.reason); signal.addEventListener('abort', onAbort, { once: true }); });
  try { return await Promise.race([Promise.resolve().then(work), aborted]); }
  finally { signal.removeEventListener('abort', onAbort); }
}
export async function resolveCustomProviderAuth(entry, options, signal) {
  signal.throwIfAborted();
  const apiKey = entry.custom.auth.apiKey;
  if (!apiKey) return undefined;
  if (typeof apiKey.resolve !== 'function') invalid('custom provider API key resolver');
  if (options.apiKey !== undefined) bounded(options.apiKey, 'explicit provider key', 65536, { controls: true });
  const env = options.env === undefined ? {} : plainJSON(options.env, 'custom provider environment', 16384);
  const credential = options.apiKey === undefined ? undefined : { type: 'api_key', key: options.apiKey, ...(options.env === undefined ? {} : { env }) };
  const ctx = { env: async name => {
    bounded(name, 'environment variable name', 256);
    signal.throwIfAborted();
    return env[name] ?? process.env[name];
  }, fileExists() { unsupported('custom provider auth file access', 'only explicit and process-local environment inputs are available'); } };
  const result = await abortable(() => apiKey.resolve({ ctx, credential, signal }), signal);
  signal.throwIfAborted();
  if (result === undefined) return undefined;
  fields(result, ['auth', 'env', 'source'], 'custom provider auth result');
  fields(result.auth, ['apiKey', 'headers', 'baseUrl'], 'custom provider request auth');
  if (result.auth.apiKey !== undefined) bounded(result.auth.apiKey, 'custom provider key', 65536, { controls: true });
  if (result.auth.headers !== undefined) plainJSON(result.auth.headers, 'custom provider headers', 16384);
  if (result.auth.baseUrl !== undefined) bounded(result.auth.baseUrl, 'custom provider baseUrl', 8192);
  if (result.env !== undefined) plainJSON(result.env, 'custom provider environment', 16384);
  return structuredClone(result);
}
export async function getCustomProviderAuth(runtime, id, options = {}) {
  const entry = providerState(runtime).desired.get(id);
  if (!entry?.custom) return undefined;
  if (!runtime.initialized || runtime.stopping) unsupported('custom provider auth', 'an admitted invocation is required');
  const store = runtime.current();
  const signal = AbortSignal.any([store.controller.signal, entry.controller.signal, ...(options.signal ? [options.signal] : [])]);
  return resolveCustomProviderAuth(entry, options, signal);
}
export async function getCustomProviderApiKey(runtime, id, options = {}) {
  return (await getCustomProviderAuth(runtime, id, options))?.auth.apiKey;
}
