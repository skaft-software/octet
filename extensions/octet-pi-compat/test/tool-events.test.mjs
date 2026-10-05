// Pi 1.0.2 tool_call / tool_result event contracts across the protocol boundary.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

async function fixture(t, source) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-tool-events-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'index.ts');
  await writeFile(path, source);
  return path;
}
async function beforeToolCall(t, source, args) {
  const peer = launch(t, [await fixture(t, source)]);
  await peer.init();
  const reply = await peer.request('hook/run', { hook: 'before_tool_call', payload: { name: 'read', arguments: args }, context: peer.context() }).response;
  await peer.close();
  return reply;
}

test('tool_call: in-place input mutation is returned as replacement arguments', async t => {
  const reply = await beforeToolCall(t, `export default pi => pi.on('tool_call', e => { e.input.path = 'changed'; });`, { path: 'original', offset: 1 });
  assert.ok(!reply.error, JSON.stringify(reply.error));
  assert.deepEqual(reply.result.disposition, { action: 'continue' });
  assert.deepEqual(reply.result.arguments, { path: 'changed', offset: 1 });
});

test('tool_call: later handlers see earlier mutations', async t => {
  const reply = await beforeToolCall(t, `export default pi => {
    pi.on('tool_call', e => { e.input.path = 'first'; });
    pi.on('tool_call', e => { e.input.path = e.input.path + '-second'; });
  };`, { path: 'original' });
  assert.deepEqual(reply.result.arguments, { path: 'first-second' });
});

test('tool_call: unchanged input sends no replacement', async t => {
  const reply = await beforeToolCall(t, `export default pi => pi.on('tool_call', () => undefined);`, { path: 'original' });
  assert.equal(reply.result.arguments, undefined);
});

test('tool_call: block stops later handlers and denies', async t => {
  const reply = await beforeToolCall(t, `export default pi => {
    pi.on('tool_call', () => ({ block: true, reason: 'no' }));
    pi.on('tool_call', e => { e.input.path = 'unreachable'; });
  };`, { path: 'original' });
  assert.deepEqual(reply.result.disposition, { action: 'deny', reason: 'no' });
  assert.equal(reply.result.arguments, undefined);
});

async function afterToolCall(t, source, payload) {
  const peer = launch(t, [await fixture(t, source)]);
  await peer.init();
  const reply = await peer.request('hook/run', { hook: 'after_tool_call', payload: { name: 'read', arguments: { path: 'p' }, output: 'private value', is_error: false, ...payload }, context: peer.context() }).response;
  await peer.close();
  return reply;
}

test('tool_result: content replacement is returned and drops stale structured content', async t => {
  const reply = await afterToolCall(t, `export default pi => pi.on('tool_result', () => ({ content: [{ type: 'text', text: 'redacted' }] }));`, { structured_content: { secret: 1 } });
  assert.ok(!reply.error, JSON.stringify(reply.error));
  assert.deepEqual(reply.result.tool_result, { content: ['redacted'] });
});

test('tool_result: handlers chain and see earlier replacements', async t => {
  const reply = await afterToolCall(t, `export default pi => {
    pi.on('tool_result', e => ({ content: [{ type: 'text', text: e.content[0].text + ' one' }] }));
    pi.on('tool_result', e => ({ content: [{ type: 'text', text: e.content[0].text + ' two' }], isError: true, details: { n: 2 } }));
  };`, {});
  assert.deepEqual(reply.result.tool_result, { content: ['private value one two'], is_error: true, metadata: { pi_details: { n: 2 } } });
});

test('tool_result: event carries details and structured content', async t => {
  const reply = await afterToolCall(t, `export default pi => pi.on('tool_result', e => ({ details: { saw: [e.details, e.structuredContent, e.input.path, e.isError] } }));`,
    { structured_content: { s: 1 }, metadata: { pi_details: { d: 1 } } });
  assert.deepEqual(reply.result.tool_result, { metadata: { pi_details: { saw: [{ d: 1 }, { s: 1 }, 'p', false] } } });
});

test('tool_result: no result leaves the tool result alone', async t => {
  const reply = await afterToolCall(t, `export default pi => pi.on('tool_result', () => undefined);`, {});
  assert.equal(reply.result.tool_result, undefined);
});
