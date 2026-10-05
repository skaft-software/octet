// Pi 1.0.2 sendMessage / sendUserMessage across the protocol boundary.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';
import { customMessageParams } from '../lib/custom-messages.mjs';
import { translateSessionEntries } from '../lib/session-mirror.mjs';

test('normalize: custom messages preserve content shape and reject unsupported image blocks', () => {
  const input = { customType: 'job', content: [{ type: 'text', text: 'a' }, { type: 'text', text: 'b' }], display: false, details: { secret: 1 } };
  const wire = customMessageParams(input, { triggerTurn: false });
  assert.deepEqual(wire, { custom_type: 'job', content: input.content, display: false, details: input.details, trigger_turn: false });
  assert.deepEqual(customMessageParams({ customType: 'empty', display: true }).content, []);
  assert.throws(() => customMessageParams({ customType: 'media', content: [{ type: 'image', data: 'raw' }] }), /unsupported_feature/);
});

test('mirror: typed durable custom messages have Pi custom_message identity, details, and visibility', () => {
  const entry = { id: '003', parent: '002', timestamp_unix_ms: 1700000000000,
    value: { type: 'message', User: { content: [{ Text: 'model body' }] } },
    metadata: { custom_message: { custom_type: 'job', content: [{ type: 'text', text: 'model body' }], display: false, details: { secret: 1 } } } };
  assert.deepEqual(translateSessionEntries([entry], 'octet-pi-compat'), [{ id: '003', parentId: '002', timestamp: new Date(entry.timestamp_unix_ms).toISOString(),
    type: 'custom_message', customType: 'job', content: [{ type: 'text', text: 'model body' }], display: false, details: { secret: 1 } }]);
});

async function command(t, body) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-messages-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'index.ts');
  await writeFile(path, `export default pi => pi.registerCommand('probe', { handler: async () => { ${body} } });`);
  const peer = launch(t, [path]);
  await peer.init();
  const reply = await peer.command('probe').response;
  assert.ok(!reply.error, JSON.stringify(reply.error));
  return peer;
}

test('sendMessage: one typed custom message with Pi delivery options, no substitute user message', async t => {
  const peer = await command(t, `await pi.sendMessage({ customType: 'job', content: 'done', display: false, details: { id: 1 } }, { deliverAs: 'followUp', triggerTurn: true });`);
  const sent = peer.seen.filter(f => f.method === 'session/send_message');
  assert.equal(sent.length, 1);
  const { parent_request_id, resource_owner, ...params } = sent[0].params;
  assert.deepEqual(params, { custom_type: 'job', content: 'done', display: false, details: { id: 1 }, deliver_as: 'follow_up', trigger_turn: true });
  assert.equal(peer.seen.some(f => ['session/send_user_message', 'session/append_entry'].includes(f.method)), false);
  await peer.close();
});

test('sendMessage: content blocks preserve identity and nextTurn passes through', async t => {
  const peer = await command(t, `await pi.sendMessage({ customType: 'note', content: [{ type: 'text', text: 'a' }, { type: 'text', text: 'b' }], display: true }, { deliverAs: 'nextTurn' });`);
  const params = peer.seen.find(f => f.method === 'session/send_message').params;
  assert.deepEqual(params.content, [{ type: 'text', text: 'a' }, { type: 'text', text: 'b' }]);
  assert.equal(params.deliver_as, 'next_turn');
  assert.equal(params.trigger_turn, undefined);
  await peer.close();
});

test('sendUserMessage: deliverAs steer is forwarded', async t => {
  const peer = await command(t, `await pi.sendUserMessage('go', { deliverAs: 'steer' });`);
  const params = peer.seen.find(f => f.method === 'session/send_user_message').params;
  assert.equal(params.text, 'go');
  assert.equal(params.deliver_as, 'steer');
  await peer.close();
});
