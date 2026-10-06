import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { canonicalToPi, piToCanonical, projectContext } from '../lib/provider-context.mjs';
import { Runtime } from '../lib/runtime.mjs';
import { launch, owner, root } from './helper.mjs';
const fixture = join(root, 'test/fixtures/provider-context.ts');
const grant = { grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'provider-context:4', owner, expected_head: 'old-head' };
const user = text => ({ User: { content: [{ Text: text }] } });
const contextParams = (peer, mode = 'replace') => ({ hook: 'provider_context', session_leaf: grant,
  context: peer.context({ session_name: mode, session_leaf_id: 'old-head', session_entries: [], session_branch: [] }),
  payload: { request: { system: 'real system', messages: [user('original')], tools: [{ name: 'not-replaceable' }] },
    preparation: { resource_owner: owner.session_id, session_id: 'actual-session', head: 'old-head', tool_generation: 7 } } });

for (const mode of ['replace', 'in-place', 'throws']) test(`real process context ${mode}: ordered callbacks project only canonical messages and preserve real system`, async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const reply = await peer.request('hook/run', contextParams(peer, mode)).response;
  assert.deepEqual(reply.result.provider_context, { messages: [user(mode === 'replace' ? 'projected' : mode === 'in-place' ? 'mutated' : 'original'), user('second:real system')], system: 'real system' });
  assert.equal(reply.result.provider_context.tools, undefined);
  if (mode === 'throws') {
    const issue = peer.seen.find(frame => frame.method === 'notification' && frame.params.title === '[Extension issues]');
    assert.ok(issue, peer.stderr());
    assert.equal(issue.params.level, 'warning');
    assert.ok(issue.params.message.includes(fixture));
    assert.match(issue.params.message, /context callback failed and was skipped/);
    assert.match(peer.stderr(), /\[Extension issues\]/);
    assert.doesNotMatch(issue.params.message + peer.stderr(), /ordinary context failure/);
  }
  await peer.close();
});

for (const event of ['context', 'context_with_system']) test(`${event} callback issues retain refusal and cancellation guards`, async () => {
  const runtime = new Runtime({ extensions: [fixture] }, { notify: async () => {} });
  runtime.uninstallChildren(); runtime.features.add('session_entries');
  const reports = []; runtime.reportCallbackError = (...args) => reports.push(args);
  runtime.backgroundError = () => assert.fail('recoverable callback errors must use the issue reporter');
  const store = { id: 1, method: 'hook/run', controller: new AbortController(), pending: new Set(), errors: [], live: true };
  const params = contextParams({ context: host => ({ workspace: root, resource_owner: owner, host }) });
  params.payload.request.tools = [];
  const failure = new Error('private context details'); let kept = 0;
  const handlers = [{ factory: 0, handler() { throw failure; } }, { factory: 0, handler() { kept++; } }];
  runtime.events.set(event, handlers);
  assert.ok(await projectContext(runtime, params, store));
  assert.deepEqual(reports, [[event, 0, failure]]);
  assert.equal(kept, 1);
  for (const code of [-32601, -32602, -32002, -32800]) {
    const refusal = Object.assign(new Error('native refusal'), { code });
    handlers[0].handler = () => { throw refusal; };
    await assert.rejects(projectContext(runtime, params, store), error => error === refusal);
  }
  const cancelled = Object.assign(new Error('cancelled'), { code: -32800 });
  handlers[0].handler = () => { store.controller.abort(cancelled); throw failure; };
  await assert.rejects(projectContext(runtime, params, store), error => error === cancelled);
  assert.equal(reports.length, 1); assert.equal(kept, 1);
});

test('unchanged context contributes no unnecessary canonical rewrite', async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const reply = await peer.request('hook/run', contextParams(peer, 'unchanged')).response;
  assert.equal(reply.result.provider_context, undefined); await peer.close();
});
for (const mode of ['unknown-field', 'unknown-role', 'malformed']) test(`context refuses ${mode} rather than silently dropping fields/roles`, async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const reply = await peer.request('hook/run', contextParams(peer, mode)).response;
  assert.ok(reply.error); assert.equal(reply.result, undefined); await peer.close();
});
for (const mode of ['owner', 'head', 'unbound']) test(`context refuses invalid native preparation ${mode}`, async t => {
  const peer = launch(t, [fixture]); await peer.init(); const params = contextParams(peer);
  if (mode === 'owner') params.payload.preparation.resource_owner = 'not-owner';
  if (mode === 'head') params.payload.preparation.head = 'stale-head';
  if (mode === 'unbound') delete params.session_leaf;
  assert.ok((await peer.request('hook/run', params).response).error); await peer.close();
});

test('context append publishes projection only after synchronous known leaf ACK with exact successor', async t => {
  const peer = launch(t, [fixture], { hold: ['session/append_entry'] }); await peer.init();
  const request = peer.request('hook/run', contextParams(peer, 'append'));
  const call = await peer.wait(frame => frame.method === 'session/append_entry');
  assert.equal(call.params.parent_request_id, request.id); assert.deepEqual(call.params.resource_owner, owner);
  assert.deepEqual(call.params.session_leaf, { grant_id: grant.grant_id, activation_epoch: 4, operation_id: grant.operation_id });
  assert.deepEqual(call.params.data, { before: 'old-head', text: 'durable\ncheckpoint' });
  assert.ok(!peer.seen.some(frame => frame.id === request.id && !frame.method));
  peer.send({ jsonrpc: '2.0', id: call.id, result: { entry_id: 'committed', head: 'committed', successor: { ...grant, grant_id: 'b'.repeat(64), expected_head: 'committed' } } });
  assert.deepEqual((await request.response).result.provider_context.messages, [user('original'), user('known:committed:committed')]);
  await peer.close();
});

test('context cancellation refuses pending callback and cannot emit a late projection', async t => {
  const peer = launch(t, [fixture], { hold: ['confirmation/request'] }); await peer.init(); await peer.start();
  const request = peer.request('hook/run', contextParams(peer, 'wait'));
  const call = await peer.wait(frame => frame.method === 'confirmation/request');
  peer.notify('$/cancelRequest', { id: request.id });
  assert.equal((await request.response).error.code, -32800);
  peer.send({ jsonrpc: '2.0', id: call.id, result: { confirmed: true } });
  const barrier = await peer.request('hook/run', contextParams(peer, 'unchanged')).response; assert.ok(barrier.result);
  assert.equal(peer.seen.filter(frame => frame.id === request.id && !frame.method).length, 1);
  await peer.close();
});

test('canonical tool calls/results retain actual IDs, arguments, role order and assistant model/protocol', () => {
  const messages = [user('question'), { Assistant: { model: 'historical-model', protocol: 'open_ai_responses', content: [{ Text: 'text' }, { ToolCall: { id: 'call-1', name: 'read', arguments_json: '{"path":"file"}' } }] } },
    { User: { content: [{ ToolResult: { tool_call_id: 'call-1', content: [{ Text: 'result' }], is_error: false } }] } }];
  const pi = canonicalToPi(messages); assert.equal(pi[1].timestamp, undefined); assert.equal(pi[1].usage, undefined); assert.equal(pi[1].provider, undefined);
  assert.equal(pi[2].toolName, 'read'); assert.deepEqual(piToCanonical(pi), messages);
});
test('opaque continuation/media/unsupported protocol explicitly refuse, never silently lose native parts', () => {
  for (const part of [{ ProviderMetadata: {} }, { Media: {} }, { Reasoning: { text: 'thinking', state: { kind: 'opaque' } } }]) assert.throws(() => canonicalToPi([{ Assistant: { model: 'model', protocol: 'open_ai_responses', content: [part] } }]), /unsupported_feature/);
  assert.throws(() => canonicalToPi([{ Assistant: { model: 'model', protocol: 'unknown', content: [] } }]), /unsupported_feature/);
});

test('waitForIdle waits for the negotiated real command receipt, not a locally resolved Promise', async t => {
  const peer = launch(t, [fixture], { hold: ['session/wait_for_idle'] }); await peer.init(['session_entries', 'session_control_v1']);
  const request = peer.command('idle-proof'); const call = await peer.wait(frame => frame.method === 'session/wait_for_idle');
  assert.deepEqual(call.params, { parent_request_id: request.id, resource_owner: owner });
  assert.ok(!peer.seen.some(frame => frame.method === 'notification' && frame.params.message === 'real idle receipt'));
  peer.send({ jsonrpc: '2.0', id: call.id, result: { session_id: 'actual-session' } });
  assert.ok((await request.response).result); await peer.wait(frame => frame.method === 'notification' && frame.params.message === 'real idle receipt'); await peer.close();
});

// Every Codex turn commits encrypted reasoning without a summary. Read-only
// lifecycle observations (turn_end, message_end) see it the way Pi 1.0.2's
// Responses provider stores it: summary text or "", plus the item as signature.
// Context rewriting still refuses it: the signature cannot be replayed losslessly.
test('observed OpenAI reasoning without text matches Pi; context rewriting still refuses it', () => {
  const reasoning = { Reasoning: { text: null, state: { protocol: 'open_ai_responses', model: 'codex/gpt-6-luna',
    kind: { OpenAiReasoning: { item_id: 'rs_1', encrypted_content: 'gAAAA' } } } } };
  const message = { Assistant: { content: [reasoning, { Text: 'hi' }], model: 'codex/gpt-6-luna', protocol: 'open_ai_responses' } };
  const [observed] = canonicalToPi([message], new Map(), { observation: true });
  assert.deepEqual(observed.content[0], { type: 'thinking', thinking: '',
    thinkingSignature: JSON.stringify({ type: 'reasoning', id: 'rs_1', encrypted_content: 'gAAAA', summary: [] }) });
  assert.deepEqual(observed.content[1], { type: 'text', text: 'hi' });
  const summarized = { Assistant: { ...message.Assistant, content: [{ Reasoning: { ...reasoning.Reasoning, text: 'plan' } }] } };
  const [observedSummary] = canonicalToPi([summarized], new Map(), { observation: true });
  assert.deepEqual(observedSummary.content[0], { type: 'thinking', thinking: 'plan',
    thinkingSignature: JSON.stringify({ type: 'reasoning', id: 'rs_1', encrypted_content: 'gAAAA', summary: [{ type: 'summary_text', text: 'plan' }] }) });
  for (const native of [message, summarized]) assert.throws(() => canonicalToPi([native]), {
    code: -32601, message: /^unsupported_feature context (absent reasoning text|opaque reasoning continuation):/,
  });
  // Observation output is not replay authority, even with retained provenance.
  for (const messages of [[observed], [observedSummary]]) {
    for (const projected of [messages, structuredClone(messages)]) assert.throws(() => piToCanonical(projected), {
      code: -32601, message: /^unsupported_feature Pi opaque reasoning signature:/,
    });
  }
});

test('OpenAI observations omit absent item IDs without inventing IDs or authorizing replay', () => {
  for (const text of [null, 'plan']) for (const encrypted_content of [null, 'gAAAA']) {
    const state = { protocol: 'open_ai_responses', model: 'model', kind: { OpenAiReasoning: { item_id: null, encrypted_content } } };
    const native = [{ Assistant: { model: state.model, protocol: state.protocol, content: [{ Reasoning: { text, state } }] } }];
    const before = structuredClone(native), observed = canonicalToPi(native, new Map(), { observation: true });
    assert.deepEqual(observed[0].content, [{ type: 'thinking', thinking: text ?? '',
      thinkingSignature: JSON.stringify({ type: 'reasoning', ...(encrypted_content === null ? {} : { encrypted_content }),
        summary: text === null ? [] : [{ type: 'summary_text', text }] }) }]);
    assert.deepEqual(native, before);
    assert.throws(() => canonicalToPi(native), { code: -32601, message: /^unsupported_feature context (absent reasoning text|opaque reasoning continuation):/ });
    for (const messages of [observed, structuredClone(observed)]) assert.throws(() => piToCanonical(messages), {
      code: -32601, message: /^unsupported_feature Pi opaque reasoning signature:/,
    });
  }
});

test('Anthropic redacted observations match Pi without becoming replayable signed thinking', () => {
  const state = { protocol: 'anthropic_messages', model: 'model', kind: { AnthropicRedacted: { data: 'AQIDBA==' } } };
  const native = [{ Assistant: { model: state.model, protocol: state.protocol, content: [{ Reasoning: { text: null, state } }, { Text: 'answer' }] } }];
  const before = structuredClone(native), observed = canonicalToPi(native, new Map(), { observation: true });
  assert.deepEqual(observed[0].content, [{ type: 'thinking', thinking: '[Reasoning redacted]', thinkingSignature: 'AQIDBA==', redacted: true }, { type: 'text', text: 'answer' }]);
  assert.deepEqual(native, before);
  assert.throws(() => canonicalToPi(native), { code: -32601, message: /^unsupported_feature context absent reasoning text:/ });
  assert.throws(() => canonicalToPi(native, new Map(), { observation: false }), { code: -32601, message: /^unsupported_feature context absent reasoning text:/ });
  for (const messages of [observed, structuredClone(observed)]) assert.throws(() => piToCanonical(messages), {
    code: -32601, message: /^unsupported_feature Pi thinking part.redacted:/,
  });
  for (const patch of [{ protocol: 'open_ai_responses' }, { model: 'foreign-model' }]) {
    const foreign = structuredClone(native); Object.assign(foreign[0].Assistant.content[0].Reasoning.state, patch);
    assert.throws(() => canonicalToPi(foreign, new Map(), { observation: true }), { code: -32601, message: /signature producer must match/ });
  }
  const malformed = structuredClone(native); malformed[0].Assistant.content[0].Reasoning.state.kind.AnthropicRedacted.data = null;
  assert.throws(() => canonicalToPi(malformed, new Map(), { observation: true }), { code: -32602, message: /context text must be UTF-8 text/ });
});

test('process turn observations deliver ID-less OpenAI and redacted Anthropic reasoning', async t => {
  const peer = launch(t, [join(root, 'test/fixtures/model-turns.ts')]); await peer.init(['session_entries']); await peer.start();
  const cases = [
    ['open_ai_responses', { OpenAiReasoning: { item_id: null, encrypted_content: 'gAAAA' } },
      { type: 'thinking', thinking: '', thinkingSignature: JSON.stringify({ type: 'reasoning', encrypted_content: 'gAAAA', summary: [] }) }],
    ['anthropic_messages', { AnthropicRedacted: { data: 'AQIDBA==' } },
      { type: 'thinking', thinking: '[Reasoning redacted]', thinkingSignature: 'AQIDBA==', redacted: true }],
  ];
  for (const [protocol, kind, expected] of cases) {
    const entry = { id: 'assistant', parent: null, timestamp_unix_ms: 1234, value: { type: 'message',
      Assistant: { model: 'model', protocol, content: [{ Reasoning: { text: null, state: { model: 'model', protocol, kind } } }] } } };
    const reply = await peer.request('hook/run', { hook: 'model_turn_end',
      session_leaf: { ...grant, operation_id: 'model-turn:4', expected_head: entry.id },
      context: peer.context({ session_leaf_id: entry.id, session_entries: [entry], session_branch: [entry] }),
      payload: { kind: 'model_turn_end', run_id: 'run:user', turn_index: 0, timestamp_ms: 1300, assistant_entry: entry, tool_result_entries: [] },
    }).response;
    assert.ok(reply.result, JSON.stringify(reply));
    const frame = await peer.wait(frame => frame.method === 'notification' && /^(model-end:|Pi callback skipped: turn_end;)/.test(frame.params.message));
    assert.match(frame.params.message, /^model-end:/);
    const event = JSON.parse(frame.params.message.slice('model-end:'.length));
    assert.deepEqual(event.message.content, [expected]);
    assert.equal(event.message.timestamp, 1234);
  }
  await peer.close();
});

test('context replay refuses opaque state before parsing its kind; observations still validate it', () => {
  for (const text of ['thinking', null]) {
    const messages = [{ Assistant: { model: 'model', protocol: 'open_ai_responses',
      content: [{ Reasoning: { text, state: { kind: 'opaque' } } }] } }];
    const refusal = { code: -32601, message: /^unsupported_feature context (absent reasoning text|opaque reasoning continuation):/ };
    assert.throws(() => canonicalToPi(messages), refusal);
    assert.throws(() => canonicalToPi(messages, new Map(), { observation: false }), refusal);
    assert.throws(() => canonicalToPi(messages, new Map(), { observation: true }), {
      code: -32602, message: 'invalid_request reasoning state kind',
    });
  }
});

test('process context validates opaque reasoning before callbacks and permits ordinary context filtering', async t => {
  const peer = launch(t, [fixture]); await peer.init();
  const state = { protocol: 'open_ai_responses', model: 'model',
    kind: { OpenAiReasoning: { item_id: 'rs_1', encrypted_content: 'gAAAA' } } };
  for (const reasoning of [{ text: 'thinking', state: { kind: 'opaque' } }, { text: null, state }, { text: 'plan', state }]) {
    const params = contextParams(peer, 'replace');
    params.payload.request.messages = [{ Assistant: { model: 'model', protocol: 'open_ai_responses', content: [{ Reasoning: reasoning }] } }];
    const reply = await peer.request('hook/run', params).response;
    if (reasoning.state === state) {
      assert.deepEqual(reply.result.provider_context.messages, [user('projected'), user('second:real system')]);
    } else {
      assert.equal(reply.error?.code, -32602);
      assert.match(reply.error.message, /invalid_request reasoning state kind/);
      assert.equal(reply.result, undefined);
    }
  }
  await peer.close();
});
