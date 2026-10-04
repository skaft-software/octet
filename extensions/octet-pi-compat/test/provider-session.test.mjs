import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';
import { translateSessionEntries } from '../lib/session-mirror.mjs';
const pipeline = join(root, 'test/fixtures/provider-pipeline.ts');
const operations = join(root, 'test/fixtures/session-operations.ts');
const model = { id: 'native-model', provider: 'native-provider', api: 'openai-chat' };
const grant = { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'session-operation:2', owner, expected_head: 'kept' };
const entry = { id: 'kept', parent: null, timestamp_unix_ms: 1234, value: { type: 'message', User: { content: [{ Text: 'kept text' }] } } };
const operation = (peer, hook, payload, mode) => ({ hook, payload, session_leaf: grant, context: peer.context({ session_name: mode, session_entries: [entry], session_branch: [entry], session_leaf_id: 'kept' }) });

test('actual encoded phases: replacement, ordered in-place mutation, exact header deletion patch and response arrival', async t => {
  const peer = launch(t, [pipeline]); await peer.init(['pipeline_hooks_v1']);
  const params = (hook, body) => ({ hook, context: peer.context(), payload: { operation_id: 'actual-attempt', model, ...body } });
  assert.deepEqual((await peer.request('hook/run', params('before_provider_request', { payload: { model: 'wire-model', max_output_tokens: 1, input: ['encoded'] } })).response).result,
    { provider_payload: { model: 'wire-model', max_output_tokens: 256, input: ['encoded'], sequence: ['first', 'second'] } });
  assert.deepEqual((await peer.request('hook/run', params('before_provider_headers', { headers: { 'x-keep': 'unchanged', 'x-remove': 'old', 'x-null': 'old' } })).response).result,
    { provider_headers: { 'x-remove': null, 'x-null': null, 'x-added': ['first', 'second'] } });
  assert.deepEqual((await peer.request('hook/run', params('after_provider_response', { status: 429, headers: { 'retry-after': '2' } })).response).result, {});
  await peer.wait(frame => frame.method === 'notification' && frame.params.message === 'actual-arrival:429'); await peer.close();
});
test('private provider callback exception is redacted, not emitted with wire body', async t => {
  const peer = launch(t, [pipeline]); await peer.init(['pipeline_hooks_v1']);
  const reply = await peer.request('hook/run', { hook: 'before_provider_request', context: peer.context(), payload: { operation_id: 'attempt', model, payload: { secret: 'NEVER-LOG-ME' } } }).response;
  assert.deepEqual(reply.result, { provider_payload: { secret: 'NEVER-LOG-ME' } });
  assert.ok(!peer.stderr().includes('NEVER-LOG-ME'));
  assert.ok(peer.seen.filter(frame => frame.method === 'notification').every(frame => !JSON.stringify(frame).includes('NEVER-LOG-ME'))); await peer.close();
});
test('provider pipeline cancellation has no late success or header/payload publication', async t => {
  const peer = launch(t, [pipeline], { hold: ['confirmation/request'] }); await peer.init(['pipeline_hooks_v1']);
  const request = peer.request('hook/run', { hook: 'before_provider_request', context: peer.context({ session_name: 'wait' }), payload: { operation_id: 'attempt', model, payload: {} } });
  const call = await peer.wait(frame => frame.method === 'confirmation/request'); peer.notify('$/cancelRequest', { id: request.id });
  assert.equal((await request.response).error.code, -32800);
  peer.send({ jsonrpc: '2.0', id: call.id, result: { confirmed: true } }); await peer.close();
});
for (const reason of ['manual', 'threshold', 'overflow']) test(`real session compaction ${reason}: replacement/veto rather than advisory no-op`, async t => {
  const peer = launch(t, [operations]); await peer.init(['session_entries']);
  const payload = { kind: 'before_compact', reason, first_kept: 'kept', preparation: { messages: [], turn_prefix_messages: [], previous_summary: null, details: { read_files: [], modified_files: [] } }, branch_entries: [entry], custom_instructions: null };
  const reply = await peer.request('hook/run', operation(peer, 'session_before_compact', payload)).response;
  assert.deepEqual(reply.result.session_operation, reason === 'manual' ? { action: 'replace_compaction', replacement: { summary: 'real replacement', first_kept: 'kept' } } : { action: 'cancel' });
  if (reason === 'manual') assert.match((await peer.request('hook/run', operation(peer, 'session_before_compact', payload, 'extra')).response).error.message, /tokensBefore/);
  await peer.close();
});
test('real tree callback carries its live leaf and append waits for a known native receipt', async t => {
  const peer = launch(t, [operations], { hold: ['session/append_entry'] }); await peer.init(['session_entries']);
  assert.equal((await peer.request('hook/run', operation(peer, 'session_before_tree', { kind: 'before_tree', target_id: 'kept', old_head: 'old' })).response).result.session_operation.action, 'cancel');
  const request = peer.request('hook/run', operation(peer, 'session_tree', { kind: 'tree', new_head: 'kept', old_head: 'old' }));
  const call = await peer.wait(frame => frame.method === 'session/append_entry');
  assert.deepEqual(call.params.data, { old: 'old', current: 'kept' }); assert.deepEqual(call.params.resource_owner, owner);
  peer.send({ jsonrpc: '2.0', id: call.id, result: { entry_id: 'durable-tree-state', head: 'durable-tree-state', successor: null } });
  assert.equal((await request.response).result.session_operation.action, 'continue'); await peer.close();
});
test('native mirrors preserve actual IDs/parents/time and only the initialized namespace private entry', () => {
  const own = { id: 'private', parent: 'kept', timestamp_unix_ms: 1235, value: { type: 'config', model: null, reasoning: null }, metadata: { extension_metadata: {
    actual: { provenance: { extension: 'actual' }, value: { entry_type: 'checkpoint', data: { version: 1 } } },
    other: { provenance: { extension: 'other' }, value: { entry_type: 'secret', data: 'DO-NOT-PUBLISH' } },
  } } };
  const mirror = translateSessionEntries([entry, own], 'actual');
  assert.equal(mirror[0].timestamp, new Date(1234).toISOString()); assert.equal(mirror[0].message.timestamp, 1234);
  assert.deepEqual(mirror[1], { id: 'private', parentId: 'kept', timestamp: new Date(1235).toISOString(), type: 'custom', customType: 'checkpoint', data: { version: 1 } });
  assert.ok(!JSON.stringify(mirror).includes('DO-NOT-PUBLISH'));
  assert.equal(translateSessionEntries([own], 'unrelated')[0].type, 'octet_native');
  const legacy = structuredClone(entry); delete legacy.timestamp_unix_ms;
  assert.equal(translateSessionEntries([legacy], 'actual')[0].timestamp, undefined);
});
