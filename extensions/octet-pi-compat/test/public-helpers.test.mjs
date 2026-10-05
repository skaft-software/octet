import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import * as helpers from '../shims/coding-agent.mjs';
import { uuidv7 } from '../shims/ai.mjs';
import { homedir } from 'node:os';

test('documented agent paths and UUIDv7 authoring helpers follow Pi semantics', t => {
  const original = process.env.PI_CODING_AGENT_DIR;
  t.after(() => { if (original === undefined) delete process.env.PI_CODING_AGENT_DIR; else process.env.PI_CODING_AGENT_DIR = original; });
  delete process.env.PI_CODING_AGENT_DIR;
  assert.equal(helpers.CONFIG_DIR_NAME, '.pi');
  assert.equal(helpers.getAgentDir(), join(homedir(), '.pi', 'agent'));
  process.env.PI_CODING_AGENT_DIR = '~/custom';
  assert.equal(helpers.getAgentDir(), join(homedir(), 'custom'));
  process.env.PI_CODING_AGENT_DIR = 'file:///tmp/pi-config';
  assert.equal(helpers.getAgentDir(), '/tmp/pi-config');
  const first = uuidv7(123), second = uuidv7(123);
  assert.match(first, /^00000000-007b-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.ok(first < second);
  for (const value of [-1, 0.5, NaN, 0x1000000000000]) assert.throws(() => uuidv7(value), RangeError);
});

test('Pi guards use toolName for both built-in and custom tool events', () => {
  assert.equal(helpers.isToolCallEventType('my_tool', { toolName: 'my_tool', input: {} }), true);
  assert.equal(helpers.isToolCallEventType('read', { toolName: 'write' }), false);
  for (const name of ['Bash', 'PowerShell', 'Read', 'Edit', 'Write', 'Grep', 'Find', 'Ls']) {
    const toolName = name.toLowerCase();
    assert.equal(helpers[`is${name}ToolResult`]({ toolName }), true);
    assert.equal(helpers[`is${name}ToolResult`]({ toolName: 'custom' }), false);
  }
});

test('frontmatter has Pi BOM/newline, YAML, empty and malformed behavior', () => {
  assert.deepEqual(helpers.parseFrontmatter('\uFEFF---\r\nname: hi\r\nvalues: [1, 2]\r\n---\r\n body '),
    { frontmatter: { name: 'hi', values: [1, 2] }, body: 'body' });
  assert.deepEqual(helpers.parseFrontmatter('---\n---\n'), { frontmatter: {}, body: '' });
  assert.equal(helpers.stripFrontmatter('---\nx: true\n---\nhello'), 'hello');
  assert.deepEqual(helpers.parseFrontmatter('\uFEFFno\r\nfrontmatter'), { frontmatter: {}, body: 'no\nfrontmatter' });
  assert.throws(() => helpers.parseFrontmatter('---\nx: [\n---\nbody'));
});

test('truncation respects complete lines, byte limits, UTF-8 tails and Pi metadata', () => {
  const head = helpers.truncateHead('α\nβ\nγ\n', { maxLines: 1 });
  assert.equal(head.content, 'α'); assert.equal(head.totalLines, 3); assert.equal(head.outputBytes, 2);
  assert.equal(head.truncatedBy, 'lines'); assert.equal(head.lastLinePartial, false);
  assert.equal(helpers.truncateHead('abc\ndef', { maxBytes: 2 }).firstLineExceedsLimit, true);
  const tail = helpers.truncateTail('ab😀', { maxBytes: 4 });
  assert.equal(tail.content, '😀'); assert.equal(tail.lastLinePartial, true);
  assert.equal(helpers.truncateTail('ab😀', { maxBytes: 3 }).content, '');
  assert.deepEqual(helpers.truncateLine('abcdef', 3), { text: 'abc... [truncated]', wasTruncated: true });
  assert.equal(helpers.truncateHead('').totalLines, 0);
  assert.equal(helpers.formatSize(1024), '1.0KB');
});

test('message conversion excludes private details and preserves original LLM messages', () => {
  const user = { role: 'user', content: 'hello', timestamp: 1 };
  const result = helpers.convertToLlm([user,
    { role: 'custom', content: 'hidden', details: { secret: true }, display: false, timestamp: 2 },
    { role: 'bashExecution', command: 'ls', excludeFromContext: true },
    { role: 'branchSummary', summary: 'branch', timestamp: 3 },
  ]);
  assert.equal(result[0], user);
  assert.deepEqual(result[1], { role: 'user', content: [{ type: 'text', text: 'hidden' }], timestamp: 2 });
  assert.equal(result.length, 3); assert.equal('details' in result[1], false);
  assert.equal(helpers.serializeConversation([
    { role: 'assistant', content: [{ type: 'thinking', thinking: 'plan' }, { type: 'text', text: 'answer' },
      { type: 'toolCall', name: 'read', arguments: { path: 'a' } }] },
    { role: 'toolResult', content: [{ type: 'text', text: 'x'.repeat(2001) }] },
  ]).startsWith('[Assistant thinking]: plan\n\n[Assistant]: answer\n\n[Assistant tool calls]: read(path="a")'), true);
});

test('file mutation queue serializes aliases, releases on failure and permits other files', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-pi-helpers-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'file'); await writeFile(path, ''); await symlink(path, join(dir, 'alias'));
  let release, ready;
  const held = new Promise(done => { release = done; });
  const started = new Promise(done => { ready = done; });
  const order = [];
  const first = helpers.withFileMutationQueue(path, async () => { order.push('first'); ready(); await held; throw new Error('expected'); });
  const rejected = assert.rejects(first, /expected/);
  await started;
  const second = helpers.withFileMutationQueue(join(dir, 'alias'), async () => { order.push('second'); return 7; });
  await helpers.withFileMutationQueue(join(dir, 'other'), async () => { order.push('other'); });
  assert.deepEqual(order, ['first', 'other']); release();
  await rejected; assert.equal(await second, 7); assert.deepEqual(order, ['first', 'other', 'second']);
});
