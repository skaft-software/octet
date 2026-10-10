import { parentPort, workerData } from 'node:worker_threads';
import { read, write } from 'node:fs';
import { Readable, Writable } from 'node:stream';
import { Socket } from 'node:net';
import { Transport, MAX_FRAME, SYNC_REPLY, SYNC_UNKNOWN, SYNC_CANCELLED } from './transport.mjs';
import { rpcError } from './errors.mjs';

const header = new Int32Array(workerData.syncBuffer, 0, 4);
const body = new Uint8Array(workerData.syncBuffer, 16);
// Pipes inherited from Node may already be O_NONBLOCK. fs streams treat EAGAIN
// as fatal; these callback-based fd streams retain partial reads/writes and
// retry readiness without a second reader or abandoning a partial JSON frame.
const retryable = error => ['EAGAIN', 'EWOULDBLOCK', 'EINTR'].includes(error?.code);
let readRetry, writeRetry;
// Node Unix pipes use libuv readiness (no idle polling). Windows lacks the
// net.Socket fd option, and Bun does not read inherited pipe fds through it.
// Both use the callback fd reader, still owned exclusively by this worker.
const input = process.platform === 'win32' || process.versions.bun ? new Readable({
  read() {
    const buffer = Buffer.alloc(65536);
    const attempt = () => read(0, buffer, 0, buffer.length, null, (error, count) => {
      if (input.destroyed) return;
      if (retryable(error)) { readRetry = setTimeout(attempt, 5); return; }
      if (error) input.destroy(error); else input.push(count ? buffer.subarray(0, count) : null);
    });
    attempt();
  },
  destroy(error, callback) { clearTimeout(readRetry); callback(error); },
}) : new Socket({ fd: 0, readable: true, writable: false });
const output = new Writable({
  write(chunk, _encoding, callback) {
    let offset = 0;
    const attempt = () => write(1, chunk, offset, chunk.length - offset, null, (error, count) => {
      if (retryable(error)) { writeRetry = setTimeout(attempt, 5); return; }
      if (error) { callback(error); return; }
      offset += count;
      if (offset < chunk.length) { writeRetry = setTimeout(attempt, count ? 0 : 5); return; }
      callback();
    });
    attempt();
  },
  destroy(error, callback) { clearTimeout(writeRetry); callback(error); },
});
let sync, sequence = 0, bytes = 0;
const forwarded = new Map();
// Async request ids this extension issued. Ids are never reused, so this is
// also the tombstone set for replies that arrive after an async request was
// cancelled or settled (the factory thread drops known ids).
const issuedAsync = new Set();
// Settled or host-cancelled synchronous request ids, bounded oldest-first. A
// reply for one of these is dropped here: the factory thread has no channel for
// a reply it no longer waits on, so forwarding it would be a protocol error.
const MAX_SETTLED_SYNC = 4096;
const settledSync = new Set();
function tombstoneSync(id) {
  if (id === undefined || settledSync.has(id)) return;
  settledSync.add(id);
  if (settledSync.size > MAX_SETTLED_SYNC) settledSync.delete(settledSync.values().next().value);
}
const activeParents = new Set(), cancelledParents = new Set();
let shutdownRequested = false;
function complete(value, status) {
  tombstoneSync(sync?.id);
  if (Atomics.load(header, 0) !== 1) return;
  clearTimeout(sync?.timer); clearTimeout(sync?.watchdog);
  const encoded = Buffer.from(JSON.stringify(value));
  if (encoded.length > MAX_FRAME) throw rpcError(-32602, 'bounds_exceeded synchronous response');
  body.set(encoded); Atomics.store(header, 1, encoded.length);
  sync = undefined;
  Atomics.store(header, 0, status); Atomics.notify(header, 0);
}
// A read-only synchronous request (a catalog/context read) has no commitment to
// learn, so a host that drops it gets terminal cancellation and the factory
// thread is woken instead of waiting out the watchdog. The host also drops the
// request silently, so its reply may never arrive.
const READ_ONLY_SYNC = new Set(['tools/snapshot', 'composition/context']);
function cancelSync() {
  if (!sync || sync.cancelled) return;
  sync.cancelled = true; Atomics.store(header, 2, 1);
  const { id, method } = sync;
  transport.notify('$/cancelRequest', { id, reason: 'cancelled' }).catch(error => transport.fail(error));
  if (READ_ONLY_SYNC.has(method)) complete({ code: -32800, message: 'request cancelled: the host dropped this read-only request' }, SYNC_CANCELLED);
}
function forward(message) {
  // Control and replies are read here even while the factory is in Atomics.wait.
  if (message.method === 'shutdown') shutdownRequested = true;
  if (message.method === '$/cancelRequest' && activeParents.has(message.params.id)) cancelledParents.add(message.params.id);
  if (message.method && message.id !== undefined) {
    if (activeParents.size >= 128 || activeParents.has(message.id)) throw rpcError(-32600, 'invalid active parent catalog');
    activeParents.add(message.id);
  }
  if (sync && (message.method === 'shutdown' || message.method === '$/cancelRequest' && [sync.parent, sync.id].includes(message.params.id))) cancelSync();
  const size = Buffer.byteLength(JSON.stringify(message));
  // The running factory must be able to receive a full 128-event editor burst
  // plus its opening reply/control frames. A blocked synchronous append keeps
  // the stricter 128-frame limit; neither path admits unbounded Port buffering.
  if (forwarded.size >= (sync ? 128 : 256) || bytes + size > 4194304) throw rpcError(-32012, 'bounds_exceeded worker input queue');
  const token = ++sequence; forwarded.set(token, size); bytes += size;
  parentPort.postMessage({ kind: 'message', token, message });
}
const transport = new Transport({ write: output.write.bind(output), output }, {
  local: true, onMessage: forward,
  onLost(error, eof) {
    complete({ code: error.code || -32002, message: String(error.message).slice(0, 4096) }, SYNC_UNKNOWN);
    parentPort.postMessage({ kind: 'lost', code: error.code || -32002, message: String(error.message).slice(0, 4096), eof });
    input.destroy(); output.destroy(); parentPort.close();
  },
});
transport.response = message => {
  if (sync && message.id === sync.id) { complete(message, SYNC_REPLY); return; }
  // A late reply for a settled or host-cancelled synchronous request is
  // dropped here; it is not a protocol error. Unknown ids still terminalize.
  if (settledSync.has(message.id)) return;
  if (issuedAsync.has(message.id)) { forward(message); return; }
  throw rpcError(-32600, 'unknown host response id');
};
parentPort.on('message', packet => {
  try {
    if (packet.kind === 'frame-limit') { transport.setFrameLimit(packet.bytes); return; }
    if (packet.kind === 'received') {
      const size = forwarded.get(packet.token);
      if (size === undefined) throw rpcError(-32600, 'unknown worker input acknowledgement');
      forwarded.delete(packet.token); bytes -= size; return;
    }
    if (packet.kind === 'sync') {
      if (sync || !['session/append_entry', 'session/setup', 'tools/register', 'tools/snapshot', 'tools/set_active', 'ui/chrome', 'model/select', 'composition/context'].includes(packet.message.method)) throw rpcError(-32600, 'invalid synchronous request');
      sync = { id: packet.message.id, parent: packet.parent, method: packet.message.method };
      sync.timer = setTimeout(cancelSync, packet.timeout);
      sync.watchdog = setTimeout(() => transport.fail(rpcError(-32002, 'session append outcome unknown: reply deadline')), packet.timeout + 29000);
      transport.send(packet.message).catch(error => transport.fail(error));
      if (shutdownRequested || cancelledParents.has(sync.parent)) cancelSync();
      return;
    }
    if (packet.kind === 'send' && !packet.message.method && packet.message.id !== undefined) {
      activeParents.delete(packet.message.id); cancelledParents.delete(packet.message.id);
    }
    if (packet.kind === 'send' && packet.message.method && packet.message.id !== undefined) {
      if (issuedAsync.size >= 65536 || issuedAsync.has(packet.message.id)) throw rpcError(-32600, 'invalid host request identity');
      issuedAsync.add(packet.message.id);
    }
    const work = packet.kind === 'send' ? transport.send(packet.message, packet.key) : packet.kind === 'idle' ? transport.idle() : Promise.reject(rpcError(-32600, 'unknown worker command'));
    work.then(() => parentPort.postMessage({ kind: 'ack', token: packet.token }), error => {
      parentPort.postMessage({ kind: 'ack', token: packet.token, error: { code: error.code || -32002, message: error.message } });
    });
  } catch (error) { transport.fail(error); }
});
process.on('uncaughtException', error => transport.fail(error));
process.on('unhandledRejection', error => transport.fail(error));
transport.start(input);
