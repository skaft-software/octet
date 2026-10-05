// Selection is published only from an authoritative Rust-host receipt.
import { bounded, fields, invalid } from './errors.mjs';

const levels = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
function publish(store, result) {
  fields(result, ['selected', 'model_view', 'reasoning'], 'model selection receipt');
  if (typeof result.selected !== 'boolean') invalid('model selection receipt');
  if (!result.selected) return false;
  if (!levels.has(result.reasoning) || !result.model_view) invalid('model selection receipt');
  bounded(result.model_view.id, 'selected model id', 256);
  bounded(result.model_view.provider, 'selected provider', 256);
  store.state.host.model_view = structuredClone(result.model_view);
  store.state.host.reasoning = result.reasoning;
  return true;
}

export function setModel(runtime, store, model) {
  runtime.require('session_control_v1'); runtime.assertOwner(store);
  if (!model || typeof model !== 'object') invalid('model');
  bounded(model.provider, 'provider', 256); bounded(model.id, 'model id', 256);
  return runtime.track(runtime.hostCall('model/select', {
    resource_owner: store.state.owner,
    selection: { operation: 'model', provider: model.provider, id: model.id },
  }, store), store).then(result => publish(store, result));
}

export function setThinkingLevel(runtime, store, level) {
  runtime.require('session_control_v1'); runtime.assertOwner(store);
  if (!levels.has(level)) invalid('thinking level');
  store.controller.signal.throwIfAborted();
  if (runtime.stopping || !store.live || runtime.active.get(store.id)?.controller !== store.controller) invalid('active parent required for synchronous thinking selection');
  const result = runtime.transport.requestSync('model/select', {
    parent_request_id: store.id, resource_owner: store.state.owner,
    selection: { operation: 'thinking', level },
  }, { parent: store.id, signal: store.controller.signal });
  if (!publish(store, result)) invalid('thinking selection refused');
}
