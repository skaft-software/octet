// Selection is published only from an authoritative Rust-host receipt.
import { bounded, fields, invalid } from './errors.mjs';

const levels = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
function publish(store, result) {
  fields(result, ['selected', 'queued', 'model_view', 'reasoning', 'context_usage'], 'model selection receipt');
  if (typeof result.selected !== 'boolean') invalid('model selection receipt');
  if (Object.hasOwn(result, 'queued')) {
    if (result.queued !== true || result.selected !== true || Object.keys(result).length !== 2) invalid('queued model selection receipt');
    // Admission is not application. Retained getters follow later host snapshots.
    return true;
  }
  if (!result.selected) return false;
  if (!levels.has(result.reasoning) || !result.model_view) invalid('model selection receipt');
  bounded(result.model_view.id, 'selected model id', 256);
  bounded(result.model_view.provider, 'selected provider', 256);
  if (!Object.hasOwn(result, 'context_usage')) invalid('model selection receipt requires context_usage');
  const usage = result.context_usage;
  if (usage !== null) {
    fields(usage, ['tokens', 'contextWindow', 'percent'], 'context usage receipt');
    if (!Number.isSafeInteger(usage.tokens) || usage.tokens < 0
        || !Number.isSafeInteger(usage.contextWindow) || usage.contextWindow <= 0
        || !Number.isFinite(usage.percent) || usage.percent < 0
        || usage.contextWindow !== result.model_view.context_window) invalid('context usage receipt');
  }
  const modelView = structuredClone(result.model_view), contextUsage = structuredClone(usage);
  store.state.host.model = modelView.id;
  store.state.host.model_view = modelView;
  store.state.host.reasoning = result.reasoning;
  store.state.host.context_usage = contextUsage;
  return true;
}

export function setModel(runtime, store, model) {
  runtime.require('session_control_v1'); runtime.assertOwner(store);
  if (!model || typeof model !== 'object') invalid('model');
  bounded(model.provider, 'provider', 256); bounded(model.id, 'model id', 256);
  return runtime.track(runtime.hostCall('model/select', {
    resource_owner: store.state.owner,
    selection: { operation: 'model', provider: model.provider, id: model.id },
  }, store).then(result => {
    store.controller.signal.throwIfAborted(); runtime.assertOwner(store);
    return publish(store, result);
  }), store);
}

export function setThinkingLevel(runtime, store, level) {
  runtime.require('session_control_v1'); runtime.assertOwner(store);
  if (!levels.has(level)) invalid('thinking level');
  store.controller.signal.throwIfAborted();
  if (runtime.stopping || !Number.isSafeInteger(store.id) || store.id < 0) invalid('owner-bound thinking selection required');
  const live = store.live && runtime.active.get(store.id)?.controller === store.controller;
  const result = runtime.transport.requestSync('model/select', {
    parent_request_id: store.id, resource_owner: store.state.owner,
    selection: { operation: 'thinking', level },
  }, { parent: live ? store.id : undefined, signal: store.controller.signal });
  if (!publish(store, result)) invalid('thinking selection refused');
}
