import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { Runtime } from '../lib/runtime.mjs';
import { canonicalToPi, piToCanonical, projectContext } from '../lib/provider-context.mjs';
import { launch, owner, root } from './helper.mjs';

const fixture = join(root, 'test/fixtures/provider-context.ts');
const grant = { grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'provider-context:4', owner, expected_head: 'old-head' };
const user = text => ({ User: { content: [{ Text: text }] } });
const transcript = () => [user('question'), { Assistant: { model: 'model', protocol: 'open_ai_responses', content: [
  { Reasoning: { text: null, state: { model: 'model', protocol: 'open_ai_responses', kind: { OpenAiReasoning: { item_id: 'rs_1', encrypted_content: 'opaque' } } } } },
  { ToolCall: { id: 'call-1', name: 'Bash', arguments_json: '{"command":"printf ok"}' } },
] } }, { User: { content: [{ ToolResult: { tool_call_id: 'call-1', content: [{ Text: 'ok' }], is_error: false } }] } }];
const params = (context, messages, mode = 'unchanged') => ({ hook: 'provider_context', session_leaf: grant,
  context: context({ session_name: mode, session_leaf_id: 'old-head', session_entries: [], session_branch: [] }),
  payload: { request: { system: 'real system', messages, tools: [] },
    preparation: { resource_owner: owner.session_id, session_id: 'actual-session', head: 'old-head', tool_generation: 7 } } });

test('tool follow-up runs unchanged context callbacks without rewriting native encrypted reasoning', async () => {
  const runtime = new Runtime({ extensions: [fixture] }, { notify: async () => {} });
  runtime.uninstallChildren(); runtime.features.add('session_entries');
  let calls = 0;
  runtime.events.set('context', [{ factory: 0, handler(event) {
    calls++;
    if (event.messages.length > 1) assert.equal(event.messages[1].content[0].thinking, '');
    return { messages: event.messages };
  } }]);
  const context = host => ({ workspace: root, resource_owner: owner, host });
  const store = () => ({ id: calls + 1, method: 'hook/run', controller: new AbortController(), pending: new Set(), errors: [], live: true });
  assert.equal((await projectContext(runtime, params(context, [user('question')]), store())).provider_context, undefined);
  const messages = transcript(), before = structuredClone(messages);
  assert.equal((await projectContext(runtime, params(context, messages), store())).provider_context, undefined);
  assert.equal(calls, 2);
  assert.deepEqual(messages, before);
});

test('real process context edits preserve untouched opaque reasoning and tool replay', async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const messages = transcript(), before = structuredClone(messages);
  const reply = await peer.request('hook/run', params(host => peer.context(host), messages, 'throws')).response;
  assert.ok(reply.result, JSON.stringify(reply));
  assert.deepEqual(reply.result.provider_context, { messages: [...before, user('second:real system')], system: 'real system' });
  assert.deepEqual(messages, before);
  await peer.close();
});

test('redacted native thinking cannot be reclassified as signed thinking by removing its marker', () => {
  const state = { protocol: 'anthropic_messages', model: 'model', kind: { AnthropicRedacted: { data: 'opaque' } } };
  const native = [{ Assistant: { model: 'model', protocol: state.protocol, content: [{ Reasoning: { text: null, state } }] } }];
  const projected = canonicalToPi(native, new Map(), { replay: true });
  assert.deepEqual(piToCanonical(projected), native);
  delete projected[0].content[0].redacted;
  assert.throws(() => piToCanonical(projected));
});

test('opaque replay requires unchanged native provenance, never a copied or modified signature', () => {
  const native = transcript();
  const projected = canonicalToPi(native, new Map(), { replay: true });
  assert.deepEqual(piToCanonical(projected), native);
  for (const mutate of [
    messages => { messages[1].content[0].thinkingSignature = 'forged'; },
    messages => { messages[1].model = 'foreign-model'; },
    messages => { messages[1].content[0].thinking = 'invented'; },
    messages => { messages[1] = structuredClone(messages[1]); },
    messages => { messages[1].content[0][Symbol('hidden')] = true; },
    messages => { Object.defineProperty(messages[1].content[0], 'hidden', { value: true }); },
    messages => { messages[1].content[0].hidden = undefined; },
    messages => { Object.defineProperty(messages[1].content[0], 'thinkingSignature', { get() { return JSON.stringify({ type: 'reasoning', id: 'rs_1', encrypted_content: 'opaque', summary: [] }); }, enumerable: true }); },
  ]) {
    const messages = canonicalToPi(native, new Map(), { replay: true }); mutate(messages);
    assert.throws(() => piToCanonical(messages));
  }
  const mismatched = transcript(); mismatched[1].Assistant.content[0].Reasoning.state.model = 'foreign-model';
  assert.throws(() => canonicalToPi(mismatched, new Map(), { replay: true }), /signature producer must match/);
});
