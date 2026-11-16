import test from 'node:test';
import assert from 'node:assert/strict';
import { sessionEntryCopies, sessionEntryCopy, translateSessionEntries } from '../lib/session-mirror.mjs';
import { piToCanonical } from '../lib/provider-context.mjs';

const user = (id, parent, content = [{ Text: 'message' }]) => ({ id, parent,
  value: { type: 'message', User: { content } } });
const assistant = (id, parent, names) => ({ id, parent, value: { type: 'message', Assistant: {
  model: 'model', protocol: 'open_ai_chat', content: names.map(name => ({ ToolCall: {
    id: 'call', name, arguments_json: '{}', async: false, argument_error: null,
  } })),
} } });
const result = (id, parent) => user(id, parent, [{ ToolResult: {
  tool_call_id: 'call', content: [{ Text: 'result' }], is_error: false,
} }]);

test('large session ancestry is traversed linearly, not once for every entry', () => {
  const count = 1600;
  let parentReads = 0;
  const entries = Array.from({ length: count }, (_, i) => {
    const item = user(String(i), i ? String(i - 1) : null);
    const parent = item.parent;
    Object.defineProperty(item, 'parent', { enumerable: true, get() { parentReads++; return parent; } });
    return item;
  });
  const translated = translateSessionEntries(entries, 'actual');
  assert.equal(translated.length, count);
  assert.equal(translated.at(-1).parentId, String(count - 2));
  assert.ok(parentReads <= 5 * count, `expected linear ancestry reads, got ${parentReads} for ${count} entries`);
});

test('tool names follow the nearest actual ancestor, including forks, duplicates and out-of-order snapshots', () => {
  const entries = [result('near-result', 'near'), result('root-result', 'root'),
    assistant('other', 'root', ['other-branch']), assistant('near', 'root', ['near-first', 'near-second']),
    assistant('root', null, ['root-name']), result('other-result', 'other')];
  const translated = translateSessionEntries(entries, 'actual');
  assert.deepEqual(translated.map(item => item.id), entries.map(item => item.id));
  assert.deepEqual([translated[0].message.toolName, translated[1].message.toolName, translated[5].message.toolName],
    ['near-first', 'root-name', 'other-branch']);
  assert.throws(() => translateSessionEntries([result('cut', 'missing')], 'actual'), /no preceding call/);
});

test('tool-name indexing agrees with a nearest-ancestor oracle across seeded shuffled forests', () => {
  let seed = 13;
  const random = max => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % max; };
  for (let sample = 0; sample < 32; sample++) {
    const entries = [assistant('root', null, ['root-first', 'root-second'])], expected = new Map();
    for (let i = 1; i < 48; i++) {
      const parent = entries[random(entries.length)];
      if (random(3) === 0) {
        const item = assistant(`e${i}`, parent.id, [`tool-${i}`, `duplicate-${i}`]);
        item.value.Assistant.content[0].ToolCall.id = `call-${random(3)}`;
        entries.push(item);
      } else {
        // The small oracle deliberately walks ancestry instead of sharing the
        // production traversal. Calls on sibling branches are never candidates.
        const calls = new Map();
        for (let cursor = parent; cursor; cursor = entries.find(entry => entry.id === cursor.parent)) {
          for (const part of cursor.value.Assistant?.content ?? []) {
            if (!calls.has(part.ToolCall.id)) calls.set(part.ToolCall.id, part.ToolCall.name);
          }
        }
        const [id, tool] = [...calls][random(calls.size)];
        const item = result(`e${i}`, parent.id);
        item.value.User.content[0].ToolResult.tool_call_id = id;
        entries.push(item); expected.set(item.id, tool);
      }
    }
    // Include a separate root and an unrelated native marker in every forest.
    entries.push(assistant('other-root', null, ['other-tool']), result('other-result', 'other-root'),
      { id: 'marker', parent: 'missing-cut', value: { type: 'config' } });
    expected.set('other-result', 'other-tool');
    for (let i = entries.length - 1; i > 0; i--) { const j = random(i + 1); [entries[i], entries[j]] = [entries[j], entries[i]]; }
    const before = JSON.stringify(entries), translated = translateSessionEntries(entries, 'actual');
    assert.deepEqual(translated.map(entry => entry.id), entries.map(entry => entry.id));
    for (const entry of translated) if (expected.has(entry.id)) assert.equal(entry.message.toolName, expected.get(entry.id));
    assert.equal(JSON.stringify(entries), before);
  }
});

test('cycles in message ancestry still refuse, including cycles through native markers', () => {
  const marker = (id, parent) => ({ id, parent, value: { type: 'marker' } });
  const markers = [marker('one', 'two'), marker('two', 'one')];
  assert.deepEqual(translateSessionEntries(markers, 'actual').map(item => item.type), ['octet_native', 'octet_native']);
  assert.throws(() => translateSessionEntries([...markers, user('message', 'one')], 'actual'), /cyclic session ancestry/);
  assert.throws(() => translateSessionEntries([user('self', 'self')], 'actual'), /cyclic session ancestry/);
  assert.throws(() => translateSessionEntries([user('duplicate', null), user('duplicate', null)], 'actual'), /duplicate session entry id/);
});

test('copied observations isolate nested mutations and preserve JSON __proto__ data', () => {
  const call = assistant('call-entry', null, ['read']);
  call.value.Assistant.content[0].ToolCall.arguments_json = '{"__proto__":{"nested":["kept"]}}';
  const entries = [call, result('result-entry', 'call-entry')];
  for (const read of [() => sessionEntryCopies(entries, 'actual')[0], () => sessionEntryCopy(entries, 'actual', 'call-entry')]) {
    const first = read();
    const args = first.message.content[0].arguments;
    assert.equal(Object.getPrototypeOf(args), Object.prototype);
    assert.equal(Object.hasOwn(args, '__proto__'), true);
    args.__proto__.nested[0] = 'changed';
    first.message.content.push({ type: 'text', text: 'changed' });
    assert.deepEqual(read().message.content[0].arguments.__proto__.nested, ['kept']);
    assert.equal(read().message.content.length, 1);
  }
});

test('cached history copies isolate custom data, tool details and appended observations', () => {
  const entries = [assistant('call', null, ['read']), result('result', 'call')];
  entries[1].metadata = { tool_output: { metadata: { pi_details: { nested: ['kept', null] } } } };
  entries.push({ id: 'private', parent: 'result', value: { type: 'extension' },
    metadata: { extension_metadata: { actual: { provenance: { extension: 'actual' },
      value: { entry_type: 'memo', data: { nested: [{ text: 'large immutable text '.repeat(512) }] } } } } } });
  const first = sessionEntryCopies(entries, 'actual');
  first[1].message.details.nested[0] = 'changed';
  first[2].data.nested[0].text = 'changed';
  first.splice(0, 1);
  const second = sessionEntryCopies(entries, 'actual');
  assert.deepEqual(second[1].message.details, { nested: ['kept', null] });
  assert.equal(second[2].data.nested[0].text, 'large immutable text '.repeat(512));
  const privateCopy = sessionEntryCopy(entries, 'actual', 'private');
  privateCopy.data.nested.push('changed');
  assert.equal(sessionEntryCopy(entries, 'actual', 'private').data.nested.length, 1);
  assert.equal(second.length, 3);
  entries.push(user('appended', 'private'));
  assert.deepEqual(sessionEntryCopies(entries, 'actual').map(entry => entry.id), ['call', 'result', 'private', 'appended']);
  assert.equal(sessionEntryCopy(entries, 'actual', 'appended').message.content[0].text, 'message');
});

test('copied history never acquires native opaque replay provenance', () => {
  for (const [protocol, kind] of [['open_ai_responses', { OpenAiReasoning: { item_id: 'rs_1', encrypted_content: 'opaque' } }],
    ['anthropic_messages', { AnthropicRedacted: { data: 'opaque' } }]]) {
    const state = { model: 'model', protocol, kind };
    const entry = { id: 'opaque', parent: null, value: { type: 'message', Assistant: { model: 'model', protocol,
      content: [{ Reasoning: { text: 'visible', state } }] } } };
    const before = structuredClone(entry), entries = [entry];
    for (const read of [() => sessionEntryCopies(entries, 'actual')[0], () => sessionEntryCopy(entries, 'actual', 'opaque')]) {
      const observed = read().message;
      assert.equal(typeof observed.content[0].thinkingSignature, 'string');
      assert.throws(() => piToCanonical([observed]), /unsupported_feature/);
      assert.throws(() => piToCanonical([JSON.parse(JSON.stringify(observed))]), /unsupported_feature/);
      observed.content[0].thinkingSignature = 'changed';
      assert.notEqual(read().message.content[0].thinkingSignature, 'changed');
    }
    assert.deepEqual(entry, before);
    const foreign = structuredClone(entry);
    foreign.value.Assistant.content[0].Reasoning.state.model = 'foreign';
    const message = sessionEntryCopies([foreign], 'actual')[0].message;
    assert.equal(Object.hasOwn(message.content[0], 'thinkingSignature'), false);
    assert.deepEqual(piToCanonical([message])[0].Assistant.content, [{ Reasoning: { text: 'visible', state: null } }]);
  }
});

test('unchanged indexed reads and one-entry append maintenance stay flat as history grows', t => {
  const work = [];
  for (const count of [16, 160, 1600]) {
    let reads = 0;
    const entries = Array.from({ length: count }, (_, i) => {
      const entry = user(`e${i}`, i ? `e${i - 1}` : null), value = entry.value;
      Object.defineProperty(entry, 'value', { enumerable: true, get() { reads++; return value; } });
      return entry;
    });
    sessionEntryCopy(entries, 'actual', `e${count - 1}`); // bootstrap
    reads = 0;
    // A lookup of the first ID must not mask a linear .find lookup of later IDs.
    let idReads = 0;
    const first = sessionEntryCopies(entries, 'actual');
    for (const entry of entries) {
      const id = entry.id;
      Object.defineProperty(entry, 'id', { enumerable: true, get() { idReads++; return id; } });
    }
    let searched = 0;
    const find = Array.prototype.find;
    Array.prototype.find = function (predicate, thisArg) { return find.call(this, (...args) => { searched++; return predicate.apply(thisArg, args); }); };
    try {
      for (let i = 0; i < 10; i++) assert.equal(sessionEntryCopy(entries, 'actual', `e${count - 1}`).id, `e${count - 1}`);
    } finally { Array.prototype.find = find; }
    assert.equal(searched, 0, 'single-entry lookups use the index, not a translation scan');
    entries.push(user('new', `e${count - 1}`));
    assert.equal(sessionEntryCopy(entries, 'actual', 'new').id, 'new');
    assert.equal(first.length, count, 'retained copies do not grow');
    t.diagnostic(`${count} entries: old value reads=${reads} old ID reads=${idReads}`);
    work.push(reads + idReads);
  }
  assert.ok(work.every(reads => reads < 40), `old history revisited: ${work}`);
});

test('append cache preserves nearest tool names, forks, namespaces and replacement arrays', () => {
  const entries = [assistant('call', null, ['root']), result('result', 'call')];
  sessionEntryCopy(entries, 'actual', 'result');
  entries.push(assistant('near', 'result', ['near-first', 'near-second']), result('near-result', 'near'));
  assert.equal(sessionEntryCopy(entries, 'actual', 'near-result').message.toolName, 'near-first');
  entries.push(result('fork', 'call'));
  assert.equal(sessionEntryCopy(entries, 'actual', 'fork').message.toolName, 'root');
  const privateEntry = { id: 'private', parent: 'fork', value: { type: 'extension' }, metadata: { extension_metadata: {
    actual: { provenance: { extension: 'actual' }, value: { entry_type: 'memo', data: { kept: true } } },
  } } };
  entries.push(privateEntry);
  assert.equal(sessionEntryCopy(entries, 'actual', 'private').type, 'custom');
  assert.equal(sessionEntryCopy(entries, 'other', 'private').type, 'octet_native');
  const replacement = [user('call', null, [{ Text: 'replacement' }])];
  assert.equal(sessionEntryCopy(replacement, 'actual', 'call').message.role, 'user');
  assert.equal(sessionEntryCopy(entries, 'actual', 'call').message.role, 'assistant');
});
