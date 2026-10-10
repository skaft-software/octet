import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, realpathSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';

const secret = 'PRIVATE_ISSUE_DISPATCH_DATA';
const notice = frame => frame.method === 'notification' && frame.params.title === '[Extension issues]';
const events = ['input', 'before_agent_start', 'before_provider_request', 'before_provider_headers', 'after_provider_response', 'resources_discover'];
const features = ['before_prompt_state_v1', 'input_transform_v1', 'pipeline_hooks_v1', 'resource_paths_v1'];
const model = { id: 'test-model', provider: 'test-provider', api: 'openai-completions' };
function fixtures(t, sources) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-issue-dispatch-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return sources.map((source, n) => {
    const path = join(dir, `extension-${n}.ts`); writeFileSync(path, source); return path;
  });
}
function request(peer, event) {
  const context = peer.context();
  if (event === 'input') return { hook: 'before_prompt', context, payload: { phase: 'input', text: secret, source: 'interactive' } };
  if (event === 'before_agent_start') return { hook: 'before_prompt', context, payload: { prompt: secret, system_prompt: 'real system' } };
  if (event === 'resources_discover') return { hook: event, context, payload: { cwd: root, reason: 'reload' } };
  return { hook: event, context, payload: { operation_id: 'attempt', model,
    ...(event === 'before_provider_request' ? { payload: { text: secret } } : { headers: { 'x-private': secret } }),
    ...(event === 'after_provider_response' ? { status: 200 } : {}) } };
}

for (const event of events) test(`${event}: recoverable failures are private, actionable, and once per extension across requests`, async t => {
  const entries = fixtures(t, [
    `export default pi => pi.on('${event}', () => { throw new Error('${secret}'); });`,
    `export default pi => {
      pi.on('${event}', () => { throw new Error('${secret}'); });
      pi.on('${event}', (_event, ctx) => ctx.ui.notify('later callback ran'));
    };`,
  ]);
  const peer = launch(t, entries); await peer.init(features); await peer.start();
  for (let n = 0; n < 3; n++) {
    const reply = await peer.request('hook/run', request(peer, event)).response;
    assert.ok(reply.result, JSON.stringify(reply));
  }
  const notices = peer.seen.filter(notice);
  assert.equal(notices.length, 2);
  for (const entry of entries) {
    const matching = notices.filter(frame => frame.params.message.includes(entry));
    assert.equal(matching.length, 1);
    assert.match(matching[0].params.message, new RegExp(event + ' callback failed'));
    assert.match(matching[0].params.message, /Next:/);
  }
  assert.equal(peer.seen.filter(frame => frame.method === 'notification' && frame.params.message === 'later callback ran').length, 3);
  assert.doesNotMatch(JSON.stringify(notices) + peer.stderr(), new RegExp(secret));
  await peer.close();
});

test('specialized issue reporting never converts coded refusals into successful hooks', async t => {
  const entries = fixtures(t, [`export default pi => {
    for (const event of ${JSON.stringify(events)}) pi.on(event, () => { throw Object.assign(new Error('host refused'), {code: -32002}); });
  };`]);
  const peer = launch(t, entries); await peer.init(features); await peer.start();
  for (const event of events) {
    const reply = await peer.request('hook/run', request(peer, event)).response;
    assert.equal(reply.error?.code, -32002, event);
  }
  assert.equal(peer.seen.filter(notice).length, 0);
  await peer.close();
});

function modelEnd(peer, protocol) {
  const assistant = { id: 'assistant', parent: null, timestamp_unix_ms: 1000,
    value: { type: 'message', Assistant: { model: 'native-model', protocol, content: [{ Text: secret }] } } };
  return { hook: 'model_turn_end', context: peer.context({ session_entries: [assistant], session_branch: [assistant], session_leaf_id: 'assistant' }),
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 1, operation_id: 'turn:1', owner, expected_head: 'assistant' },
    payload: { kind: 'model_turn_end', run_id: 'run', turn_index: 0, timestamp_ms: 1100, assistant_entry: assistant,
      assistant_metadata: { assistant_entry_id: assistant.id, model: 'native-model', stop_reason: 'end_turn', cost: null,
        usage: { input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, cache_write_1h_tokens: 0, reasoning_tokens: 0, total_tokens: 2 } }, tool_result_entries: [] } };
}

test('model-turn projection problems group affected extension paths once without exposing private native fields', async t => {
  const entries = fixtures(t, [0, 1].map(() => `export default pi => pi.on('turn_end', (_event, ctx) => ctx.ui.notify('converted turn delivered'));`));
  const peer = launch(t, entries); await peer.init(['session_entries']); await peer.start();
  for (let n = 0; n < 3; n++) {
    const reply = await peer.request('hook/run', modelEnd(peer, secret)).response;
    assert.ok(reply.result, JSON.stringify(reply));
  }
  const notices = peer.seen.filter(notice);
  assert.equal(notices.length, 1, 'one group, not one notice per affected callback or turn');
  for (const path of entries) assert.ok(notices[0].params.message.includes(path));
  assert.match(notices[0].params.message, /turn_end.*octet could not convert/i);
  assert.match(notices[0].params.message, /Next:/);
  assert.doesNotMatch(JSON.stringify(notices) + peer.stderr(), new RegExp(secret));
  assert.ok((await peer.request('hook/run', modelEnd(peer, 'open_ai_chat')).response).result);
  assert.equal(peer.seen.filter(frame => frame.method === 'notification' && frame.params.message === 'converted turn delivered').length, 2,
    'an earlier projection issue must not disable future callbacks');
  await peer.close();
});
