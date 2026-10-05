// Pi 1.0.2 tool_call / tool_result event contracts across the protocol boundary.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';
import { modelTurn } from '../lib/model-turns.mjs';
import { canonicalToPi } from '../lib/provider-context.mjs';

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
  const reply = await peer.request('hook/run', { hook: 'before_tool_call', payload: { name: 'read', arguments: args, tool_call_id: 'call-0', parent_tool_call_id: 'parent-0' }, context: peer.context() }).response;
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

test('tool_call: block carries termination and exact host IDs', async t => {
  const reply = await beforeToolCall(t, `export default pi => pi.on('tool_call', e => ({block:true, reason:e.parentToolCallId + '/' + e.toolCallId, terminate:true}));`, {path:'p'});
  assert.deepEqual(reply.result.disposition, {action:'deny', reason:'parent-0/call-0'});
  assert.equal(reply.result.terminate, true);
});

test('tool_call: terminate alone does not block or stop execution', async t => {
  const reply = await beforeToolCall(t, `export default pi => pi.on('tool_call', () => ({terminate:true}));`, {path:'p'});
  assert.deepEqual(reply.result.disposition, {action:'continue'});
  assert.equal(reply.result.terminate, undefined);
});

const usage = {input:7,output:3,cacheRead:2,cacheWrite:1,totalTokens:13,cost:{input:.7,output:.3,cacheRead:.2,cacheWrite:.1,total:1.3}};
test('tool_result: usage chains, costs remain exact, and existing metadata survives', async t => {
  const reply = await afterToolCall(t, `export default pi => {
    pi.on('tool_result', () => ({usage:${JSON.stringify(usage)}}));
    pi.on('tool_result', e => ({details:{usage:e.usage,id:e.toolCallId,parent:e.parentToolCallId}}));
  };`, {tool_call_id:'nested-1',parent_tool_call_id:'parent-1', metadata:{native:{keep:true}}});
  assert.deepEqual(reply.result.tool_result.metadata, {native:{keep:true},pi_usage:usage,pi_details:{usage,id:'nested-1',parent:'parent-1'}});
  assert.deepEqual(reply.result.tool_result.usage, {input_tokens:7,output_tokens:3,cache_read_tokens:2,cache_write_tokens:1,cache_write_1h_tokens:0,reasoning_tokens:0,total_tokens:13});
});

test('tool_result: incoming ordered images chain and publish only owned artifact references', async t => {
  const image = {type:'image',mimeType:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII='};
  const peer = launch(t, [await fixture(t, `export default pi => {
    pi.on('tool_result', e => ({content:[{type:'text',text:'before'},...e.content,{type:'text',text:'after'}]}));
    pi.on('tool_result', e => ({details:{types:e.content.map(p=>p.type)}}));
  };`)]);
  await peer.init(['session_entries','artifacts']);
  const request = peer.request('hook/run', {hook:'after_tool_call',payload:{name:'read',arguments:{path:'p'},output:'',is_error:false,pi_content:[image]},context:peer.context()});
  const publish = await peer.wait(frame => frame.method === 'artifact/publish');
  assert.equal(publish.params.parent_request_id, request.id);
  assert.equal(publish.params.data.data, image.data);
  peer.send({jsonrpc:'2.0',id:publish.id,result:{artifact_id:'host-owned-artifact'}});
  const reply = await request.response;
  assert.deepEqual(reply.result.tool_result.content, [{type:'text',text:'before'},{type:'image',artifact_id:'host-owned-artifact',mime_type:'image/png'},{type:'text',text:'after'}]);
  assert.deepEqual(reply.result.tool_result.metadata.pi_details.types, ['text','image','text']);
  await peer.close();
});

test('background native settlement: observes the complete paired result without changing native scheduling', async () => {
  const assistant = { id:'assistant', parent:null, timestamp_unix_ms:1000,
    value:{type:'message',Assistant:{model:'scripted',protocol:'open_ai_responses',content:[
      {ToolCall:{id:'call-0',name:'read',arguments_json:'{"path":"original.txt"}',async:true}},
    ]}} };
  const result = { id:'result', parent:'assistant', timestamp_unix_ms:1100,
    value:{type:'message',User:{content:[{ToolResult:{tool_call_id:'call-0',content:[{Text:'FINAL_REDACTION'}],is_error:false}}]}} };
  const before = structuredClone(assistant), observed = [];
  const runtime = {
    require(feature) { assert.equal(feature, 'session_entries'); },
    metadata: () => ({hooks:['model_turn_end']}),
    bind(_params, store) { store.leaf = {grant:{}}; },
    assertOwner() {}, queued: (_store, work) => work(),
    runEvent: async (type, event) => { assert.equal(type,'turn_end'); observed.push(event); },
    flush: async () => {},
  };
  const reply = await modelTurn(runtime, {hook:'model_turn_end',context:{resource_owner:owner},payload:{
    kind:'model_turn_end',run_id:'actual-run',turn_index:0,timestamp_ms:1200,
    assistant_entry:assistant,tool_result_entries:[result],
  }}, {controller:new AbortController()});
  assert.deepEqual(reply.session_operation, {action:'continue'});
  assert.equal(observed.length, 1);
  assert.deepEqual(observed[0].message.content, [{type:'toolCall',id:'call-0',name:'read',arguments:{path:'original.txt'}}]);
  assert.deepEqual(observed[0].toolResults, [{role:'toolResult',toolCallId:'call-0',toolName:'read',content:[{type:'text',text:'FINAL_REDACTION'}],isError:false,timestamp:1100}]);
  assert.deepEqual(assistant, before); // Original durable async scheduling stays intact.
  assert.throws(() => canonicalToPi([{Assistant:assistant.value.Assistant}]), /scheduling/);
});

test('tool_result: invalid usage is rejected at the boundary', async t => {
  const reply = await afterToolCall(t, `export default pi => pi.on('tool_result', () => ({usage:{...${JSON.stringify(usage)},input:-1}}));`, {});
  assert.equal(reply.error.code, -32602);
});
