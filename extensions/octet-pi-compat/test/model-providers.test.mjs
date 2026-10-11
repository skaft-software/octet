import test from 'node:test';
import assert from 'node:assert/strict';
import { currentModel, modelRegistry, modelView, scopedModels, thinkingLevel } from '../lib/providers.mjs';
import { host } from './helper.mjs';
const view = host.model_view;
const refusal = error => error.code === -32601 && /unsupported_feature/.test(error.message);

test('thinking getters only expose actual portable Pi levels', () => {
  for (const level of ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']) {
    assert.equal(thinkingLevel({ reasoning: level }), level);
  }
  for (const reasoning of [null, undefined, 'Off', 'Effort(High)', 'on', 'ultra', 'budget=123', { effort: 'high' }]) {
    assert.equal(thinkingLevel({ reasoning }, true), undefined);
    assert.throws(() => thinkingLevel({ reasoning }), refusal);
  }
});

test('model view preserves factual units and snapshots do not mutate native facts', () => {
  const model = currentModel({ model_view: view });
  assert.equal(model.id, view.id);
  assert.equal(model.cost.input, 1);
  assert.equal(model.cost.cacheRead, .1);
  model.input.push('image'); model.cost.input = 99;
  assert.deepEqual(view.input, ['text']); assert.equal(view.cost.input, 1000000);
  assert.equal(currentModel({ model: 'bare-id' }), undefined);
  assert.equal(currentModel({ model: null }), undefined);
  assert.equal(modelView(undefined), undefined);
});

test('scoped models are the host invocation scope, not the available catalog', () => {
  assert.throws(() => scopedModels({}), refusal);
  assert.deepEqual(scopedModels({ pi_models: { scoped_models: [] } }), []);
  const facts = { pi_models: { scoped_models: [{ model: view, thinkingLevel: 'high' }] } };
  const scope = scopedModels(facts);
  assert.equal(scope[0].thinkingLevel, 'high');
  scope[0].model.input.push('image');
  assert.deepEqual(view.input, ['text']);
});

test('registry reads follow refreshed host snapshots without inventing full inventory', () => {
  let facts = { pi_models: { available_models: [view] } };
  const registry = modelRegistry(() => facts);
  assert.equal(registry.getAvailable()[0].contextWindow, 32768);
  assert.throws(() => registry.getAll(), refusal);
  assert.throws(() => registry.find('test', view.id), refusal);
  facts = { pi_models: { available_models: [] } };
  assert.deepEqual(registry.getAvailable(), []);
  facts = {}; assert.throws(() => registry.getAvailable(), refusal);
});

test('registry refuses incomplete inventory and provider-level auth facts', () => {
  const key = JSON.stringify([view.provider, view.id]);
  const registry = modelRegistry(() => ({ pi_models: {
    available_models: [view],
    model_auth: { [key]: { using_oauth: true } },
  } }));
  assert.throws(() => registry.getAll(), refusal);
  assert.throws(() => registry.find(view.provider, view.id), refusal);
  assert.throws(() => registry.hasConfiguredAuth(view), refusal);
  assert.throws(() => registry.isUsingOAuth(view), refusal);
  assert.throws(() => registry.getApiKey(view), refusal);
});
