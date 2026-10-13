import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { harness, initialize, request, tool, context } from './harness.mjs';
const bounded = {timeout: 10_000};

test('malformed envelopes/params are rejected without dispatch and transport remains usable', bounded, async t => {
  const h = harness(t);
  h.send(tool(10)); assert.equal((await h.reply(10)).error.code, -32600);
  await h.ready();
  h.child.stdin.write('{broken JSON\n'); assert.equal((await h.reply(null)).error.code, -32700);
  for (const [id, message, expected] of [
    [2, {...tool(2), jsonrpc: '1.0'}, -32600],
    [3, {...tool(3), result: {}}, -32600],
    [4, tool(4, {text: 3}), -32602],
    [5, tool(5, {ms: -1}), -32602],
    [6, tool(6, {ms: 1.5}), -32602],
    [7, tool(7, {extra: 'not declared'}), -32602],
    [8, tool(8, {}, {context: []}), -32602],
    [9, tool(9, {}, {catalog_revision: 0}), -32602],
    [11, request(11, 'command/execute', {name: 'test-command', arguments: [false], context}), -32602],
    [12, request(12, 'shutdown', null), -32602],
    [13, request(13, 'shutdown', {extra: 1}), -32602],
    [14, tool(14, null), -32602],
    [15, tool(15, {}, {name: 'unknown'}), -32601],
    [16, request(16, 'unknown/method'), -32601],
    [17, request(17, 'initialize', initialize()), -32600],
    [18, tool(18, {}, {context: {...context, resource_owner: {session_id: 'x', extension_instance_id: 'x', process_generation: -1}}}), -32602],
  ]) { h.send(message); assert.equal((await h.reply(id)).error.code, expected); }
  h.send(tool(19)); assert((await h.reply(19)).result); await h.stop();
});

const invalidOffers = [
  ['canonical API is not translated', p => { p.api_version = '0.3'; }],
  ['protocol exact selection', p => { p.protocol.version = '0.2'; }],
  ['unknown required feature', p => { p.protocol.required_features.push('made_up'); }],
  ['missing required feature', p => { p.protocol.required_features.pop(); }],
  ['duplicate features', p => { p.protocol.optional_features.push('request_progress'); }],
  ['required optional overlap', p => { p.protocol.optional_features.push('content_parts'); }],
  ['zero concurrency', p => { p.protocol.limits.max_concurrent_requests = 0; }],
  ['fractional concurrency', p => { p.protocol.limits.max_concurrent_requests = 1.5; }],
  ['wrong concurrency type', p => { p.protocol.limits.max_concurrent_requests = true; }],
  ['mismatched tool catalog', p => { p.contributes.tools = []; }],
  ['duplicate tool catalog', p => { p.contributes.tools.push('test_tool'); }],
  ['mismatched command catalog', p => { p.contributes.commands.push('absent'); }],
  ['unsupported hook declaration', p => { p.contributes.hooks.push('before_prompt'); }],
  ['unsupported presentation declaration', p => { p.contributes.presentation = true; }],
  ['malformed contributes', p => { p.contributes = null; }],
];
for (const [name, mutate] of invalidOffers) test(`initialize rejects ${name}`, bounded, async t => {
  const h = harness(t); const params = initialize(); mutate(params);
  h.send(request(1, 'initialize', params)); assert((await h.reply(1)).error);
  h.child.stdin.end(); assert.equal((await h.exited).code, 0);
});

test('full frame at 1 MiB including LF is accepted; one byte more terminates', bounded, async t => {
  const h = harness(t); await h.ready();
  const frame = JSON.stringify(tool(2));
  h.child.stdin.write(frame + ' '.repeat(1_048_575 - Buffer.byteLength(frame)) + '\n');
  assert((await h.reply(2)).result); await h.stop();
  const oversized = harness(t); await oversized.ready();
  const frame2 = JSON.stringify(tool(3));
  oversized.child.stdin.write(frame2 + ' '.repeat(1_048_576 - Buffer.byteLength(frame2)) + '\n');
  assert.equal((await oversized.exited).code, 1);
  assert.match(oversized.stderr(), /frame exceeds/);
});

test('invalid UTF-8 is terminal rather than replacement-decoded', bounded, async t => {
  const h = harness(t); await h.ready(); h.child.stdin.write(Buffer.from([0x22, 0xff, 0x22, 0x0a]));
  assert.equal((await h.exited).code, 1); assert.match(h.stderr(), /invalid UTF-8/);
});

test('UTF-8 BOM is not silently stripped from a JSON frame', bounded, async t => {
  const h = harness(t); await h.ready();
  h.child.stdin.write(Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from(JSON.stringify(tool(2)) + '\n')]));
  assert.equal((await h.reply(null)).error.code, -32700);
  assert.equal(h.frames.filter(frame => frame.id === 2).length, 0);
  h.send(tool(3)); assert((await h.reply(3)).result); await h.stop();
});

test('nonportable numeric values, depth and lone surrogates fail closed', bounded, async t => {
  const h = harness(t); await h.ready();
  h.child.stdin.write('{"jsonrpc":"2.0","id":2,"method":"tool/call","params":{"n":1e400}}\n');
  assert.equal((await h.reply(2)).error.code, -32600);
  h.child.stdin.write('{"jsonrpc":"2.0","id":3,"method":"tool/call","params":{"n":9007199254740992}}\n');
  assert.equal((await h.reply(3)).error.code, -32600);
  h.child.stdin.write('{"jsonrpc":"2.0","id":4,"method":"tool/call","params":{"n":"\\ud800"}}\n');
  assert.equal((await h.reply(4)).error.code, -32600);
  h.child.stdin.write('{"jsonrpc":"2.0","id":5,"method":"tool/call","params":' + '['.repeat(34) + '0' + ']'.repeat(34) + '}\n');
  assert.equal((await h.reply(5)).error.code, -32600); await h.stop();
});

test('duplicate active IDs terminate rather than settling the wrong original waiter', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {ms: 30_000})); await h.progress(2); h.send(tool(2));
  assert.equal((await h.exited).code, 1); assert.match(h.stderr(), /duplicate active/);
});

const duplicateMalformedFrames = [
  ['malformed ordinary envelope', () => ({...tool(2), jsonrpc: '1.0'})],
  ['invalid JSON value boundary', () => tool(2, {text: '\ud800'})],
  ['cancel request reuse', () => request(2, '$/cancelRequest', {id: 2, reason: 'user'})],
];
for (const [name, message] of duplicateMalformedFrames) test(`active ID ownership precedes ${name} rejection`, bounded, async t => {
  const h = harness(t); await h.ready();
  h.send(tool(2, {ms: 80})); await h.progress(2);
  h.send(message()); await delay(150);
  // Duplicate envelope IDs are terminal transport errors, never a fabricated
  // response on the live original ID. Only that original handler started.
  assert.equal(h.frames.filter(frame => frame.method === '$/progress' && frame.params.request_id === 2).length, 1);
  assert.equal(h.frames.filter(frame => frame.id === 2).length, 0);
  assert.equal(h.child.exitCode, 1); assert.match(h.stderr(), /duplicate active/);
});

test('valid cancellation notifications share only the target ID and settle the tool exactly once', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {ms: 30_000})); await h.progress(2);
  const notification = {jsonrpc: '2.0', method: '$/cancelRequest', params: {id: 2, reason: 'user'}};
  h.send(notification); h.send(notification);
  assert.equal((await h.reply(2)).error.code, -32800);
  h.send(tool(3)); assert.equal((await h.reply(3)).result.content[0].text, 'hello:2');
  await h.stop();
  assert.equal(h.frames.filter(frame => frame.id === 2).length, 1);
  assert.equal(h.frames.filter(frame => frame.method === '$/progress' && frame.params.request_id === 2).length, 1);
});

test('invalid cancellation cannot cancel unrelated work', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {ms: 30})); await h.progress(2);
  // String child IDs are now valid but never alias the numeric host ID.
  h.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id: '2'}});
  h.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id: false}});
  assert((await h.reply(2)).result); await h.stop();
  assert.match(h.stderr(), /Invalid cancellation/);
});
