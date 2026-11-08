// Provider payload phases are bounded by the host's *actual* frame budget.
//
// Before the fix the adapter applied a 768 KiB body bound while the host
// transported up to one 1 MiB frame, so a legitimate long-context request was
// refused inside the host-admitted range and surfaced as one opaque
// "Parse error: provider pipeline hook failed". The host now states the budget
// it admitted (`payload_limit_bytes`), the adapter applies that exact budget to
// both the incoming body and the replacement it returns, and a body that cannot
// be framed is refused by the host with a precise reason instead of being
// silently skipped.
import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, root } from './helper.mjs';
const pipeline = join(root, 'test/fixtures/provider-pipeline.ts');
const model = { id: 'native-model', provider: 'native-provider', api: 'openai-chat' };
const ask = (peer, payload, limit) => peer.request('hook/run', { hook: 'before_provider_request', context: peer.context(),
  payload: { operation_id: 'probe', model, ...(limit === undefined ? {} : { payload_limit_bytes: limit }), payload } }).response;

test('a long-context body inside the host budget runs the phase and can be transformed', async t => {
  const peer = launch(t, [pipeline]); await peer.init(['pipeline_hooks_v1']); await peer.start();
  const reply = await ask(peer, { input: ['x'.repeat(900 * 1024)] }, 1047552);
  assert.equal(reply.error, undefined, JSON.stringify(reply.error));
  // Observable transformation from the registered handler, not a skipped phase.
  assert.equal(reply.result.provider_payload.max_output_tokens, 256);
  assert.deepEqual(reply.result.provider_payload.sequence, ['first', 'second']);
  await peer.close();
});

test('multibyte text is measured as encoded bytes, not characters', async t => {
  const peer = launch(t, [pipeline]); await peer.init(['pipeline_hooks_v1']); await peer.start();
  // 290k three-byte characters: 870 KB encoded, inside both the 900 KiB budget
  // and the 1 MiB frame. The same character count under a byte-blind check would
  // have been refused for the wrong reason.
  const reply = await ask(peer, { input: ['\u4f60'.repeat(290 * 1024)] }, 900 * 1024);
  assert.equal(reply.error, undefined, JSON.stringify(reply.error));
  await peer.close();
  const strict = launch(t, [pipeline]); await strict.init(['pipeline_hooks_v1']); await strict.start();
  // 320k characters = 960 KB encoded: still one legal frame, but over the stated
  // 900 KiB budget, so the adapter must refuse it rather than the transport.
  const refused = await ask(strict, { input: ['\u4f60'.repeat(320 * 1024)] }, 900 * 1024);
  assert.equal(refused.error?.code, -32602, JSON.stringify(refused.error));
  assert.match(refused.error?.message ?? '', /bounds_exceeded provider payload/);
  await strict.close();
});

test('escaping is counted: a body that only fits unescaped is refused', async t => {
  const peer = launch(t, [pipeline]); await peer.init(['pipeline_hooks_v1']); await peer.start();
  // 400k raw quote bytes serialize to ~800 KB once escaped: inside one frame,
  // above the 700 KiB budget. A byte-blind check would have admitted it.
  const body = { input: ['"'.repeat(400 * 1024)] };
  const reply = await ask(peer, body, 700 * 1024);
  assert.equal(reply.error?.code, -32602, JSON.stringify(reply.error));
  assert.match(reply.error?.message ?? '', /bounds_exceeded provider payload/);
  await peer.close();
});

test('a replacement that grows past the host budget is refused, not truncated', async t => {
  const peer = launch(t, [pipeline]); await peer.init(['pipeline_hooks_v1']); await peer.start();
  // The fixture appends max_output_tokens and a sequence; a budget this tight
  // admits the request but not the growth.
  const request = { input: ['x'.repeat(600)] };
  const exact = JSON.stringify(request).length;
  const reply = await ask(peer, request, exact + 8);
  assert.equal(reply.error?.code, -32602, JSON.stringify(reply.error));
  assert.match(reply.error?.message ?? '', /bounds_exceeded provider payload/);
  await peer.close();
});

test('an absent host budget keeps the conservative fallback', async t => {
  const peer = launch(t, [pipeline]); await peer.init(['pipeline_hooks_v1']); await peer.start();
  // 1008 KiB: one legal frame, above the 1000 KiB fallback budget.
  const reply = await ask(peer, { input: ['x'.repeat(1008 * 1024)] });
  assert.equal(reply.error?.code, -32602, JSON.stringify(reply.error));
  await peer.close();
});
