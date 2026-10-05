import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { stripTypeScriptTypes } from 'node:module';
import { join } from 'node:path';
import { calculateContextTokens, estimateTokens, buildSessionContext } from '../lib/context.mjs';
import { root, launch } from './helper.mjs';

const timestamp = '2026-01-01T00:00:00.000Z';
const entry = (id, parentId, value) => ({ id, parentId, timestamp, ...value });
const message = (id, parentId, role, content, extra = {}) => entry(id, parentId, {
  type: 'message', message: { role, content, ...extra },
});
const usageCases = [
  { input: 4, output: 3, cacheRead: 2, cacheWrite: 1, totalTokens: 15 },
  { input: 4, output: 3, cacheRead: 2, cacheWrite: 1, totalTokens: 0 },
  { input: 4, output: 3, cacheRead: 2, cacheWrite: 1 },
];
const messages = [
  { role: 'system', content: 'hi', sections: { tools: '1234', empty: '', missing: undefined },
    toolsAdded: [{ name: 'read' }], toolsRemoved: ['ignored by Pi heuristic'] },
  { role: 'user', content: 'hello' },
  { role: 'user', content: [{ type: 'text', text: '1234' }, { type: 'image', data: 'x' }] },
  { role: 'assistant', content: [{ type: 'thinking', thinking: '1234' }, { type: 'text', text: 'abc' },
    { type: 'toolCall', name: 'read', arguments: { path: 'a' } }] },
  { role: 'toolResult', content: 'hello' },
  { role: 'custom', content: [{ type: 'image' }] },
  { role: 'bashExecution', command: 'echo', output: '1234' },
  { role: 'branchSummary', summary: 'short' },
  { role: 'compactionSummary', summary: 'longer' },
  { role: 'futureRole', content: 'not counted' },
];
const branch = [
  entry('level', null, { type: 'thinking_level_change', thinkingLevel: 'high' }),
  entry('model', 'level', { type: 'model_change', provider: 'one', modelId: 'first' }),
  message('u', 'model', 'user', 'original'),
  message('a', 'u', 'assistant', [{ type: 'text', text: 'answer' }], { provider: 'two', model: 'second' }),
  entry('edit', 'a', { type: 'context_edit', targetId: 'u', replacement: { content: 'changed' } }),
  message('sibling', 'u', 'user', 'other branch'),
];
const compaction = [
  message('sys', null, 'system', 'old system'),
  message('u', 'sys', 'user', 'old user'),
  entry('c1', 'u', { type: 'compaction', summary: 'old summary', tokensBefore: 40, firstKeptEntryId: 'u',
    systemMessage: { role: 'system', content: 'old checkpoint' } }),
  message('tool', 'c1', 'toolResult', [{ type: 'text', text: 'tool output' }]),
  entry('c2', 'tool', { type: 'compaction', summary: 'new summary', tokensBefore: 80, firstKeptEntryId: 'u',
    systemMessage: { role: 'system', content: 'new checkpoint' } }),
  entry('edit', 'c2', { type: 'context_edit', targetId: 'u', replacement: { content: 'new user' } }),
];

test('Pi context usage totals and chars/4 estimates are pure, not live budget accounting', () => {
  assert.deepEqual(usageCases.map(calculateContextTokens), [15, 10, 10]);
  assert.deepEqual(messages.map(estimateTokens), [6, 2, 1201, 6, 2, 1200, 2, 2, 2, 0]);
});

test('Pi session projection selects the leaf path and derives model/thinking settings', () => {
  assert.deepEqual(buildSessionContext([]), { messages: [], thinkingLevel: 'off', model: null });
  assert.deepEqual(buildSessionContext(branch, null), buildSessionContext([]));
  const before = structuredClone(branch);
  const projected = buildSessionContext(branch, 'edit');
  assert.deepEqual(projected.messages, [{ role: 'user', content: 'changed' }, branch[3].message]);
  assert.equal(projected.thinkingLevel, 'high');
  assert.deepEqual(projected.model, { provider: 'two', modelId: 'second' });
  assert.equal(buildSessionContext(branch).messages[0].content, 'original');
  assert.deepEqual(buildSessionContext(branch, 'missing'), buildSessionContext(branch));
  assert.deepEqual(buildSessionContext(branch, 'edit', new Map(branch.map(e => [e.id, e]))), projected);
  assert.deepEqual(branch, before);
});

test('Pi projection keeps only the newest compaction checkpoint and applies retained context edits', () => {
  const result = buildSessionContext(compaction);
  assert.deepEqual(result.messages, [
    { role: 'system', content: 'new checkpoint' },
    { role: 'compactionSummary', summary: 'new summary', tokensBefore: 80, timestamp: Date.parse(timestamp) },
    { role: 'user', content: 'new user' }, compaction[3].message,
  ]);
  const withoutKept = structuredClone(compaction);
  withoutKept[4].firstKeptEntryId = 'absent';
  assert.equal(buildSessionContext(withoutKept).messages.length, 2);
});

test('Pi projection normalizes old content and handles custom/summary entries without mutating inputs', () => {
  const entries = [
    message('sys', null, 'system', null),
    message('u', 'sys', 'user', undefined),
    entry('private', 'u', { type: 'custom', customType: 'state', data: 'not model context' }),
    entry('custom', 'private', { type: 'custom_message', customType: 'ui', display: false, details: { x: 1 } }),
    entry('summary', 'custom', { type: 'branch_summary', summary: 'branch', fromId: 'u' }),
  ];
  assert.deepEqual(buildSessionContext(entries).messages, [
    { role: 'system', content: '' }, { role: 'user', content: [] },
    { role: 'custom', customType: 'ui', content: [], display: false, details: { x: 1 }, timestamp: Date.parse(timestamp) },
    { role: 'branchSummary', summary: 'branch', fromId: 'u', timestamp: Date.parse(timestamp) },
  ]);
  assert.equal(entries[0].message.content, null);
});

test('Pi context edits use replacement.content, preserve metadata, and last edit wins', () => {
  const entries = [message('a', null, 'assistant', [], { provider: 'p', model: 'm', stopReason: 'stop' })];
  entries.push(entry('drop', 'a', { type: 'context_edit', targetId: 'a', replacement: null }));
  assert.deepEqual(buildSessionContext(entries).messages, []);
  entries.push(entry('restore', 'drop', { type: 'context_edit', targetId: 'a', replacement: { content: 'new' } }));
  assert.deepEqual(buildSessionContext(entries).messages, [{ ...entries[0].message, content: [{ type: 'text', text: 'new' }] }]);
  entries.push(entry('replace', 'restore', { type: 'context_edit', targetId: 'a', replacement: { content: [{ type: 'text', text: 'array' }] } }));
  assert.deepEqual(buildSessionContext(entries).messages[0].content, [{ type: 'text', text: 'array' }]);
});

test('both coding-agent aliases expose working pure helpers at factory time and over the raw process', async t => {
  const peer = launch(t, [join(root, 'test/fixtures/context.ts')]);
  assert.equal(peer.metadata.tools[0].description, 'Pure helper import estimate: 2');
  await peer.init();
  const response = await peer.request('tool/call', { name: 'context_helpers', arguments: {}, context: peer.context() }).response;
  assert.ok(response.result, JSON.stringify(response));
  assert.deepEqual(JSON.parse(response.result.content[0].text), {
    tokens: 14, estimate: 1200, context: { messages: [{ role: 'user', content: 'hello' }], thinkingLevel: 'off', model: null },
  });
  await peer.close();
});

// Optional, offline differential oracle. Extract only the pinned pure function
// declarations, never import/execute upstream's agent, session store, or SDK.
const referenceRepo = process.env.PI_REFERENCE_REPO;
test('pure helper results match source-extracted Pi 1.0.2 reference functions', {
  skip: !referenceRepo && 'set PI_REFERENCE_REPO to a local reviewed Pi checkout containing the pinned commit',
}, async () => {
  const ref = 'cd32f7725fdbddbaecdff5b1e68491563394e0ca';
  function source(path, hash) {
    const text = execFileSync('git', ['-C', referenceRepo, 'show', `${ref}:packages/coding-agent/src/core/${path}.ts`], { encoding: 'utf8', timeout: 10000 });
    assert.equal(createHash('sha256').update(text).digest('hex'), hash);
    return text;
  }
  function section(text, start, end) {
    const at = text.indexOf(start), until = text.indexOf(end, at);
    assert.ok(at >= 0 && until > at, `missing pinned function boundary: ${start}`);
    return text.slice(at, until);
  }
  const comp = source('compaction/compaction', 'd5aebd41333957b57fa3f1bee2a18b3c00b5d47bb1b4af6f913cf8cd791099c8');
  const session = source('session-manager', '450d82c529933214e088b8422f00061815e617f352f6c834689787a314064bff');
  const custom = source('messages', '5397e3c96c9504266e4ee8b6e88d5098b3280ee7dbb0c687b74708c62a06c8d4');
  const pure = [
    section(comp, 'export function calculateContextTokens', '/**\n * Get usage'),
    section(comp, 'const ESTIMATED_IMAGE_CHARS', 'function isCutPointMessage'),
    section(custom, 'export function createBranchSummaryMessage', '/**\n * Transform AgentMessages'),
    section(session, 'function buildEntryIndex', '/**\n * Compute the default session directory'),
  ].join('\n');
  const js = stripTypeScriptTypes(pure);
  assert.doesNotMatch(js, /\bimport\s/);
  const oracle = await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);
  for (const usage of usageCases) assert.equal(calculateContextTokens(usage), oracle.calculateContextTokens(usage));
  for (const msg of messages) assert.equal(estimateTokens(msg), oracle.estimateTokens(msg));
  // Different leaves, retained compactions, absent content, arbitrary branches,
  // and edits of all entry kinds. These are pure arrays, never host transactions.
  let seed = 17;
  const random = n => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % n; };
  const generated = [];
  for (let i = 0; i < 160; i++) {
    const parent = i ? `g${random(i)}` : null;
    const id = `g${i}`;
    const kinds = [
      { type: 'message', message: messages[random(messages.length - 1)] },
      { type: 'context_edit', targetId: `g${random(Math.max(i, 1))}`, replacement: random(3) ? { content: `edit ${i}` } : null },
      { type: 'compaction', summary: `summary ${i}`, tokensBefore: i, firstKeptEntryId: parent },
      { type: 'custom_message', customType: 'test', content: 'custom', display: false },
      { type: 'branch_summary', summary: 'branch', fromId: parent },
      { type: 'thinking_level_change', thinkingLevel: 'medium' },
    ];
    generated.push(entry(id, parent, kinds[random(kinds.length)]));
  }
  for (const entries of [[], branch, compaction, generated]) {
    for (const leaf of [undefined, null, 'absent', ...entries.map(e => e.id)]) {
      assert.deepEqual(buildSessionContext(entries, leaf), oracle.buildSessionContext(entries, leaf), `leaf ${leaf}`);
      const index = new Map(entries.map(e => [e.id, e]));
      assert.deepEqual(buildSessionContext(entries, leaf, index), oracle.buildSessionContext(entries, leaf, index));
    }
  }
});
