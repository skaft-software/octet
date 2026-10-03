import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { Extension, UnsupportedFeatureError } from '../process/index.mjs';
import { schema, matches } from '../process/schema.mjs';
import { harness, initialize, request, tool, context } from './harness.mjs';

const bounded = {timeout: 10_000};
test('real process: exact negotiation, split UTF-8 frames, tool, progress, command and shutdown', bounded, async t => {
  const h = harness(t);
  const init = Buffer.from(JSON.stringify(request(1, 'initialize', initialize())) + '\n');
  for (let offset = 0; offset < init.length; offset += 7) h.child.stdin.write(init.subarray(offset, offset + 7));
  const selected = (await h.reply(1)).result;
  assert.equal(selected.api_version, '0.4');
  assert.deepEqual(selected.protocol.features, ['request_cancellation', 'content_parts', 'request_progress']);
  assert.equal(selected.protocol.limits.max_concurrent_requests, 2);
  assert.equal(selected.tools[0].parameters.type, 'object');
  const frame = Buffer.from(JSON.stringify(tool(2, {text: 'héllo 🦀', mode: 'progress'})) + '\r\n');
  for (const byte of frame) h.child.stdin.write(Buffer.from([byte]));
  assert.deepEqual((await h.reply(2)).result, {content: [{type: 'text', text: 'héllo 🦀:1'}], is_error: false});
  assert.deepEqual(h.frames.filter(f => f.method === '$/progress').map(f => f.params.sequence), [1, 2]);
  h.send(request(3, 'command/execute', {name: 'test-command', arguments: ['one', 'two'], context}));
  assert.equal((await h.reply(3)).result.text, 'one,two@/local/workspace');
  h.send(tool(4, {mode: 'owner'}));
  assert.deepEqual(JSON.parse((await h.reply(4)).result.content[0].text), context.resource_owner);
  await h.stop();
  assert.match(h.stderr(), /diagnostic from author/); assert.match(h.stderr(), /shutdown:shutdown/);
});

test('unoffered progress is explicit; unsupported hooks/structured/media features are not selected', bounded, async t => {
  const h = harness(t);
  const p = initialize(); p.protocol.optional_features = [];
  await h.ready(p);
  h.send(tool(2)); assert((await h.reply(2)).result);
  h.send(tool(3, {mode: 'progress'})); assert.equal((await h.reply(3)).error.code, -32603);
  assert.equal(h.frames.filter(f => f.method === '$/progress').length, 0);
  h.send(request(4, 'hook/run', {hook: 'before_prompt', payload: {}, context}));
  assert.equal((await h.reply(4)).error.code, -32601);
  await h.stop();
  assert.throws(() => new Extension().hook('before_prompt', () => {}), UnsupportedFeatureError);
});

test('expected tool failures are model-visible; unexpected exceptions, stdout writes and malformed results stay private', bounded, async t => {
  const h = harness(t); await h.ready();
  h.send(tool(2, {mode: 'error'}));
  assert.deepEqual((await h.reply(2)).result, {content: [{type: 'text', text: 'Expected tool failure'}], is_error: true});
  for (const [id, mode] of [[3, 'throw'], [4, 'raw'], [5, 'invalid'], [6, 'large']]) {
    h.send(tool(id, {mode})); assert.equal((await h.reply(id)).error.code, -32603);
  }
  assert(!JSON.stringify(h.frames).includes('PRIVATE-EXCEPTION'));
  assert(!h.stderr().includes('PRIVATE-EXCEPTION'));
  h.send(tool(7)); assert((await h.reply(7)).result); await h.stop();
});

test('cancellation is cooperative, idempotent, single-terminal and leaves unrelated calls usable', bounded, async t => {
  const h = harness(t); await h.ready();
  h.send(tool(2, {ms: 30_000})); await h.progress(2);
  const cancel = {jsonrpc: '2.0', method: '$/cancelRequest', params: {id: 2, reason: 'user'}};
  h.send(cancel); h.send(cancel);
  assert.equal((await h.reply(2)).error.code, -32800);
  h.send(cancel); h.send(tool(3, {text: 'still ready'}));
  assert((await h.reply(3)).result); await h.stop();
  assert.equal(h.frames.filter(frame => frame.id === 2).length, 1);
});

test('queued cancellation never runs the handler; concurrency remains capped', bounded, async t => {
  const h = harness(t); const p = initialize(); p.protocol.limits.max_concurrent_requests = 1;
  const selected = await h.ready(p); assert.equal(selected.protocol.limits.max_concurrent_requests, 1);
  h.send(tool(2, {ms: 100})); await h.progress(2);
  h.send(tool(3, {text: 'queued'}));
  h.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id: 3}});
  assert.equal((await h.reply(3)).error.code, -32800);
  assert.equal((await h.reply(2)).result.content[0].text, 'hello:1');
  h.send(tool(4)); assert.equal((await h.reply(4)).result.content[0].text, 'hello:2');
  await h.stop();
});

test('cancelled noncooperative handlers terminate generation after the host grace', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {mode: 'never'}));
  await delay(30);
  const started = Date.now(); h.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id: 2}});
  assert.equal((await h.exited).code, 1);
  assert(Date.now() - started >= 1900 && Date.now() - started < 3000);
  assert.equal(h.frames.filter(f => f.id === 2).length, 0);
});

test('shutdown cancels in-flight work, rejects admission, acknowledges and exits within signal cap', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {ms: 30_000})); await h.progress(2);
  const start = Date.now();
  h.child.stdin.write(JSON.stringify(request(3, 'shutdown')) + '\n' + JSON.stringify(tool(4)) + '\n');
  assert.equal((await h.reply(2)).error.code, -32800);
  assert.equal((await h.reply(4)).error.code, -32000);
  assert.deepEqual((await h.reply(3)).result, {}); assert.equal((await h.exited).code, 0);
  assert(Date.now() - start < 1400);
});

test('EOF drains cooperatively without inventing shutdown acknowledgement', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {ms: 30_000})); await h.progress(2);
  h.child.stdin.end(); assert.equal((await h.exited).code, 0);
  assert.equal((await h.reply(2)).error.code, -32800);
  assert.match(h.stderr(), /shutdown:transport_lost/);
});

test('stuck shutdown hook is bounded and reports nonclean exit', bounded, async t => {
  const h = harness(t, {env: {STUCK_SHUTDOWN: '1'}}); await h.ready();
  const start = Date.now(); h.send(request(2, 'shutdown'));
  assert.deepEqual((await h.reply(2)).result, {}); assert.equal((await h.exited).code, 1);
  assert(Date.now() - start >= 900 && Date.now() - start < 1400);
});

test('stale progress context cannot emit after settlement', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {mode: 'progress'})); await h.reply(2);
  h.send(tool(3, {mode: 'stale'})); assert.equal((await h.reply(3)).error.code, -32603);
  assert.equal(h.frames.filter(f => f.method === '$/progress' && f.params.request_id === 2).length, 2);
  await h.stop();
});

test('bounded admission refuses the 65th outstanding request without running it', bounded, async t => {
  const h = harness(t); const p = initialize(); p.protocol.limits.max_concurrent_requests = 1; await h.ready(p);
  for (let id = 2; id <= 66; id++) h.send(tool(id, {mode: 'never'}));
  assert.equal((await h.reply(66)).error.code, -32000);
  h.send(request(99, 'shutdown')); await h.reply(99); assert.equal((await h.exited).code, 1);
});

test('writer queue exhaustion terminates instead of buffering unbounded progress', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {mode: 'flood'}));
  assert.equal((await h.exited).code, 1); assert.match(h.stderr(), /writer queue exceeded 128/);
});

test('all Console methods stay on bounded stderr, including dir/table', bounded, async t => {
  const h = harness(t); await h.ready(); h.send(tool(2, {mode: 'diagnostics'}));
  assert((await h.reply(2)).result); await h.stop();
  assert.match(h.stderr(), /console.dir stays off stdout/);
  assert.match(h.stderr(), /console.table stays off stdout/);
  assert(Buffer.byteLength(h.stderr()) <= 65_536);
});

test('EOF discards incomplete final framing without executing it', bounded, async t => {
  const h = harness(t); await h.ready(); h.child.stdin.end(JSON.stringify(tool(2)));
  assert.equal((await h.exited).code, 0); assert.equal(h.frames.filter(frame => frame.id === 2).length, 0);
  assert.match(h.stderr(), /Discarded incomplete final frame/);
});

test('initialization wait is bounded by the actual 30-second host default', {timeout: 35_000}, async t => {
  const started = Date.now(); const h = harness(t);
  assert.equal((await h.exited).code, 1);
  assert(Date.now() - started >= 29_000 && Date.now() - started < 34_000);
  assert.match(h.stderr(), /initialize deadline exceeded/);
});

test('schema validation refuses missing schemas/unsupported keywords and enforces nested argument types', () => {
  const ext = new Extension();
  assert.throws(() => ext.tool({name: 'x', description: 'x'}, () => 'x'));
  assert.throws(() => ext.tool({name: 'x', description: 'x', parameters: {type: 'object', properties: {x: {type: 'string', pattern: '.*'}}}}, () => 'x'), /Unsupported schema/);
  assert.throws(() => ext.tool({name: 'x', description: 'x', parameters: {type: 'object'}, output_schema: {}}, () => 'x'));
  const definition = schema({type: 'object', properties: {items: {type: 'array', items: {type: 'integer'}, minItems: 1}}, required: ['items'], additionalProperties: false});
  assert(matches(definition, {items: [1, 2]}));
  for (const value of [{}, {items: []}, {items: ['2']}, {items: [2], extra: true}]) assert(!matches(definition, value));
});
