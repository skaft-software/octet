import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { Transport, MAX_FRAME } from '../lib/transport.mjs';

function reader(limit = MAX_FRAME) {
  const input = new EventEmitter(), output = new EventEmitter(), messages = [], errors = [];
  const transport = new Transport({ output }, {
    local: true, onMessage: message => messages.push(message), onLost: error => errors.push(error),
  });
  // The physical worker uses this same reader and dispatches responses separately.
  transport.response = message => messages.push(message);
  transport.setFrameLimit(limit);
  transport.start(input);
  return { input, transport, messages, errors };
}
const encode = message => Buffer.from(JSON.stringify(message) + '\n');
function feed(input, bytes, chunkBytes) {
  for (let at = 0; at < bytes.length; at += chunkBytes) input.emit('data', bytes.subarray(at, at + chunkBytes));
}
function inputWork(action) {
  const concat = Buffer.concat, copy = Buffer.prototype.copy, indexOf = Buffer.prototype.indexOf;
  let copied = 0, scanned = 0;
  try {
    Buffer.concat = function (list, length) {
      copied += length ?? list.reduce((sum, item) => sum + item.length, 0);
      return concat.call(this, list, length);
    };
    Buffer.prototype.copy = function (target, targetStart = 0, start = 0, end = this.length) {
      copied += end - start;
      return copy.call(this, target, targetStart, start, end);
    };
    Buffer.prototype.indexOf = function (value, offset = 0, ...args) {
      const at = indexOf.call(this, value, offset, ...args);
      if (value === 10) scanned += at < 0 ? this.length - offset : at - offset + 1;
      return at;
    };
    action();
    return { copied, scanned };
  } finally { Buffer.concat = concat; Buffer.prototype.copy = copy; Buffer.prototype.indexOf = indexOf; }
}

for (const size of [65536, 262144, MAX_FRAME]) test(`fragmented ${size}-byte frames have linear copy and scan work`, () => {
  const peer = reader();
  const message = { jsonrpc: '2.0', id: 1, result: { data: '' } };
  message.result.data = 'x'.repeat(size - Buffer.byteLength(JSON.stringify(message)));
  const bytes = encode(message);
  assert.equal(bytes.length, size + 1);
  const work = inputWork(() => feed(peer.input, bytes, 4096));
  assert.deepEqual(peer.messages, [message]);
  assert.deepEqual(peer.errors, []);
  assert.ok(work.copied <= 3 * bytes.length, `copied ${work.copied} bytes for ${bytes.length} input bytes`);
  assert.equal(work.scanned, bytes.length, 'scan new input once, never the accumulated prefix');
});

test('complete frames borrow the input without copying, including many coalesced frames', () => {
  const peer = reader(128); // The combined chunk exceeds the per-frame limit.
  const messages = Array.from({ length: 64 }, (_, id) => ({ jsonrpc: '2.0', id, method: 'hook/run', params: {} }));
  const bytes = Buffer.concat(messages.map(encode));
  const work = inputWork(() => peer.input.emit('data', bytes));
  assert.deepEqual(peer.messages, messages);
  assert.deepEqual(peer.errors, []);
  assert.equal(work.copied, 0);
  assert.equal(work.scanned, bytes.length);
});

test('byte-split UTF-8, CRLF and partial/coalesced frame boundaries preserve exact order and content', () => {
  const peer = reader();
  const messages = ['👩🏽‍💻漢字\u2028\u2029', 'second', 'third'].map(text => ({ jsonrpc: '2.0', method: 'input', params: { text } }));
  const first = Buffer.from(JSON.stringify(messages[0]) + '\r\n');
  feed(peer.input, first, 1);
  const second = encode(messages[1]), third = encode(messages[2]);
  peer.input.emit('data', Buffer.concat([second, third.subarray(0, 7)]));
  peer.input.emit('data', third.subarray(7));
  assert.deepEqual(peer.messages, messages);
  assert.deepEqual(peer.errors, []);
});

for (const complete of [false, true]) test(`an oversized ${complete ? 'complete' : 'partial'} frame refuses before growing its input buffer`, () => {
  const limit = 256, peer = reader(limit);
  peer.input.emit('data', Buffer.from('{'));
  const bytes = Buffer.alloc(65536, 120);
  if (complete) bytes[bytes.length - 1] = 10;
  const work = inputWork(() => peer.input.emit('data', bytes));
  assert.equal(peer.transport.closed, true);
  assert.equal(peer.errors.length, 1);
  assert.equal(peer.errors[0].code, -32602);
  assert.match(peer.errors[0].message, /bounds_exceeded.*input frame/);
  assert.deepEqual(peer.messages, []);
  assert.equal(work.copied, 0, 'check the negotiated limit before allocating/copying the incoming segment');
});

for (const bytes of [Buffer.from([0xff, 10]), Buffer.from([0xe2, 0x28, 0xa1, 10]), Buffer.from('\n'),
  Buffer.from('null\n'), Buffer.from('{broken}\n'),
  Buffer.from('{"jsonrpc":"2.0","id":1,"result":"\\ud800"}\n')]) test('fragmented invalid input still terminalizes without delivering a message', () => {
  const peer = reader();
  feed(peer.input, bytes, 1);
  assert.equal(peer.transport.closed, true);
  assert.equal(peer.errors.length, 1);
  assert.deepEqual(peer.messages, []);
});
