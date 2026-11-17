import test from 'node:test';
import assert from 'node:assert/strict';
import { handleRunMessage } from '../lib/run-messages.mjs';
import { toolLifecyclePayload } from '../lib/tool-events.mjs';
import { StreamingJsonParser as PiParser } from '../../../scripts/fixtures/pi-reference/node_modules/pi-session-1-5-0/node_modules/@earendil-works/pi-ai/dist/utils/streaming-json.js';

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
        // Pi streaming-json.ts:7-12 exposes unfinished strings too.
        assert.deepEqual(retained[1].message.content[0].arguments, { text: '' });
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

test('20k numeric chunks materialize partial arguments with linear parse work', async t => {
  let latest;
  const runtime = { require() {}, metadata: () => ({ hooks: [] }), runEvent: async (type, event) => {
    if (type === 'message_update') latest = event;
  } }, store = { state: { host: {} } };
  await handleRunMessage(runtime, 'turn/started', {}, store);
  const tool = fields => handleRunMessage(runtime, 'message/updated', { tool_call: { index: 0, ...fields } }, store);
  await tool({ phase: 'start', id: 'call', name: 'tool' });
  // Pi deliberately allows a large open-container preview to lag while it
  // earns snapshot copy credit. Compare the same append/snapshot schedule.
  const oracle = new PiParser();
  oracle.append('{"items":[0'); oracle.snapshot();
  let expected;
  for (let i = 0; i < 20000; i++) { oracle.append(',0'); expected = oracle.snapshot(); }
  let calls = 0, bytes = 0;
  const parse = JSON.parse;
  JSON.parse = (text, ...args) => { calls++; bytes += Buffer.byteLength(text); return parse(text, ...args); };
  try {
    await tool({ phase: 'delta', delta: '{"items":[0' });
    for (let i = 0; i < 20000; i++) await tool({ phase: 'delta', delta: ',0' });
    const args = latest.message.content[0].arguments;
    t.diagnostic(`JSON.parse calls=${calls}, bytes=${bytes}`);
    assert.ok(calls <= 2, 'one getter must not revisit historical prefixes');
    assert.ok(bytes <= 2 * 40010, 'parse input stays proportional to current bytes');
    assert.deepEqual(args, expected);
    assert.ok(args.items.length > 19000);
    assert.equal(args.items.at(-1), 0);
    await tool({ phase: 'delta', delta: ']}' });
    assert.equal(latest.message.content[0].arguments.items.length, 20001);
    assert.deepEqual(args, expected, 'later chunks leave retained snapshots unchanged');
  } finally { JSON.parse = parse; }
});

test('partial argument snapshots include unfinished strings, arrays and scalars', async () => {
  const updates = [], runtime = { require() {}, metadata: () => ({ hooks: [] }), runEvent: async (type, event) => {
    if (type === 'message_update') updates.push(event);
  } }, store = { state: { host: {} } };
  await handleRunMessage(runtime, 'turn/started', {}, store);
  const tool = fields => handleRunMessage(runtime, 'message/updated', { tool_call: { index: 0, ...fields } }, store);
  await tool({ phase: 'start', id: 'call', name: 'tool' });
  const chunks = ['{"text":"hel', 'lo\\u00', '21", "items":[1', ',2], "ok":t', 'rue, "missing":', 'null}', 'malformed trailing input'];
  for (const delta of chunks) await tool({ phase: 'delta', delta });
  assert.deepEqual(updates.slice(1).map(event => event.message.content[0].arguments), [
    { text: 'hel' }, { text: 'hello' }, { text: 'hello!', items: [1] },
    { text: 'hello!', items: [1, 2], ok: true }, { text: 'hello!', items: [1, 2], ok: true },
    { text: 'hello!', items: [1, 2], ok: true, missing: null }, { text: 'hello!', items: [1, 2], ok: true, missing: null },
  ]);
});
