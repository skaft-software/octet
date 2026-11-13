import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

const kept = { id: 'kept', parent: null, value: { type: 'message', User: { content: [{ Text: 'keep' }] } } };
const grant = { grant_id: 'a'.repeat(64), activation_epoch: 2, operation_id: 'session-operation:2', owner, expected_head: 'kept' };
const before = () => ({ kind: 'before_compact', reason: 'manual', first_kept: 'kept',
  preparation: { messages: [], turn_prefix_messages: [], previous_summary: null }, branch_entries: [kept], custom_instructions: null });
async function fixture(t, source, options) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-session-operation-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const file = join(dir, 'extension.ts'); await writeFile(file, source);
  const peer = launch(t, [file], options); await peer.init(['session_entries']);
  return peer;
}
const request = (peer, hook, payload) => peer.request('hook/run', { hook, payload, session_leaf: grant,
  context: peer.context({ session_entries: [kept], session_branch: [kept], session_leaf_id: 'kept' }) });

for (const model of ['model', 'foreign']) test(`compaction preparation observes opaque reasoning from ${model} without replay authority`, async t => {
  const peer = await fixture(t, `export default pi => pi.on('session_before_compact', event => {
    const message = event.preparation.messagesToSummarize[0];
    if (message.content[0].thinking !== 'visible') throw new Error('lost reasoning');
    return { compaction: { summary: 'replacement', firstKeptEntryId: event.preparation.firstKeptEntryId } };
  });`);
  const payload = before();
  payload.preparation.messages = [{ Assistant: { model: 'model', protocol: 'open_ai_responses', content: [
    { Reasoning: { text: 'visible', state: { protocol: 'open_ai_responses', model, kind: { OpenAiReasoning: { item_id: 'rs_1', encrypted_content: 'opaque' } } } } },
  ] } }];
  const reply = await request(peer, 'session_before_compact', payload).response;
  assert.ok(reply.result, JSON.stringify(reply));
  assert.deepEqual(reply.result.session_operation, { action: 'replace_compaction', replacement: { summary: 'replacement', first_kept: 'kept' } });
  assert.equal(peer.seen.some(frame => frame.method === 'notification'), false);
  await peer.close();
});

test('split-turn compaction shares tool identities across the summary and retained prefix', async t => {
  const peer = await fixture(t, `export default pi => pi.on('session_before_compact', event => {
    if (event.preparation.turnPrefixMessages[0].toolName !== 'read') throw new Error('lost tool identity');
  });`);
  const payload = before();
  payload.preparation.messages = [{ Assistant: { model: 'model', protocol: 'open_ai_chat', content: [
    { ToolCall: { id: 'call', name: 'read', arguments_json: '{}' } },
  ] } }];
  payload.preparation.turn_prefix_messages = [{ User: { content: [{ ToolResult: { tool_call_id: 'call', content: [{ Text: 'result' }], is_error: false } }] } }];
  const reply = await request(peer, 'session_before_compact', payload).response;
  assert.ok(reply.result, JSON.stringify(reply));
  assert.equal(reply.result.session_operation.action, 'continue');
  assert.equal(peer.seen.some(frame => frame.method === 'notification'), false);
  await peer.close();
});

for (const hook of ['session_before_compact', 'session_compact', 'session_before_tree', 'session_tree']) test(`${hook} isolates a throwing callback and still applies a later explicit veto`, async t => {
  const peer = await fixture(t, `export default pi => {
    pi.on('${hook}', () => { throw new Error('PRIVATE-CALLBACK-TEXT'); });
    pi.on('${hook}', () => ${hook.startsWith('session_before_') ? '({ cancel: true })' : 'undefined'});
  };`);
  const payload = hook === 'session_before_compact' ? before()
    : hook === 'session_compact' ? { kind: 'compacted', reason: 'manual', entry: { id: 'compact', parent: 'kept', value: { type: 'compaction', summary: 'summary', first_kept: 'kept', details: { read_files: [], modified_files: [] } } }, from_extension: false }
    : hook === 'session_before_tree' ? { kind: 'before_tree', target_id: 'kept', old_head: 'old' }
    : { kind: 'tree', new_head: 'kept', old_head: 'old' };
  const reply = await request(peer, hook, payload).response;
  assert.ok(reply.result, JSON.stringify(reply));
  assert.equal(reply.result.session_operation.action, hook.startsWith('session_before_') ? 'cancel' : 'continue');
  const warning = peer.seen.find(frame => frame.method === 'notification');
  assert.match(warning?.params.message, new RegExp(hook + ' hook failed and was skipped'));
  assert.ok(!JSON.stringify(peer.seen).includes('PRIVATE-CALLBACK-TEXT'));
  await peer.close();
});

test('an unavailable Pi preparation field warns instead of preventing native compaction', async t => {
  const peer = await fixture(t, `export default pi => pi.on('session_before_compact', event => { void event.preparation.tokensBefore; });`);
  const reply = await request(peer, 'session_before_compact', before()).response;
  assert.ok(reply.result, JSON.stringify(reply));
  assert.equal(reply.result.session_operation.action, 'continue');
  assert.match(peer.seen.find(frame => frame.method === 'notification')?.params.message, /session_before_compact/);
  await peer.close();
});

test('caught host append failures remain fatal at the session-operation boundary', async t => {
  const peer = await fixture(t, `export default pi => pi.on('session_before_compact', () => {
    try { pi.appendEntry('proof', {}); } catch {}
  });`, { hold: ['session/append_entry'] });
  const call = request(peer, 'session_before_compact', before());
  const append = await peer.wait(frame => frame.method === 'session/append_entry');
  peer.send({ jsonrpc: '2.0', id: append.id, error: { code: -32002, message: 'append refused' } });
  assert.equal((await call.response).error.code, -32002);
  await peer.close();
});
