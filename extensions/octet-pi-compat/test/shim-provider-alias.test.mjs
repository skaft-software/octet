import test from 'node:test';
import assert from 'node:assert/strict';
import { createJiti } from 'jiti';
import { emulatedPiAliases, typeboxAliases } from '../lib/pi-modules.mjs';
import { withNativeProvider } from '../lib/native-provider.mjs';

for (const prefix of ['@earendil-works', '@mariozechner']) {
  test(`${prefix} jiti provider helpers share the native admitted callback scope`, { timeout: 5000 }, async () => {
    const jiti = createJiti(import.meta.url, { alias: { ...typeboxAliases(), ...emulatedPiAliases() }, tryNative: false });
    const ai = await jiti.import(`${prefix}/pi-ai`), catalog = await jiti.import(`${prefix}/pi-ai/providers/all`);
    const model = ai.getModels('anthropic').find(model => model.api === 'anthropic-messages');
    const controller = new AbortController(), options = { signal: controller.signal };
    const context = { messages: [], tools: [] }, calls = [];
    const runtime = {
      require(feature) { assert.equal(feature, 'provider_inheritance_v1'); },
      transport: { async request(method, params) {
        calls.push({ method, params });
        return { message: { model: 'fixture-native-model', protocol: 'anthropic_messages', content: [{ Text: 'fixture native response' }] },
          usage: { input_tokens: 2, output_tokens: 3, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: 5 }, stop_reason: 'end_turn', timestamp: 123 };
      } },
    };
    for (const delegate of [ai.anthropicMessagesApi().streamSimple, catalog.builtinModels().getProvider('anthropic').stream]) {
      const job = { id: 'fixture-native-stream', controller, model: { pi: model }, context, options, entry: { inherited: true, config: {} }, request: { tools: [] } };
      const stream = withNativeProvider(runtime, job, () => delegate(model, context, options));
      const events = [];
      for await (const event of stream) events.push(event.type);
      assert.deepEqual(events, ['start', 'text_start', 'text_delta', 'text_end', 'done']);
      const result = await stream.result();
      assert.equal(result.content[0].text, 'fixture native response');
      assert.equal(result.usage.totalTokens, 5);
      assert.equal(result.stopReason, 'stop');
      assert.throws(() => delegate(model, context, options), /admitted provider request/);
    }
    assert.equal(calls.length, 2);
    assert.ok(calls.every(call => call.method === 'provider/delegate' && call.params.stream_id === 'fixture-native-stream'));
    assert.doesNotMatch(JSON.stringify(calls), /apiKey|authorization/);
  });
}
