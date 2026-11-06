import test from 'node:test';
import assert from 'node:assert/strict';
import { customOpenAiChatProfile } from '../lib/custom-provider-http.mjs';
import { snapshotCustomProvider } from '../lib/custom-provider.mjs';
import { withNativeProvider, openaiCompletionsDelegate, openaiResponsesDelegate } from '../lib/native-provider.mjs';

const model = { id: 'custom/model', name: 'Custom', provider: 'custom', api: 'openai-completions', baseUrl: 'https://custom.invalid/v1', reasoning: true, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 8192, maxTokens: 1024 };
const profile = (developer = true, max = false, strict = false) => ({ supports_developer_role: developer, use_max_tokens: max, supports_strict_mode: strict });

test('custom Chat defaults match pinned Pi without granting strict support to compatible endpoints', () => {
  assert.deepEqual(customOpenAiChatProfile(model), profile());
  assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl: 'https://api.openai.com/v1' }), profile());
  assert.deepEqual(customOpenAiChatProfile({ ...model, compat: { supportsDeveloperRole: false, maxTokensField: 'max_tokens' } }), profile(false, true));
  assert.deepEqual(customOpenAiChatProfile({ ...model, compat: { supportsDeveloperRole: true, maxTokensField: 'max_completion_tokens', supportsStrictMode: true } }), profile(true, false, true));
  assert.deepEqual(customOpenAiChatProfile({ ...model, compat: { supportsDeveloperRole: null, maxTokensField: null, supportsStrictMode: null } }), profile());
});

test('bounded role and token defaults follow pinned provider and URL detection with explicit overrides last', () => {
  for (const provider of ['zai', 'zai-coding-cn', 'together', 'moonshotai', 'moonshotai-cn', 'cloudflare-ai-gateway', 'nvidia', 'ant-ling', 'deepseek']) {
    assert.deepEqual(customOpenAiChatProfile({ ...model, provider }), profile(false, true), provider);
    assert.deepEqual(customOpenAiChatProfile({ ...model, provider, compat: { supportsDeveloperRole: true, maxTokensField: 'max_completion_tokens', supportsStrictMode: true } }), profile(true, false, true), provider);
  }
  for (const provider of ['cerebras', 'xai', 'opencode', 'cloudflare-workers-ai']) assert.deepEqual(customOpenAiChatProfile({ ...model, provider }), profile(false), provider);
  for (const baseUrl of ['https://api.z.ai/v1', 'https://open.bigmodel.cn/v1', 'https://api.together.ai/v1', 'https://api.together.xyz/v1', 'https://api.moonshot.cn/v1', 'https://gateway.ai.cloudflare.com/v1', 'https://integrate.api.nvidia.com/v1', 'https://api.ant-ling.com/v1', 'https://API.DEEPSEEK.COM/v1', 'https://chutes.ai/v1']) assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl }), profile(false, true), baseUrl);
  for (const baseUrl of ['https://api.x.ai/v1', 'https://cerebras.ai/v1', 'https://opencode.ai/v1', 'https://api.cloudflare.com/v1']) assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl }), profile(false), baseUrl);
  for (const id of ['openai/gpt', 'anthropic/claude']) assert.deepEqual(customOpenAiChatProfile({ ...model, provider: 'openrouter', id }), profile(), id);
  assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl: 'https://openrouter.ai/api/v1', id: 'other/model' }), profile(false));
});

test('URL-derived Chat defaults match DNS labels, not paths or lookalike hosts', () => {
  for (const host of ['api.z.ai', 'open.bigmodel.cn', 'api.together.ai', 'api.together.xyz', 'api.moonshot.cn', 'api.moonshot.ai', 'openrouter.ai', 'api.cloudflare.com', 'gateway.ai.cloudflare.com', 'integrate.api.nvidia.com', 'api.ant-ling.com', 'cerebras.ai', 'deepseek.com', 'api.x.ai', 'chutes.ai', 'opencode.ai']) {
    for (const baseUrl of [`https://${host}.invalid/v1`, `https://not-${host}/v1`, `https://custom.invalid/${host}/v1`, `https://custom.invalid/v1?endpoint=${host}`, `https://custom.invalid/v1#${host}`]) {
      assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl }), profile(), baseUrl);
    }
  }
  assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl: 'https://REGION.DEEPSEEK.COM/v1' }), profile(false, true));
  assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl: 'https://API.MOONSHOT.AI/v1' }), profile(false, true));
  assert.deepEqual(customOpenAiChatProfile({ ...model, baseUrl: '' }), profile());
});

test('custom profile validates only its three honored fields without replacing other compatibility data', () => {
  for (const compat of [{ supportsDeveloperRole: 'false' }, { supportsDeveloperRole: 0 }, { supportsStrictMode: 'true' }, { supportsStrictMode: {} }, { maxTokensField: 'arbitrary_cap' }]) assert.throws(() => customOpenAiChatProfile({ ...model, compat }), /invalid_request custom OpenAI Chat compatibility/);
  const compat = { supportsDeveloperRole: false, maxTokensField: 'max_tokens', supportsStrictMode: false, sendSessionAffinityHeaders: true, sessionAffinityFormat: 'openai-nosession', thinkingFormat: 'openai' };
  const before = structuredClone(compat);
  assert.deepEqual(customOpenAiChatProfile({ ...model, compat }), profile(false, true));
  assert.deepEqual(compat, before);
  assert.equal(customOpenAiChatProfile({ ...model, api: 'openai-responses' }), undefined);
  assert.equal(customOpenAiChatProfile({ ...model, api: 'anthropic-messages' }), undefined);
});

test('actual custom snapshot retains compatibility and adds only the bounded native Chat baseline', () => {
  const compat = { supportsDeveloperRole: false, maxTokensField: 'max_tokens', supportsStrictMode: true, sendSessionAffinityHeaders: true };
  const selected = { ...model, compat };
  const declarations = [];
  const entry = snapshotCustomProvider({ id: model.provider, getModels: () => [selected], streamSimple() {} }, 0, (id, config) => {
    declarations.push(config);
    return { models: [{ wire: { id: 'native-wire-id', pi_metadata: { input: ['text'], base_url: model.baseUrl } } }] };
  });
  assert.deepEqual(entry.models[0].pi.compat, compat);
  assert.deepEqual(entry.models[0].wire.pi_metadata.custom_openai_chat, profile(false, true, true));
  assert.equal(declarations[0].models[0].compat, undefined);
  assert.deepEqual(selected, { ...model, compat });
});

function delegateFixture(t, api = 'openai-completions') {
  const selected = { ...model, api, compat: { supportsDeveloperRole: true, maxTokensField: 'max_completion_tokens', supportsStrictMode: true } };
  const context = { systemPrompt: 'native system', messages: [], tools: [] };
  const request = { system: 'native system', messages: [], tools: [], tool_choice: 'auto', max_output_tokens: 64, temperature: null, reasoning: { type: 'off' }, session_id: 'native-session', cache_retention: 'short' };
  const job = { id: 'native-stream', entry: { custom: {} }, controller: new AbortController(), model: { pi: selected }, context, request, options: { cacheRetention: 'short', sessionId: 'native-session' } };
  const frames = [];
  const runtime = { require() {}, transport: { async request(method, params) {
    assert.equal(method, 'provider/delegate'); frames.push(structuredClone(params));
    return { message: { model: model.id, protocol: api === 'openai-completions' ? 'open_ai_chat' : 'open_ai_responses', content: [{ Text: 'native reply' }] }, usage: { input_tokens: 1, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: 1 }, stop_reason: 'end_turn', timestamp: 123 };
  } } };
  t.after(() => job.controller.abort());
  const run = (override = {}) => withNativeProvider(runtime, job, () => (api === 'openai-completions' ? openaiCompletionsDelegate : openaiResponsesDelegate)({ ...selected, ...override }, context, { ...job.options, signal: job.controller.signal, fetch() { assert.fail('scripted native peer must not fabricate HTTP dispatch'); } }));
  return { job, request, frames, run };
}

test('same-ID helper override reaches actual delegate without changing cap, identity or accounting', async t => {
  const value = delegateFixture(t);
  const before = structuredClone(value.request);
  const result = await value.run({ compat: { supportsDeveloperRole: false, maxTokensField: 'max_tokens' } }).result();
  assert.equal(result.stopReason, 'stop');
  assert.equal(value.frames.length, 1);
  assert.deepEqual(value.frames[0].custom_openai_chat, profile(false, true));
  assert.equal(value.frames[0].custom_http, true);
  assert.deepEqual(value.frames[0].request, before);
  assert.equal(result.usage.totalTokens, 1);
  assert.deepEqual(value.job.model.pi.compat, { supportsDeveloperRole: true, maxTokensField: 'max_completion_tokens', supportsStrictMode: true });
  assert.throws(() => value.run(), /delegate only once/);
});

test('helper identity and compatibility failures refuse before native delegation', t => {
  for (const override of [{ id: 'other' }, { provider: 'other' }, { api: 'openai-responses' }, { baseUrl: 'https://other.invalid/v1' }, { compat: { supportsStrictMode: 'false' } }]) {
    const value = delegateFixture(t);
    assert.throws(() => value.run(override));
    assert.equal(value.frames.length, 0);
    assert.equal(value.job.delegated, undefined);
  }
});

test('Responses delegates never receive a Chat-only compatibility profile', async t => {
  const value = delegateFixture(t, 'openai-responses');
  assert.equal((await value.run().result()).stopReason, 'stop');
  assert.equal(value.frames.length, 1);
  assert.equal(value.frames[0].custom_http, true);
  assert.equal(Object.hasOwn(value.frames[0], 'custom_openai_chat'), false);
});
