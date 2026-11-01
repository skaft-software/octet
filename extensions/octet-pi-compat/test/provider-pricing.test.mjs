import test from 'node:test';
import assert from 'node:assert/strict';
import { registerProvider, startProviderRegistration, validateProviderInitialization } from '../lib/providers.mjs';

const rates = (input, output = input * 2) => ({ input, output, cacheRead: input / 10, cacheWrite: input * 1.25 });
const definition = cost => ({ api: 'openai-completions', models: [{ id: 'priced', name: 'Priced', reasoning: false, input: ['text'], cost, contextWindow: 8192, maxTokens: 1024 }], streamSimple() {} });
function fixture() {
  const frames = [];
  const runtime = { initialized: false, stopping: false, features: new Set(['provider_proxy_v1']), require() {}, backgroundError(error) { throw error; },
    transport: { async request(method, params) { frames.push({ method, params }); return { revision: 1 }; }, async notify(method, params) { frames.push({ method, params }); } } };
  return { runtime, frames };
}
async function registration(cost) {
  const value = fixture();
  registerProvider(value.runtime, 0, 'priced-provider', definition(cost));
  assert.equal(value.frames.length, 0);
  validateProviderInitialization(value.runtime);
  value.runtime.initialized = true;
  startProviderRegistration(value.runtime);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(value.frames[0].method, 'providers/register');
  return value.frames[0].params.models[0].pi_metadata.pricing;
}

test('Pi full-rate tiers map strict thresholds into exact native inclusive pricing', async () => {
  const pricing = await registration({ ...rates(1), tiers: [{ inputTokensAbove: 200, ...rates(6) }, { inputTokensAbove: 100, ...rates(3) }] });
  assert.equal(pricing.input, 1_000_000);
  assert.deepEqual(pricing.tiers, [
    { min_input_tokens: 101, input: 3_000_000, output: 6_000_000, cache_read: 300_000, cache_write_5m: 3_750_000, cache_write_1h: null, reasoning: null },
    { min_input_tokens: 201, input: 6_000_000, output: 12_000_000, cache_read: 600_000, cache_write_5m: 7_500_000, cache_write_1h: null, reasoning: null },
  ]);
});

test('equal Pi thresholds retain the first complete matching tariff', async () => {
  const pricing = await registration({ ...rates(1), tiers: [{ inputTokensAbove: 0, ...rates(3) }, { inputTokensAbove: 0, ...rates(9) }] });
  assert.equal(pricing.tiers.length, 1);
  assert.equal(pricing.tiers[0].min_input_tokens, 1);
  assert.equal(pricing.tiers[0].input, 3_000_000);
});

test('incomplete, unrepresentable and unknown tier facts fail before native registration', () => {
  for (const tier of [
    { inputTokensAbove: -1, ...rates(1) }, { inputTokensAbove: 0.5, ...rates(1) },
    { inputTokensAbove: Number.MAX_SAFE_INTEGER, ...rates(1) },
    { inputTokensAbove: 1, input: 1 }, { inputTokensAbove: 1, ...rates(1), input: Infinity },
    { inputTokensAbove: 1, ...rates(1), input: 0.00000001 },
    { inputTokensAbove: 1, ...rates(1), reasoning: 1 },
  ]) {
    const { runtime, frames } = fixture();
    assert.throws(() => registerProvider(runtime, 0, 'priced-provider', definition({ ...rates(1), tiers: [tier] })));
    assert.deepEqual(frames, []);
  }
});
