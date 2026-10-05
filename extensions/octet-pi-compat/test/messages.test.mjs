// Pi 1.0.2 sendMessage / sendUserMessage across the protocol boundary.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { crc32, inflateSync } from 'node:zlib';
import { launch } from './helper.mjs';
import { customMessageParams } from '../lib/custom-messages.mjs';
import { translateSessionEntries } from '../lib/session-mirror.mjs';

// One white grayscale/alpha pixel, with a valid IDAT CRC for strict native decoding.
const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=';
const image = { type: 'image', data: png, mimeType: 'image/png' };

test('fixture: PNG chunks and pixel payload are valid, not just canonical base64', () => {
  const bytes = Buffer.from(png, 'base64');
  assert.deepEqual(bytes.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
  const types = [];
  for (let offset = 8; offset < bytes.length;) {
    const length = bytes.readUInt32BE(offset), end = offset + 8 + length;
    const type = bytes.toString('ascii', offset + 4, offset + 8);
    types.push(type);
    assert.equal(bytes.readUInt32BE(end), crc32(bytes.subarray(offset + 4, end)), `${type} CRC`);
    if (type === 'IDAT') assert.deepEqual(inflateSync(bytes.subarray(offset + 8, end)), Buffer.from([1, 255, 255]));
    offset = end + 4;
  }
  assert.deepEqual(types, ['IHDR', 'IDAT', 'IEND']);
});

test('normalize: custom messages preserve text/image content and reject malformed images', () => {
  const input = { customType: 'job', content: [{ type: 'text', text: 'a' }, { type: 'text', text: 'b' }], display: false, details: { secret: 1 } };
  const wire = customMessageParams(input, { triggerTurn: false });
  assert.deepEqual(wire, { custom_type: 'job', content: input.content, display: false, details: input.details, trigger_turn: false });
  assert.deepEqual(customMessageParams({ customType: 'empty', display: true }).content, []);
  assert.deepEqual(customMessageParams({ customType: 'media', content: [image], display: false }).content, [image]);
  assert.throws(() => customMessageParams({ customType: 'media', content: [{ type: 'image', data: 'raw' }] }), /invalid_request/);
  assert.throws(() => customMessageParams({ customType: 'media', content: [{ ...image, data: 'AB==' }] }), /invalid_request/);
  assert.throws(() => customMessageParams({ customType: 'media', content: [{ ...image, mimeType: 'image/svg+xml' }] }), /unsupported_feature/);
  assert.throws(() => customMessageParams({ customType: 'media', content: Array(9).fill(image) }), /exceeds bounds/);
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

test('sendMessage: ordered image blocks cross as typed native custom content', async t => {
  const content = [{ type: 'text', text: 'look' }, image, { type: 'text', text: 'after' }];
  const peer = await command(t, `await pi.sendMessage({ customType: 'image', content: ${JSON.stringify(content)}, display: false, details: null });`);
  const params = peer.seen.find(f => f.method === 'session/send_message').params;
  assert.deepEqual(params.content, content);
  assert.equal(params.details, null);
  assert.equal(peer.seen.some(f => f.method === 'session/send_user_message'), false);
  await peer.close();
});

test('sendUserMessage: ordered image blocks are not joined into substitute text', async t => {
  const content = [{ type: 'text', text: 'look' }, image];
  const peer = await command(t, `await pi.sendUserMessage(${JSON.stringify(content)}, { deliverAs: 'steer' });`);
  const params = peer.seen.find(f => f.method === 'session/send_user_message').params;
  assert.deepEqual(params.content, content);
  assert.equal(params.text, '');
  assert.equal(params.deliver_as, 'steer');
  await peer.close();
});

test('sendUserMessage: deliverAs steer is forwarded', async t => {
  const peer = await command(t, `await pi.sendUserMessage('go', { deliverAs: 'steer' });`);
  const params = peer.seen.find(f => f.method === 'session/send_user_message').params;
  assert.equal(params.text, 'go');
  assert.equal(params.deliver_as, 'steer');
  await peer.close();
});
