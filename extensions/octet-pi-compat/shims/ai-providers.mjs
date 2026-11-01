import inventory from './pi-1.0.2/models.json' with { type: 'json' };
import { nativeProviderModels } from '../lib/native-provider.mjs';
import { fields } from '../lib/errors.mjs';
export function builtinModels(options) {
  if (options !== undefined) fields(options, [], 'builtinModels options');
  return nativeProviderModels();
}
export const builtinProviders = () => nativeProviderModels().getProviders();
export const radiusProvider = () => nativeProviderModels().getProvider('radius');
export const getBuiltinModel = (provider, modelId) => inventory.MODELS[provider]?.[modelId];
export const getBuiltinImageModel = (provider, modelId) => inventory.IMAGE_MODELS[provider]?.[modelId];
export const getBuiltinClassifierModel = (provider, modelId) => inventory.CLASSIFIER_MODELS[provider]?.[modelId];
export const getBuiltinProviders = () => Object.keys(inventory.MODELS);
export const getBuiltinModelDataGeneratedAt = () => Date.parse('2026-10-04T00:44:45.482Z');
export const getBuiltinModels = provider => Object.values(inventory.MODELS[provider] ?? {});
export const getBuiltinImageModels = provider => Object.values(inventory.IMAGE_MODELS[provider] ?? {});
export const getBuiltinClassifierModels = provider => Object.values(inventory.CLASSIFIER_MODELS[provider] ?? {});
export const getAllBuiltinModels = provider => [...getBuiltinModels(provider), ...getBuiltinImageModels(provider), ...getBuiltinClassifierModels(provider)];
