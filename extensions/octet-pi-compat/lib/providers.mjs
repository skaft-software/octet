// Public Pi model facts. Rust remains the authority for selection and credentials.
import { strict, unsupported } from './errors.mjs';

const levels = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);
function snapshot(host, key, api) {
  const value = host.pi_models?.[key];
  if (value === undefined || value === null) unsupported(api, `${key} snapshot not supplied by the host`);
  return value;
}

export function thinkingLevel(host, optional = false) {
  // Only a portable Pi level is a ThinkingLevel. Native On, Ultra and arbitrary
  // token budgets are not Pi levels; never expose Debug strings as API values.
  if (levels.has(host.reasoning)) return host.reasoning;
  if (optional) return undefined;
  unsupported('pi.getThinkingLevel', 'the current native reasoning selection has no portable Pi thinking level');
}

export function modelView(view) {
  if (!view) return undefined;
  return {
    id: view.id, ...(view.name ? { name: view.name } : {}), api: view.api,
    provider: view.provider, reasoning: view.reasoning, input: [...view.input],
    contextWindow: view.context_window, maxTokens: view.max_tokens,
    ...(view.cost ? { cost: { input: view.cost.input / 1e6, output: view.cost.output / 1e6,
      cacheRead: view.cost.cache_read / 1e6, cacheWrite: view.cost.cache_write / 1e6 } } : {}),
  };
}

export function currentModel(host) {
  if (host.model_view) return modelView(host.model_view);
  // The host may supply an already Pi-shaped model, but a bare canonical ID
  // cannot truthfully supply the required provider/API/capabilities.
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
