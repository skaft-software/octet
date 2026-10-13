// Opt-in unchanged factories. All host inputs/ACKs below are SYNTHETIC, not
// native persistence/provider evidence. Run via scripts/test-pi-original-acceptance.py.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { launch, owner, host } from './helper.mjs';
const entry = process.env.PI_CLM_PATH;
const enabled = { skip: !entry && 'set reviewed PI_CLM_PATH', timeout: 20000 };
const namespace = 'original-clm-probe';
const features = ['autocomplete', 'session_entries', 'session_control_v1', 'pipeline_hooks_v1', 'resource_paths_v1', 'before_prompt_state_v1', 'tool_prompt_metadata_v1'];
const user = { id: 'user', parent: null, timestamp_unix_ms: 1000, value: { type: 'message', User: { content: [{ Text: 'Keep this original task.' }] } } };
const assistant = { id: 'assistant', parent: 'user', timestamp_unix_ms: 1100, value: { type: 'message', Assistant: { model: 'native-model', protocol: 'open_ai_chat', content: [{ Text: 'Original detailed answer.' }] } } };
const snapshot = (entries = [user]) => ({ session_entries: entries, session_branch: entries, session_leaf_id: entries.at(-1)?.id ?? null, system_prompt: 'Synthetic base system', context_usage: { tokens: null, percent: null, contextWindow: 32768 } });
function custom(id, parent, type, data) { return { id, parent, value: { type: 'config' }, metadata: { extension_metadata: { [namespace]: { provenance: { extension: namespace }, value: { entry_type: type, data } } } } }; }
function grant(head) { return { grant_id: 'a'.repeat(64), activation_epoch: 1, operation_id: 'original-clm-probe', owner, expected_head: head }; }
function ok(reply) { assert.ok(reply.result, JSON.stringify(reply)); return reply.result; }
async function open(t) {
  for (const [path, hash] of [[entry, 'd5e9d73034eb87e28dc9909a2d8ac28b85a1969fd234c7a8f3eddecb71e3fde6'], [join(dirname(entry), 'src/index.ts'), 'a6d1c464bebdcc927a0eb6d0bb1ff319c3e5cdf5bf844e03b599ec91169f7170']]) assert.equal(createHash('sha256').update(readFileSync(path)).digest('hex'), hash);
  const peer = launch(t, [entry], { hold: ['session/append_entry'] });
  ok(await peer.request('initialize', { api_version: '0.4', extension: { name: namespace }, host,
    contributes: { tools: peer.metadata.tools.map(t => t.name), commands: peer.metadata.commands.map(c => c.name), hooks: peer.metadata.hooks, tool_renderers: peer.metadata.tool_renderers },
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: features } }).response);
  return peer;
}
async function command(peer, args, facts) {
  const request = peer.command('clm', args, facts);
  const idle = await peer.wait(f => f.method === 'session/wait_for_idle');
  peer.send({ jsonrpc: '2.0', id: idle.id, result: { session_id: host.session_id } });
  return request;
}
async function stop(peer, facts) {
  ok(await peer.request('hook/run', { hook: 'session_end', payload: { binding: owner }, context: peer.context(facts) }).response);
  await peer.close();
}

test('CLM initial native-shaped snapshot restores branch-local settings (not registration)', enabled, async t => {
  const peer = await open(t);
  const saved = custom('saved', 'user', 'pi-clm-settings', { version: 1, overrides: { budget: 12000 } });
  const otherBranch = custom('other-branch', 'user', 'pi-clm-settings', { version: 1, overrides: { budget: 24000 } });
  const facts = { ...snapshot([user, saved]), session_entries: [user, saved, otherBranch] };
  await peer.start(facts);
  const request = await command(peer, ['config', 'budget'], facts); ok(await request.response);
  const notice = await peer.wait(f => f.method === 'notification' && f.params.message.startsWith('Budget:'));
  assert.match(notice.params.message, /^Budget: 12k —/);
  assert.doesNotMatch(notice.params.message, /24k/);
  await stop(peer, facts);
});

test('CLM original annotate uses getBranch, synchronous append ACK, then getEntries recall', enabled, async t => {
  const peer = await open(t); const facts = snapshot(); await peer.start(facts);
  const request = peer.request('tool/call', { name: 'live_context_annotate', arguments: { action: 'create', source: 'user', title: 'Preserve task', reason: 'Original acceptance', futureAction: 'Recall exact text', retention: 'archive' }, context: peer.context(facts) });
  const append = await peer.wait(f => f.method === 'session/append_entry');
  assert.equal(append.params.entry_type, 'live-context-annotation');
  assert.equal(append.params.data.source.entryId, 'user');
  assert.deepEqual(append.params.resource_owner, owner);
  assert.equal(peer.seen.some(f => f.id === request.id && !f.method), false, 'no tool result before append ACK');
  peer.send({ jsonrpc: '2.0', id: append.id, result: { entry_id: 'saved-annotation' } });
  const result = ok(await request.response);
  assert.equal(result.metadata.pi_details.annotation.source.entryId, 'user');
  const saved = custom('saved-annotation', 'user', append.params.entry_type, append.params.data);
  const recall = ok(await peer.request('tool/call', { name: 'live_context_recall', arguments: { id: append.params.data.id, maxTokens: 256 }, context: peer.context(snapshot([user, saved])) }).response);
  assert.match(JSON.stringify(recall), /Keep this original task\./);
  assert.equal(recall.metadata.pi_details.truncated, false);
  await stop(peer, snapshot([user, saved]));
});

test('CLM original context -> mirror edit -> turn_end append -> next effective context', enabled, async t => {
  const peer = await open(t); const facts = snapshot([user, assistant]); await peer.start(facts);
  const start = { hook: 'model_turn_start', payload: { kind: 'model_turn_start', run_id: 'run:user', turn_index: 0, timestamp_ms: 1050 }, context: peer.context(facts), session_leaf: grant('assistant') };
  ok(await peer.request('hook/run', start).response);
  const context = { hook: 'provider_context', session_leaf: grant('assistant'), context: peer.context(facts), payload: { request: { system: facts.system_prompt, messages: [user.value, assistant.value].map(({ type, ...value }) => value), tools: [] }, preparation: { resource_owner: owner.session_id, session_id: host.session_id, head: 'assistant', tool_generation: 1 } } };
  ok(await peer.request('hook/run', context).response);
  const pathRequest = await command(peer, ['path'], facts); ok(await pathRequest.response);
  const pathNotice = await peer.wait(f => f.method === 'notification' && f.params.message.endsWith('/LIVE_CONTEXT.md'));
  const mirror = pathNotice.params.message;
  assert.ok(mirror.startsWith(process.env.TMPDIR + '/'), 'mirror stays within this isolated receipt');
  const original = readFileSync(mirror, 'utf8'); assert.match(original, /Original detailed answer\./);
  writeFileSync(mirror, original.replace('Original detailed answer.', 'Short revised answer.'));
  const end = peer.request('hook/run', { hook: 'model_turn_end', payload: { kind: 'model_turn_end', run_id: 'run:user', turn_index: 0, timestamp_ms: 1200, assistant_entry: assistant, tool_result_entries: [] }, context: peer.context(facts), session_leaf: grant('assistant') });
  const append = await Promise.race([peer.wait(f => f.method === 'session/append_entry'), end.response.then(reply => { assert.fail(`turn_end settled before checkpoint: ${JSON.stringify(reply)}`); })]);
  assert.equal(append.params.entry_type, 'live-context-state');
  assert.equal(append.params.data.revision, 1);
  assert.match(JSON.stringify(append.params.data.checkpoint), /Short revised answer/);
  assert.equal(peer.seen.some(f => f.id === end.id && !f.method), false);
  peer.send({ jsonrpc: '2.0', id: append.id, result: { entry_id: 'checkpoint', head: 'checkpoint', successor: null } });
  ok(await end.response);
  const checkpoint = custom('checkpoint', 'assistant', append.params.entry_type, append.params.data);
  const nextFacts = snapshot([user, assistant, checkpoint]);
  context.context = peer.context(nextFacts); context.session_leaf = grant('checkpoint'); context.payload.preparation.head = 'checkpoint';
  const projected = ok(await peer.request('hook/run', context).response).provider_context;
  assert.match(JSON.stringify(projected.messages), /Short revised answer/);
  assert.doesNotMatch(JSON.stringify(projected.messages), /Original detailed answer/);
  await stop(peer, nextFacts); assert.equal(existsSync(mirror), false);
});

test('CLM original threshold compaction veto is a decision, manual compaction continues', enabled, async t => {
  const peer = await open(t); const facts = snapshot(); await peer.start(facts);
  for (const reason of ['threshold', 'manual']) {
    const reply = ok(await peer.request('hook/run', { hook: 'session_before_compact', context: peer.context(facts), session_leaf: grant('user'), payload: { kind: 'before_compact', reason, first_kept: 'user', preparation: { messages: [], turn_prefix_messages: [], previous_summary: null, details: { read_files: [], modified_files: [] } }, branch_entries: [user], custom_instructions: null } }).response);
    assert.equal(reply.session_operation.action, reason === 'threshold' ? 'cancel' : 'continue');
  }
  await stop(peer, facts);
});
