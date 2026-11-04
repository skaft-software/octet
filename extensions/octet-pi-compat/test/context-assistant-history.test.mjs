import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { assistantObservation } from '../lib/run-messages.mjs';
import { launch, owner, root } from './helper.mjs';
// A Pi package's context hook may probe session history for fields a Pi
// assistant message does not have. Those read as undefined in Pi and must not
// cause provider context preparation to refuse the turn.
const fixture = join(root, 'test/fixtures/context-assistant-history.ts');
const user = text => ({ User: { content: [{ Text: text }] } });
const assistant = text => ({ Assistant: { model: 'probe-model', protocol: 'open_ai_chat', content: [{ Text: text }] } });
const entries = (...extra) => [
  { id: '001', parent: null, timestamp_unix_ms: 1, value: { type: 'message', User: { content: [{ Text: 'question' }] } } },
  { id: '002', parent: '001', timestamp_unix_ms: 2, value: { type: 'message', ...assistant('answer') } },
  ...extra,
];
const contract = { id: '003', parent: '002', timestamp_unix_ms: 3, value: { type: 'message', User: { content: [{ Text: 'contract' }] } },
  metadata: { custom_message: { custom_type: 'goal-contract', content: 'contract', display: false } } };
const params = (peer, history) => {
  const head = history.at(-1).id;
  return { hook: 'provider_context',
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'provider-context:4', owner, expected_head: head },
    context: peer.context({ session_name: 'history', session_leaf_id: head, session_entries: history, session_branch: history }),
    payload: { request: { system: 'real system', messages: [user('question'), assistant('answer'), user('next')], tools: [] },
      preparation: { resource_owner: owner.session_id, session_id: 'actual-session', head, tool_generation: 7 } } };
};

test('a context hook reading Pi-absent keys of an assistant history entry still runs, so the turn is not refused', async t => {
  const peer = launch(t, [fixture]); await peer.init(); await peer.start();
  const reply = await peer.request('hook/run', params(peer, entries(contract))).response;
  assert.equal(reply.error, undefined, JSON.stringify(reply.error));
  assert.deepEqual(reply.result.provider_context.messages.at(-1), user('found'));
  assert.equal(reply.result.provider_context.messages.length, 4);
  await peer.close();
});

test('the same hook leaves a history without the marker unchanged', async t => {
  const peer = launch(t, [fixture]); await peer.init(); await peer.start();
  const reply = await peer.request('hook/run', params(peer, entries())).response;
  assert.equal(reply.error, undefined, JSON.stringify(reply.error));
  assert.equal(reply.result.provider_context, undefined);
  await peer.close();
});

test('assistant observations still refuse Pi facts the host cannot supply, and read other keys as absent', () => {
  const message = assistantObservation({ role: 'assistant', content: [], model: 'probe-model', api: 'openai-completions' });
  for (const key of ['usage', 'stopReason', 'provider', 'timestamp', 'responseId', 'errorMessage', 'providerThinkingLevel', 'thinkingLevel']) {
    assert.throws(() => message[key], new RegExp(`unsupported_feature assistant message.${key}`), key);
  }
  for (const key of ['type', 'customType', 'message', 'details', 'display', 'id', 'parentId', 'toolName', 'anything']) {
    assert.equal(message[key], undefined, key);
    assert.equal(key in message, false, key);
  }
  assert.equal(message.model, 'probe-model');
  assert.deepEqual(JSON.parse(JSON.stringify(message)), { role: 'assistant', content: [], model: 'probe-model', api: 'openai-completions' });
});
