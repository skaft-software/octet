import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createJiti } from 'jiti';
import { canonicalToPi, piToCanonical } from '../lib/provider-context.mjs';
import { translateSessionEntries } from '../lib/session-mirror.mjs';
import { launch, owner, root } from './helper.mjs';

// Real inline PNG bytes, not a made-up media descriptor.
const data = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aHfoAAAAASUVORK5CYII=';
const image = () => ({ Media: { Image: { source: { Inline: data }, media_type: 'image/png', detail: null } } });
const piImage = () => ({ type: 'image', data, mimeType: 'image/png' });
const user = content => ({ User: { content } });
const assistant = { Assistant: { model: 'historical-model', protocol: 'open_ai_chat', content: [
  { ToolCall: { id: 'call', name: 'read', arguments_json: '{ "path": "image.png" }', async: false, argument_error: null } },
] } };
const result = () => ({ ToolResult: { tool_call_id: 'call', content: [{ Text: 'image follows' }, image()], is_error: false, added_tool_names: null } });
const entry = (id, parent, message, metadata) => ({ id, parent, timestamp_unix_ms: 1234,
  value: { type: 'message', ...message }, ...(metadata ? { metadata } : {}) });

test('inline user/custom/tool images retain bytes, MIME and order; copied Pi images roundtrip', () => {
  const native = [user([{ Text: 'before' }, image(), { Text: 'after' }])];
  const pi = canonicalToPi(native);
  assert.deepEqual(pi, [{ role: 'user', content: [{ type: 'text', text: 'before' }, piImage(), { type: 'text', text: 'after' }] }]);
  assert.deepEqual(piToCanonical(structuredClone(pi)), native);
  assert.deepEqual(piToCanonical([{ role: 'custom', customType: 'image', display: false, content: [piImage()] }]), [user([image()])]);
  const messages = canonicalToPi([assistant, user([result()])]);
  assert.deepEqual(messages[1].content, [{ type: 'text', text: 'image follows' }, piImage()]);
  assert.deepEqual(piToCanonical(messages), [assistant, user([result()])]);
});

test('retained mixed user/tool projection roundtrips native boundaries and metadata without fake Pi fields', () => {
  const native = [assistant, user([{ Text: 'before' }, image(), result(), { Text: 'after' }]), user([{ Text: 'separate' }])];
  const before = structuredClone(native), pi = canonicalToPi(native);
  assert.deepEqual(pi.map(message => message.role), ['assistant', 'user', 'toolResult', 'user', 'user']);
  assert.deepEqual(Reflect.ownKeys(pi[1]), ['role', 'content']);
  assert.deepEqual(piToCanonical(pi), native);
  // Context callbacks commonly shallow-copy the message list, then append.
  assert.deepEqual(piToCanonical([...pi, { role: 'user', content: 'new' }]), [...native, user([{ Text: 'new' }])]);
  assert.deepEqual(native, before);
  // Edits must not be hidden by restoration of the source batch.
  pi[1].content[0].text = 'edited';
  const rewritten = piToCanonical(pi);
  assert.equal(rewritten[1].User.content[0].Text, 'edited');
  assert.deepEqual(canonicalToPi(rewritten), pi);
});

test('Anthropic/Bedrock signed visible reasoning roundtrips even through a copied Pi message', () => {
  for (const protocol of ['anthropic_messages', 'bedrock_converse']) {
    const state = { protocol, model: 'historical-model', kind: { AnthropicSignature: { signature: 'opaque-signature' } } };
    const native = [{ Assistant: { protocol, model: state.model, content: [{ Reasoning: { text: 'visible', state } }] } }];
    const pi = canonicalToPi(native);
    assert.deepEqual(pi[0].content, [{ type: 'thinking', thinking: 'visible', thinkingSignature: 'opaque-signature' }]);
    assert.deepEqual(piToCanonical(structuredClone(pi)), native);
    pi[0].content[0].thinking = 'edited visible';
    assert.deepEqual(piToCanonical(pi)[0].Assistant.content[0], { Reasoning: { text: 'edited visible', state } });
    const wrong = structuredClone(native); wrong[0].Assistant.content[0].Reasoning.state.model = 'foreign-model';
    assert.throws(() => canonicalToPi(wrong), /signature producer must match/);
    const absent = structuredClone(native); absent[0].Assistant.content[0].Reasoning.text = null;
    assert.throws(() => canonicalToPi(absent), /absent reasoning text/);
  }
});

test('source restoration cannot conceal unknown nested fields or malformed image bytes', () => {
  for (const mutate of [part => { part.extra = undefined; }, part => { part[Symbol('hidden')] = 1; },
    part => Object.defineProperty(part, 'hidden', { value: true })]) {
    const pi = canonicalToPi([user([image()])]); mutate(pi[0].content[0]);
    assert.throws(() => piToCanonical(pi), /unsupported_feature/);
  }
  for (const data of ['%%%=', 'YQ=', 'YR==', 'YQ==\n']) {
    assert.throws(() => piToCanonical([{ role: 'user', content: [{ ...piImage(), data }] }]), /image base64/);
  }
});

test('session image entries retain original IDs, parent order, actual timestamps and Pi details including null', () => {
  for (const details of [null, { source: 'read', nested: [false, 1, 'é'] }]) {
    const entries = [entry('request', null, user([image()])), entry('assistant', 'request', assistant),
      entry('result', 'assistant', user([result()]), { tool_output: { metadata: { pi_details: details } } })];
    const before = structuredClone(entries), mirror = translateSessionEntries(entries, 'actual');
    assert.deepEqual(mirror.map(item => [item.id, item.parentId]), [['request', null], ['assistant', 'request'], ['result', 'assistant']]);
    assert.equal(mirror[0].timestamp, new Date(1234).toISOString());
    assert.equal(mirror[0].message.timestamp, 1234);
    assert.deepEqual(mirror[0].message.content, [piImage()]);
    assert.deepEqual(mirror[2].message.details, details);
    assert.equal(mirror[2].message.toolName, 'read');
    assert.deepEqual(entries, before);
  }
});

test('a reused call ID on another branch cannot change the image result tool name', () => {
  const sibling = structuredClone(assistant); sibling.Assistant.content[0].ToolCall.name = 'other-branch';
  const entries = [entry('a', null, assistant), entry('sibling', null, sibling), entry('r', 'a', user([result()]))];
  assert.equal(translateSessionEntries(entries, 'actual')[2].message.toolName, 'read');
});

test('known native gaps stay explicit: mixed durable identities, opaque replay, media without Pi bindings', () => {
  assert.throws(() => translateSessionEntries([entry('a', null, assistant), entry('mixed', 'a', user([result(), { Text: 'user text' }]))], 'actual'), /cannot fabricate extra Pi entry identities/);
  for (const patch of [{ source: { Url: 'https://example.com/image.png' } }, { detail: 'High' }, { media_type: null }]) {
    const part = image(); Object.assign(part.Media.Image, patch);
    assert.throws(() => canonicalToPi([user([part])]), /unsupported_feature/);
  }
  assert.throws(() => canonicalToPi([user([{ Media: { Audio: { payload: { Inline: 'AAAA' }, format: 'Wav', transcript: null } } }])]), /Pi 1.0.2 has no audio content type/);
  const opaque = { Assistant: { model: 'model', protocol: 'anthropic_messages', content: [{ Reasoning: { text: 'reason', state: { protocol: 'anthropic_messages', model: 'model', kind: { AnthropicRedacted: { data: 'private' } } } } }] } };
  assert.throws(() => canonicalToPi([opaque]), /opaque reasoning continuation/);
});

test('process provider-context preserves an untouched mixed media batch when a callback appends a message', async t => {
  const peer = launch(t, [join(root, 'test/fixtures/provider-context.ts')]); await peer.init(['session_entries']);
  const messages = [assistant, user([{ Text: 'before' }, image(), result(), { Text: 'after' }])];
  const reply = await peer.request('hook/run', { hook: 'provider_context',
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'context:4', owner, expected_head: 'head' },
    context: peer.context({ session_name: 'throws', session_leaf_id: 'head', session_entries: [], session_branch: [] }),
    payload: { request: { system: 'real system', messages, tools: [] }, preparation: { resource_owner: owner.session_id, session_id: 'actual-session', head: 'head', tool_generation: 7 } },
  }).response;
  assert.ok(reply.result, JSON.stringify(reply));
  assert.deepEqual(reply.result.provider_context, { system: 'real system', messages: [...messages, user([{ Text: 'second:real system' }])] });
  await peer.close();
});

test('process model-turn observation carries image result bytes and durable details', async t => {
  const peer = launch(t, [join(root, 'test/fixtures/model-turns.ts')]); await peer.init(['session_entries']);
  // Native scheduling/argument-error metadata remains an independently open gap.
  const a = structuredClone(assistant); delete a.Assistant.content[0].ToolCall.async; delete a.Assistant.content[0].ToolCall.argument_error;
  a.Assistant.protocol = 'anthropic_messages';
  a.Assistant.content.unshift({ Reasoning: { text: 'visible', state: { protocol: a.Assistant.protocol, model: a.Assistant.model, kind: { AnthropicSignature: { signature: 'opaque-signature' } } } } });
  const entries = [entry('a', null, a), entry('r', 'a', user([result()]), { tool_output: { metadata: { pi_details: null } } })];
  const reply = await peer.request('hook/run', { hook: 'model_turn_end',
    session_leaf: { grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'turn:4', owner, expected_head: 'r' },
    context: peer.context({ session_leaf_id: 'r', session_entries: entries, session_branch: entries }),
    payload: { kind: 'model_turn_end', run_id: 'run:user', turn_index: 0, timestamp_ms: 1300, assistant_entry: entries[0], tool_result_entries: [entries[1]] },
  }).response;
  assert.ok(reply.result, JSON.stringify(reply));
  const frame = await peer.wait(frame => frame.method === 'notification' && frame.params.message.startsWith('model-end:'));
  const observed = JSON.parse(frame.params.message.slice('model-end:'.length));
  assert.deepEqual(observed.message.content[0], { type: 'thinking', thinking: 'visible', thinkingSignature: 'opaque-signature' });
  assert.deepEqual(observed.tools[0], { role: 'toolResult', toolCallId: 'call', toolName: 'read', content: [{ type: 'text', text: 'image follows' }, piImage()], isError: false, details: null, timestamp: 1234 });
  await peer.close();
});

const clm = process.env.PI_CLM_PATH;
test('unchanged pinned CLM source lookup/hash validation accepts original image entry identities (pure path, not CLM acceptance)', {
  skip: !clm && 'set PI_CLM_PATH to reviewed pi-clm b84a9d7c root index.ts',
}, async () => {
  const hashes = {
    'continuity.ts': '02e6a774c77734168a0cc116062cc9699a72e052119fdba13860be221a63e564',
    'context-document.ts': 'dd3d4fc60ae3e8ed7eea74f324332a4ab2f97dcb7c22bde9f997729786011f17',
    'policy.ts': '1e60a0e7f2569be07147c3b8218119c3997db37b11ff839e41724258a1fd23a6',
    'types.ts': '87eaba5c35bfa881590698974646779037fb6a82f3c32f71ade4e0091217257f',
  };
  for (const [path, hash] of Object.entries(hashes)) assert.equal(createHash('sha256').update(readFileSync(join(dirname(clm), 'src', path))).digest('hex'), hash);
  const jiti = createJiti(import.meta.url, { moduleCache: false, fsCache: false });
  const original = await jiti.import(join(dirname(clm), 'src/continuity.ts'));
  const native = [entry('original-image-id', null, user([{ Text: 'image source' }, image()]))];
  const mirror = translateSessionEntries(native, 'actual'), message = mirror[0].message;
  assert.equal(original.findSourceEntry(mirror, structuredClone(message)).id, 'original-image-id');
  const annotation = { source: { entryId: 'original-image-id', contentHash: original.sourceContentHash(message) } };
  assert.deepEqual(original.validateAnnotationSource(annotation, mirror[0]), message);
  const corrupted = structuredClone(mirror[0]); corrupted.message.content[1].data = 'YQ==';
  assert.throws(() => original.validateAnnotationSource(annotation, corrupted), /content-hash check/);
});
