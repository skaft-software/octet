// Pure helper parity only. These tests do not qualify providers or a Rust host.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { createJiti } from 'jiti';
import { calculateCost, collapseSystemMessages, createAssistantMessageEventStream,
  getCurrentSystemPrompt, getCurrentTools } from '../shims/ai.mjs';

const tool = (name, description = name) => ({ name, description, parameters: { type: 'object', properties: {} } });
const usage = (input = 0, output = 0, cacheRead = 0, cacheWrite = 0, cacheWrite1h) => ({
  input, output, cacheRead, cacheWrite, totalTokens: input + output + cacheRead + cacheWrite,
  ...(cacheWrite1h === undefined ? {} : { cacheWrite1h }),
  cost: { input: 99, output: 99, cacheRead: 99, cacheWrite: 99, total: 99 },
});
const rates = { input: 2, output: 4, cacheRead: 0.5, cacheWrite: 3 };
const tiered = { cost: { ...rates, tiers: [
  { inputTokensAbove: 100, input: 10, output: 20, cacheRead: 1, cacheWrite: 15 },
  { inputTokensAbove: 50, input: 5, output: 10, cacheRead: 0.75, cacheWrite: 7.5 },
  // Equal thresholds keep the first matching tier, even if a later rate differs.
  { inputTokensAbove: 100, input: 1000, output: 2000, cacheRead: 100, cacheWrite: 1500 },
] } };

function transcript() {
  const a = tool('a'), b = tool('b'), replacement = tool('a', 'replacement');
  const user = { role: 'user', content: 'user', timestamp: 2 };
  const custom = { role: 'custom', content: 'inert', timestamp: 3, toolsAdded: [tool('ignored')] };
  const messages = [
    user,
    { role: 'system', content: [{ type: 'text', text: 'base' }, { type: 'text', text: 'α' }],
      sections: { tone: 'old tone', format: 'old format' }, toolsAdded: [a, b], timestamp: 0 },
    custom,
    { role: 'system', content: 'later', sections: { tone: 'new tone', format: null },
      toolsRemoved: [{ name: 'a' }, { name: 'unknown' }], toolsAdded: [replacement], timestamp: 5 },
    { role: 'system', content: '', sections: { format: 'new format', empty: '' }, timestamp: 7 },
  ];
  return { messages, user, custom, a, b, replacement };
}

function assistant(text = 'answer') {
  return { role: 'assistant', content: [{ type: 'text', text }], api: 'anthropic-messages',
    provider: 'example', model: 'example', usage: usage(), stopReason: 'stop', timestamp: 1 };
}

function freeze(value) {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) freeze(child);
    Object.freeze(value);
  }
  return value;
}

test('cost mutates the existing dollar-cost object, preserving usage/model data', () => {
  const model = freeze({ cost: { ...rates } }), input = usage(10, 20, 30, 40);
  const before = structuredClone(input), cost = input.cost;
  assert.equal(calculateCost(model, input), cost);
  // Keep Pi's IEEE-754 arithmetic order; do not round to decimal literals.
  assert.deepEqual(cost, { input: (2 / 1000000) * 10, output: (4 / 1000000) * 20,
    cacheRead: (0.5 / 1000000) * 30, cacheWrite: 0.00012, total: 0.000235 });
  assert.deepEqual({ ...input, cost: before.cost }, before);
  assert.deepEqual(calculateCost(model, usage()), { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 });
});

test('cost tiers use strict input+cache thresholds, highest match, and stable equal tiers', () => {
  const model = freeze(structuredClone(tiered));
  for (const [input, output, cacheRead, cacheWrite, expectedInputRate] of [
    [50, 0, 0, 0, 2], [51, 0, 0, 0, 5], [100, 0, 0, 0, 5],
    [1, 0, 50, 50, 10], [1, 1000000, 0, 0, 2],
  ]) {
    const value = usage(input, output, cacheRead, cacheWrite);
    calculateCost(model, value);
    assert.equal(value.cost.input, expectedInputRate / 1000000 * input);
  }
});

test('one-hour cache writes use twice the selected input rate, not cacheWrite rate', () => {
  const value = usage(10, 20, 30, 40, 25);
  calculateCost({ cost: rates }, value);
  assert.equal(value.cost.cacheWrite, (3 * 15 + 2 * 2 * 25) / 1000000);
  const tieredValue = usage(1, 0, 60, 40, 25);
  calculateCost(tiered, tieredValue);
  assert.equal(tieredValue.cost.cacheWrite, (15 * 15 + 10 * 2 * 25) / 1000000);
});

test('tool replay preserves identity, order, replacement and remove-before-add semantics', () => {
  const value = transcript(); freeze(value.messages);
  const current = getCurrentTools(value.messages);
  assert.deepEqual(current.map(entry => entry.name), ['b', 'a']);
  assert.equal(current[0], value.b); assert.equal(current[1], value.replacement);
  assert.deepEqual(getCurrentTools([{ role: 'system', content: '', timestamp: 0,
    toolsAdded: [value.a, value.b] }, { role: 'system', content: '', timestamp: 1,
    toolsAdded: [value.replacement] }]), [value.replacement, value.b]);
  assert.deepEqual(getCurrentTools([]), []);
});

test('prompt replay joins text blocks, appends content and patches/deletes ordered sections', () => {
  const value = transcript(); freeze(value.messages);
  assert.equal(getCurrentSystemPrompt(value.messages), 'base\nα\n\nlater\n\nnew tone\n\nnew format');
  assert.equal(getCurrentSystemPrompt([]), '');
  assert.equal(getCurrentSystemPrompt([{ role: 'system', content: '', timestamp: 0,
    sections: { tone: 'only section' } }]), 'only section');
  assert.equal(getCurrentSystemPrompt([{ role: 'system', content: 'base', timestamp: 0,
    sections: { tone: 'gone' } }, { role: 'system', content: '', timestamp: 1,
    sections: { tone: null } }]), 'base');
});

test('collapse returns only a new transcript envelope, keeping message/tool identities and zero timestamp', () => {
  const value = transcript(), context = freeze({ messages: value.messages, extra: 'not propagated' });
  const collapsed = collapseSystemMessages(context);
  assert.notEqual(collapsed, context); assert.notEqual(collapsed.messages, context.messages);
  assert.deepEqual(Object.keys(collapsed), ['messages']);
  assert.deepEqual(collapsed.messages[0], { role: 'system', content: 'base\nα\n\nlater',
    sections: { tone: 'new tone', format: 'new format', empty: '' }, toolsAdded: [value.b, value.replacement], timestamp: 0 });
  assert.equal(collapsed.messages[1], value.user); assert.equal(collapsed.messages[2], value.custom);
  assert.equal(collapsed.messages[0].toolsAdded[0], value.b);
  assert.equal(getCurrentSystemPrompt(collapsed.messages), getCurrentSystemPrompt(value.messages));
  assert.deepEqual(collapseSystemMessages({ messages: [] }), { messages: [] });
  const userOnly = collapseSystemMessages({ messages: [value.user] });
  assert.equal(userOnly.messages[0], value.user);
  assert.deepEqual(collapseSystemMessages({ messages: [{ role: 'system', content: '', timestamp: 0 }] }),
    { messages: [{ role: 'system', content: '', timestamp: 0 }] });
});

test('queued stream events and terminal result preserve identity; later pushes are ignored', { timeout: 2000 }, async () => {
  const stream = createAssistantMessageEventStream(), message = assistant();
  const first = { type: 'text_delta', contentIndex: 0, delta: 'a', partial: message };
  const second = { type: 'text_delta', contentIndex: 0, delta: 'b', partial: message };
  const done = { type: 'done', reason: 'stop', message };
  const result = stream.result(); assert.equal(stream.result(), result);
  stream.push(first); stream.push(second); stream.push(done); stream.push({ type: 'late' });
  const events = []; for await (const event of stream) events.push(event);
  assert.deepEqual(events, [first, second, done]);
  assert.equal(events[0], first); assert.equal(events[2], done); assert.equal(await result, message);
  stream.end(assistant('ignored')); assert.equal(await result, message);
});

test('waiting stream consumers receive FIFO events; end wakes all remaining consumers', { timeout: 2000 }, async () => {
  const stream = createAssistantMessageEventStream(), one = stream[Symbol.asyncIterator](), two = stream[Symbol.asyncIterator]();
  const a = one.next(), b = two.next(), first = { type: 'start', partial: assistant() };
  stream.push(first); assert.deepEqual(await a, { value: first, done: false });
  const next = one.next(), message = assistant('ended'); stream.end(message);
  assert.deepEqual(await b, { value: undefined, done: true });
  assert.deepEqual(await next, { value: undefined, done: true });
  assert.equal(await stream.result(), message);
});

test('error terminal events resolve, not reject, with the original Pi assistant message', { timeout: 2000 }, async () => {
  const stream = createAssistantMessageEventStream(), error = { ...assistant(), stopReason: 'error', errorMessage: 'failed' };
  const event = { type: 'error', reason: 'error', error };
  const iterator = stream[Symbol.asyncIterator](), waiting = iterator.next();
  stream.push(event);
  assert.equal((await waiting).value, event); assert.equal(await stream.result(), error);
  assert.deepEqual(await iterator.next(), { value: undefined, done: true });
});

test('end without a result drains queued events but does not invent a final result', { timeout: 2000 }, async () => {
  const stream = createAssistantMessageEventStream(), event = { type: 'start', partial: assistant() };
  let resolved = false; stream.result().then(() => { resolved = true; });
  stream.push(event); stream.end();
  const events = []; for await (const value of stream) events.push(value);
  assert.deepEqual(events, [event]); await Promise.resolve(); assert.equal(resolved, false);
  const message = assistant('later end'); stream.end(message);
  assert.equal(await stream.result(), message);
});

// Optional read-only comparison with the actual pinned public implementations.
// The three imported TS utility modules have only erased type dependencies and
// text helpers. Extract calculateCost alone to avoid importing the model
// runtime and auth dependencies into this bounded pure-helper oracle.
test('five helpers agree with source-hash-verified Pi 1.0.2 public sources', {
  skip: !process.env.PI_REFERENCE_REPO, timeout: 5000,
}, async () => {
  const root = join(process.env.PI_REFERENCE_REPO, 'packages/ai/src');
  const hashes = {
    'models.ts': '4739010c7e4f7596607b1dd495b9d5ab6cd921e29f55c5c331259a04b7253269',
    'utils/transcript.ts': 'e0814f35bdcc93b017fa45589a6920e46b5480b88fba01ef93e5d2d4e815f0f2',
    'utils/text.ts': 'd37c1825855c7fc71669a545b5b266810242c1c7e58d57228c0c143f29c1c544',
    'utils/event-stream.ts': '147084657f76665ff7d6c8dcd6eb0745149e3aaee3e7b9c272b315a1b5679386',
  };
  const sources = {};
  for (const [path, hash] of Object.entries(hashes)) {
    const source = readFileSync(join(root, path), 'utf8');
    assert.equal(createHash('sha256').update(source).digest('hex'), hash, `pinned Pi source changed: ${path}`);
    sources[path] = source;
  }
  const matched = sources['models.ts'].match(/export function calculateCost\(model: AnyModel, usage: Usage\): Usage\["cost"\] \{[\s\S]*?\n\}/);
  assert.ok(matched, 'pinned cost implementation not found');
  const costSource = matched[0].replace('export ', '').replace('model: AnyModel, usage: Usage', 'model, usage')
    .replace(': Usage["cost"]', '').replace('rates: ModelCostRates', 'rates');
  const oracleCost = new Function(`${costSource}; return calculateCost;`)();
  const jiti = createJiti(import.meta.url, { fsCache: false, tryNative: false });
  const oracleTranscript = await jiti.import(join(root, 'utils/transcript.ts'));
  const oracleStream = await jiti.import(join(root, 'utils/event-stream.ts'));
  for (const model of [{ cost: rates }, tiered]) {
    for (const value of [usage(), usage(10, 20, 30, 40), usage(10, 20, 30, 40, 25),
      usage(50), usage(51), usage(100), usage(101), usage(1, 1000000), usage(1, 0, 60, 40, 25)]) {
      const left = structuredClone(value), right = structuredClone(value);
      assert.equal(calculateCost(model, left), left.cost); assert.equal(oracleCost(model, right), right.cost);
      assert.deepEqual(left, right);
    }
  }
  for (const messages of [[], transcript().messages, [{ role: 'user', content: 'only user', timestamp: 1 }],
    [{ role: 'system', content: '', timestamp: 0 }],
    [{ role: 'system', content: 'base', sections: { '2': 'two', '1': 'one', tone: 'old' }, timestamp: 0 },
      { role: 'system', content: '', sections: { tone: null, format: 'new' }, timestamp: 1 }]]) {
    assert.deepEqual(getCurrentTools(messages), oracleTranscript.getCurrentTools(messages));
    assert.equal(getCurrentSystemPrompt(messages), oracleTranscript.getCurrentSystemPrompt(messages));
    assert.deepEqual(collapseSystemMessages({ messages }), oracleTranscript.collapseSystemMessages({ messages }));
  }
  for (const terminal of ['done', 'error', 'end']) {
    const left = createAssistantMessageEventStream(), right = oracleStream.createAssistantMessageEventStream();
    const message = assistant(), first = { type: 'start', partial: message };
    left.push(first); right.push(first);
    const a = left[Symbol.asyncIterator](), b = right[Symbol.asyncIterator]();
    assert.deepEqual(await a.next(), await b.next());
    const waitingA = a.next(), waitingB = b.next();
    if (terminal === 'end') { left.end(message); right.end(message); }
    else {
      const event = terminal === 'done' ? { type: 'done', reason: 'stop', message } : { type: 'error', reason: 'error', error: message };
      left.push(event); right.push(event);
    }
    assert.deepEqual(await waitingA, await waitingB);
    assert.equal(await left.result(), message); assert.equal(await right.result(), message);
    assert.deepEqual(await a.next(), await b.next());
  }
});
