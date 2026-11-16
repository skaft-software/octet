import test from 'node:test';
import assert from 'node:assert/strict';
import { handleRunMessage } from '../lib/run-messages.mjs';

// Count actual serializer/parser input, not elapsed time, RSS or wire traffic.
// The exact same fixtures run before and after the optimization.
for (const kind of ['text', 'arguments']) test(`${kind} stream serialization follows new bytes at N/10N/100N`, async t => {
  const work = [];
  for (const count of [10, 100, 1000]) {
    let serialized = 0, parsed = 0;
    const stringify = JSON.stringify, parse = JSON.parse;
    JSON.stringify = (...args) => { const result = stringify(...args); serialized += Buffer.byteLength(result); return result; };
    JSON.parse = (text, ...args) => { parsed += Buffer.byteLength(text); return parse(text, ...args); };
    const retained = [];
    const runtime = { require() {}, metadata: () => ({ hooks: [] }), runEvent: async (type, event) => {
      if (type === 'message_update') retained.push(event);
    } };
    const store = { state: { host: {} } };
    try {
      await handleRunMessage(runtime, 'turn/started', {}, store);
      if (kind === 'arguments') await handleRunMessage(runtime, 'message/updated', { tool_call: { phase: 'start', index: 0, id: 'call', name: 'tool' } }, store);
      for (let i = 0; i < count; i++) {
        const delta = kind === 'text' ? 'x'.repeat(64) : i === 0 ? '{"text":"' : i === count - 1 ? '"}' : 'x'.repeat(64);
        await handleRunMessage(runtime, 'message/updated', kind === 'text' ? { delta } : { tool_call: { phase: 'delta', index: 0, delta } }, store);
      }
      t.diagnostic(`${count} deltas: serialized=${serialized} parsed=${parsed}`);
      work.push({ count, bytes: serialized + parsed });
      if (kind === 'text') {
        assert.equal(retained[0].message.content[0].text, 'x'.repeat(64));
        retained[0].message.content[0].text = 'callback mutation';
        assert.equal(store.state.run.partial.content[0].text.length, count * 64);
      } else {
        assert.deepEqual(retained[1].message.content[0].arguments, {});
        assert.equal(retained.at(-1).message.content[0].arguments.text.length, (count - 2) * 64);
      }
    } finally { JSON.stringify = stringify; JSON.parse = parse; }
  }
  for (const { count, bytes } of work) assert.ok(bytes < 2048 + count * 256, `full partial revisited at ${count}: ${bytes}`);
});

test('retained observations isolate nested mutation and preserve message/partial alias', async () => {
  const updates = [], runtime = { metadata: () => ({ hooks: [] }), runEvent: async (type, event) => {
    if (type === 'message_update') updates.push(event);
  } }, store = { state: { host: {} } };
  await handleRunMessage(runtime, 'turn/started', {}, store);
  for (const delta of ['one', 'two']) await handleRunMessage(runtime, 'message/updated', { delta }, store);
  assert.equal(updates[0].message.content[0].text, 'one');
  assert.equal(updates[1].message.content[0].text, 'onetwo');
  assert.strictEqual(updates[1].message, updates[1].assistantMessageEvent.partial);
});
