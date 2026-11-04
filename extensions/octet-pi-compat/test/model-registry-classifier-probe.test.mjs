import test from 'node:test';
import assert from 'node:assert/strict';
import { modelRegistry } from '../lib/providers.mjs';

// pi-mcp-adapter 7.0.0 `executeSearch` feature-detects semantic search on EVERY `mcp search`, lexical included
// (semantic-search.ts classifierRegistryOf): typeof registry.classify / findOfType / getProviderAuthStatus === 'function'.
// Pi's classify() is a credentialed provider request that the pinned Pi 1.0.2 surface and octet's host contract
// do not have, so the facade must report the classifier API as absent ("a host without classifier models", the
// adapter's own documented fallback to word search) instead of throwing from the probe, which failed every search.
const classifierRegistryOf = registry => typeof registry?.classify === 'function' && typeof registry.findOfType === 'function'
  && typeof registry.getProviderAuthStatus === 'function' ? registry : undefined;

test('mcp search probe: classifier registry API reads as absent, so lexical search proceeds', () => {
  const registry = modelRegistry(() => ({}));
  assert.equal(registry.classify, undefined);
  assert.equal(registry.findOfType, undefined);
  assert.equal(classifierRegistryOf(registry), undefined);
});

test('classifier probe tolerance does not expose credentials or hide other unsupported members', () => {
  const registry = modelRegistry(() => ({}));
  assert.throws(() => registry.generateImages, /unsupported_feature ctx\.modelRegistry\.generateImages/);
  assert.throws(() => registry.getProviderAuthStatus, /unsupported_feature ctx\.modelRegistry\.getProviderAuthStatus/);
  assert.equal(typeof registry.getApiKeyAndHeaders, 'function'); // existing, capability-gated credential path unchanged
  assert.equal(typeof registry.getAll, 'function');
});
