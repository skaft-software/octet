import test from 'node:test';
import assert from 'node:assert/strict';
import { handleRunMessage } from '../lib/run-messages.mjs';
import { toolLifecyclePayload } from '../lib/tool-events.mjs';

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

test('tool progress observation work stays independent of earlier progress', t => {
  for (const count of [10, 100, 1000]) {
    let oldReads = 0;
    const store = { state: {} };
    toolLifecyclePayload({ tool_call_id: 'call', tool_name: 'tool', args: {} }, store);
    for (let i = 0; i < count; i++) {
      const partialResult = { content: [{ type: 'text', text: 'x'.repeat(64) }] };
      const event = toolLifecyclePayload({ tool_call_id: 'call', tool_name: 'tool', partialResult }, store, 'tool/updated');
      assert.equal(event.partialResult, partialResult);
      // Any subsequent revisit of an earlier observation would be counted.
      Object.defineProperty(partialResult, 'content', { get() { oldReads++; return []; } });
    }
    t.diagnostic(`${count} progress events: old observation reads=${oldReads}`);
    assert.equal(oldReads, 0);
  }
});

test('native-sized argument chunks and later malformed input preserve argument observations', async () => {
  const events = [], runtime = { require() {}, metadata: () => ({ hooks: [] }), runEvent: async (type, value) => {
    if (type === 'message_update') events.push(value);
  } }, store = { state: { host: {} } };
  await handleRunMessage(runtime, 'turn/started', {}, store);
  const tool = async fields => handleRunMessage(runtime, 'message/updated', { tool_call: { index: 0, ...fields } }, store);
  await tool({ phase: 'start', id: 'call', name: 'tool' });
  await tool({ phase: 'delta', delta: JSON.stringify({ text: 'x'.repeat(20000) }) });
  await tool({ phase: 'delta', delta: 'malformed trailing input' });
  assert.equal(events.at(-1).message.content[0].arguments.text.length, 20000);
  events.at(-1).message.content[0].arguments.text = 'changed';
  assert.equal(events.at(-2).message.content[0].arguments.text.length, 20000);
  store.state.run.bytes = 4 * 1024 * 1024;
  await assert.rejects(tool({ phase: 'delta', delta: 'more' }), /bounded mirror exceeded/);
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
