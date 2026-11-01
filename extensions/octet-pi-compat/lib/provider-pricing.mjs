import { fields, invalid } from './errors.mjs';

function rate(value) {
  const exact = value * 1e6, rounded = Math.round(exact);
  if (!Number.isFinite(value) || value < 0 || !Number.isSafeInteger(rounded) || Math.abs(exact - rounded) > 1e-7) invalid('provider cost must be exactly representable in native microdollars');
  return rounded;
}
function rates(cost) {
  return { input: rate(cost.input), output: rate(cost.output), cache_read: rate(cost.cacheRead), cache_write_5m: rate(cost.cacheWrite), cache_write_1h: null, reasoning: null };
}
export function providerPricing(cost) {
  fields(cost, ['input', 'output', 'cacheRead', 'cacheWrite', 'tiers'], 'model cost');
  if (cost.tiers !== undefined && (!Array.isArray(cost.tiers) || cost.tiers.length > 256)) invalid('provider cost tiers must contain at most 256 definitions');
  const tiers = new Map();
  for (const tier of cost.tiers ?? []) {
    fields(tier, ['inputTokensAbove', 'input', 'output', 'cacheRead', 'cacheWrite'], 'model cost tier');
    if (!Number.isSafeInteger(tier.inputTokensAbove) || tier.inputTokensAbove < 0 || !Number.isSafeInteger(tier.inputTokensAbove + 1)) invalid('provider cost tier threshold');
    const value = { min_input_tokens: tier.inputTokensAbove + 1, ...rates(tier) };
    if (!tiers.has(value.min_input_tokens)) tiers.set(value.min_input_tokens, value);
  }
  return { ...rates(cost), tiers: [...tiers.values()].sort((a, b) => a.min_input_tokens - b.min_input_tokens) };
}
