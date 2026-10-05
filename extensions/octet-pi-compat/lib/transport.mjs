import { Console } from 'node:console';
import { Writable } from 'node:stream';
import { Worker } from 'node:worker_threads';
import { rpcError } from './errors.mjs';

export const MAX_FRAME = 1048576;
const MAX_QUEUE = 128, MAX_QUEUE_BYTES = 4194304;
const nativeTimeout = globalThis.setTimeout;
const nativeClearTimeout = globalThis.clearTimeout;

// Capture the only protocol writer before loading ANY foreign factory. Neither
// console nor process.stdout.write from an extension can enter this writer.
export function isolateStdout() {
  const output = process.stdout;
  const write = output.write.bind(output);
  const stderrWrite = process.stderr.write.bind(process.stderr);
  let diagnosticBytes = 0, resetAt = Date.now(), warned = false;
  const diagnostic = (chunk, encoding, callback) => {
    if (typeof encoding === 'function') { callback = encoding; encoding = undefined; }
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk), typeof encoding === 'string' ? encoding : 'utf8');
    if (Date.now() - resetAt >= 1000) { resetAt = Date.now(); diagnosticBytes = 0; warned = false; }
    diagnosticBytes += bytes.length;
    if (diagnosticBytes <= 65536 && process.stderr.writableLength < 262144) return stderrWrite(bytes, callback);
    if (!warned && process.stderr.writableLength < 262144) { warned = true; stderrWrite('[pi-compat: diagnostic output throttled]\n'); }
    if (callback) queueMicrotask(callback);
    return true;
  };
  let intentHandler;
  const rawWrite = (chunk, encoding, callback) => {
    const text = Buffer.isBuffer(chunk) ? chunk.toString('utf8') : String(chunk);
    const enabled = mouseModeIntent(text);
    if (enabled !== undefined) {
      if (!intentHandler) throw rpcError(-32601, 'unsupported_feature mouse capture before host binding');
      intentHandler(enabled);
      const cb = typeof encoding === 'function' ? encoding : callback; if (cb) queueMicrotask(cb);
      return true;
    }
    if (text.includes('\x1b')) throw rpcError(-32601, 'unsupported_feature direct terminal control write');
    return diagnostic(chunk, encoding, callback);
  };
  Object.defineProperty(output, 'write', { value: rawWrite, writable: false, configurable: false });
  for (const method of ['end', 'destroy']) Object.defineProperty(output, method, {
    value: () => { throw rpcError(-32601, `unsupported_feature process.stdout.${method}: RPC transport is host-owned`); }, writable: false, configurable: false,
  });
  const diagnosticStream = new Writable({ write(chunk, encoding, callback) { diagnostic(chunk, encoding, callback); } });
  globalThis.console = new Console({ stdout: diagnosticStream, stderr: diagnosticStream });
  return { write, output, setIntentHandler(handler) { intentHandler = handler; } };
}

export function mouseModeIntent(text) {
  if (text === '\x1b[?1000h\x1b[?1002h\x1b[?1006h') return true;
  if (text === '\x1b[?1000l\x1b[?1002l\x1b[?1006l') return false;
  return undefined;
}

export class Transport {
  constructor(writer, { onMessage, onLost, local = false }) {
    this.local = local;
    this.writer = writer;
    this.onMessage = onMessage; this.onLost = onLost;
    this.queue = []; this.bytes = 0; this.writing = false; this.closed = false;
    this.children = new Map(); this.childId = 0; this.syncIds = new Set();
    this.buffer = Buffer.alloc(0);
    this.idleWaiters = [];
    this.transfers = new Map(); this.transferId = 0; this.transferBytes = 0;
  }
  start(input = process.stdin) {
    if (!this.local && input === process.stdin) { this.startWorker(); return; }
    this.input = input;
    this.writer.output.on('error', error => this.fail(error));
    input.on('error', error => this.fail(error));
    input.on('end', () => this.fail(rpcError(-32002, 'transport EOF'), true));
    input.on('data', chunk => {
      try {
        this.buffer = Buffer.concat([this.buffer, chunk]);
        let at;
        while ((at = this.buffer.indexOf(10)) !== -1) {
          const line = this.buffer.subarray(0, at); this.buffer = this.buffer.subarray(at + 1);
          if (line.length > MAX_FRAME) throw rpcError(-32602, 'bounds_exceeded input frame');
          const message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(line));
          this.validate(message);
          if (!message.method) this.response(message);
          else Promise.resolve(this.onMessage(message)).catch(error => this.fail(error));
        }
        if (this.buffer.length > MAX_FRAME) throw rpcError(-32602, 'bounds_exceeded partial input frame');
      } catch (error) { this.fail(error); }
    });
  }
  validate(m) {
    // Malformed method arguments belong to their RPC handler's typed validation
    // (and a correlated refusal), not a connection-wide loss. Replies, especially
    // synchronous durable receipts, must be completely valid before waking JS.
    const stack = m?.method === undefined ? [[m, 0]] : [[m.method, 0], [m.id, 0]];
    while (stack.length) {
      const [value, depth] = stack.pop();
      if (depth > 32) throw rpcError(-32600, 'JSON-RPC depth exceeded');
      if (typeof value === 'string' && /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)) throw rpcError(-32600, 'invalid UTF-8 protocol string');
      if (value && typeof value === 'object') for (const [key, child] of Object.entries(value)) { stack.push([key, depth + 1], [child, depth + 1]); }
    }
    if (!m || Array.isArray(m) || m.jsonrpc !== '2.0') throw rpcError(-32600, 'invalid JSON-RPC envelope');
    if (m.id !== undefined && !(typeof m.id === 'string' && Buffer.byteLength(m.id) <= 256 || Number.isSafeInteger(m.id) && m.id >= 0)) throw rpcError(-32600, 'invalid JSON-RPC id');
    if (m.method !== undefined) {
      if (Object.keys(m).some(key => !['jsonrpc', 'id', 'method', 'params'].includes(key))) throw rpcError(-32600, 'invalid request envelope fields');
      if (typeof m.method !== 'string' || !m.method || Buffer.byteLength(m.method) > 128 || /[\x00-\x1f\x7f-\x9f]/u.test(m.method) || !m.params || typeof m.params !== 'object' || Array.isArray(m.params)) throw rpcError(-32600, 'invalid request');
    } else {
      if (Object.keys(m).some(key => !['jsonrpc', 'id', 'result', 'error'].includes(key))) throw rpcError(-32600, 'invalid response envelope fields');
      if (m.id === undefined || ('result' in m) === ('error' in m)) throw rpcError(-32600, 'invalid response');
      if ('error' in m && (!m.error || !Number.isSafeInteger(m.error.code) || typeof m.error.message !== 'string' || Buffer.byteLength(m.error.message) > 4096)) throw rpcError(-32600, 'invalid response error');
    }
  }
  send(message, coalesceKey) {
    if (this.closed) return Promise.reject(rpcError(-32002, 'transport closed'));
    const line = JSON.stringify(message) + '\n', size = Buffer.byteLength(line);
    if (size - 1 > MAX_FRAME) return Promise.reject(rpcError(-32602, 'bounds_exceeded output frame'));
    if (this.worker) return this.transfer({ kind: 'send', message, key: coalesceKey }, size);
    return new Promise((resolve, reject) => {
      if (coalesceKey) {
        const previous = this.queue.findIndex(frame => frame.key === coalesceKey);
        if (previous !== -1) {
          const old = this.queue.splice(previous, 1)[0]; this.bytes -= old.size;
          old.resolve(); // Last complete snapshot supersedes an unwritten one.
        }
      }
      if (this.queue.length >= MAX_QUEUE || this.bytes + size > MAX_QUEUE_BYTES) {
        const error = rpcError(-32012, 'bounds_exceeded protocol writer queue');
        reject(error); this.fail(error); return;
      }
      this.queue.push({ line, size, resolve, reject, key: coalesceKey }); this.bytes += size;
      this.pump();
    });
  }
  pump() {
    if (this.writing || this.closed) return;
    const frame = this.queue.shift();
    if (!frame) { for (const resolve of this.idleWaiters.splice(0)) resolve(); return; }
    this.bytes -= frame.size; this.writing = true; this.currentFrame = frame;
    // write's callback, not its return value, gates the next complete frame.
    // There is never an abandoned partial line on backpressure or cancellation.
    try {
      this.writer.write(frame.line, error => {
        this.writing = false; this.currentFrame = undefined;
        if (error) { frame.reject(error); this.fail(error); }
        else { frame.resolve(); this.pump(); }
      });
    } catch (error) { this.writing = false; this.currentFrame = undefined; frame.reject(error); this.fail(error); }
  }
  notify(method, params, key) { return this.send({ jsonrpc: '2.0', method, params }, key); }
  request(method, params, { parent, signal, timeout = 30000 } = {}) {
    if (this.children.size >= 128 || this.childId >= 65536) throw rpcError(-32012, 'bounds_exceeded host request catalog');
    signal?.throwIfAborted();
    const id = `pi:${++this.childId}`;
    return new Promise((resolve, reject) => {
      const cancel = reason => {
        if (!this.children.delete(id)) return;
        nativeClearTimeout(timer); signal?.removeEventListener('abort', abort);
        reject(reason);
        if (!this.closed) this.notify('$/cancelRequest', { id, reason: 'cancelled' }).catch(error => this.fail(error));
      };
      const abort = () => cancel(rpcError(-32800, 'request cancelled'));
      const timer = nativeTimeout(() => cancel(rpcError(-32002, `${method} timed out`)), timeout);
      this.children.set(id, { parent, resolve, reject, timer, signal, abort, cancel });
      signal?.addEventListener('abort', abort, { once: true });
      this.send({ jsonrpc: '2.0', id, method, params }).catch(cancel);
    });
  }
  response(message) {
    const child = this.children.get(message.id);
    if (!child) {
      const match = typeof message.id === 'string' && /^pi:([1-9][0-9]*)$/.exec(message.id);
      if (!match || Number(match[1]) > this.childId || this.syncIds.has(message.id)) throw rpcError(-32600, 'unknown host response id');
      return; // Known async cancelled/settled child IDs are never reused.
    }
    this.children.delete(message.id); nativeClearTimeout(child.timer);
    child.signal?.removeEventListener('abort', child.abort);
    if (message.error) child.reject(rpcError(message.error.code, message.error.message));
    else child.resolve(message.result);
  }
  cancel(id) {
    const child = this.children.get(id);
    child?.cancel(rpcError(-32800, 'request cancelled'));
  }
  settleParent(parent) {
    for (const child of [...this.children.values()]) if (child.parent === parent) child.cancel(rpcError(-32002, 'parent settled'));
  }
  idle() {
    if (this.worker) return this.transfer({ kind: 'idle' }, 0);
    return this.queue.length || this.writing ? new Promise(resolve => this.idleWaiters.push(resolve)) : Promise.resolve();
  }
  startWorker() {
    // The factory thread never attaches a reader to fd 0. One worker owns both
    // physical pipes; do not mix readline/async buffering with fs.readSync.
    this.syncBuffer = new SharedArrayBuffer(MAX_FRAME + 16);
    this.syncHeader = new Int32Array(this.syncBuffer, 0, 4);
    this.worker = new Worker(new URL('./transport-worker.mjs', import.meta.url), { workerData: { syncBuffer: this.syncBuffer } });
    this.worker.on('message', packet => {
      if (this.closed) return;
      if (packet.kind === 'lost') { this.fail(rpcError(packet.code, packet.message), packet.eof); return; }
      if (packet.kind === 'ack') {
        const pending = this.transfers.get(packet.token);
        if (!pending) { this.fail(rpcError(-32600, 'unknown worker acknowledgement')); return; }
        this.transfers.delete(packet.token); this.transferBytes -= pending.size;
        if (packet.error) pending.reject(rpcError(packet.error.code, packet.error.message)); else pending.resolve();
        return;
      }
      if (packet.kind === 'message') {
        this.worker.postMessage({ kind: 'received', token: packet.token });
        const m = packet.message;
        try {
          if (!m.method) this.response(m);
          else Promise.resolve(this.onMessage(m)).catch(error => this.fail(error));
        } catch (error) { this.fail(error); }
      }
    });
    this.worker.on('error', error => this.fail(error));
    this.worker.on('exit', code => { if (!this.closed) this.fail(rpcError(-32002, `transport worker exited (${code})`)); });
  }
  transfer(packet, size) {
    if (this.closed) return Promise.reject(rpcError(-32002, 'transport closed'));
    if (this.transfers.size >= MAX_QUEUE || this.transferBytes + size > MAX_QUEUE_BYTES) {
      const error = rpcError(-32012, 'bounds_exceeded worker transfer queue'); this.fail(error); return Promise.reject(error);
    }
    return new Promise((resolve, reject) => {
      const token = ++this.transferId;
      this.transfers.set(token, { resolve, reject, size }); this.transferBytes += size;
      this.worker.postMessage({ ...packet, token });
    });
  }
  requestSync(method, params, { parent, signal, onCancel, timeout = 30000 } = {}) {
    if (!['session/append_entry', 'tools/register', 'tools/snapshot', 'tools/set_active', 'ui/chrome', 'model/select'].includes(method)) throw rpcError(-32601, 'unsupported_feature synchronous request');
    if (!this.worker || this.closed) throw rpcError(-32002, 'synchronous transport is unavailable');
    if (this.children.size >= 128 || this.childId >= 65536) throw rpcError(-32012, 'bounds_exceeded host request catalog');
    signal?.throwIfAborted();
    if (Atomics.load(this.syncHeader, 0) === 1) throw rpcError(-32012, 'synchronous request already in flight');
    const id = `pi:${++this.childId}`, message = { jsonrpc: '2.0', id, method, params };
    const size = Buffer.byteLength(JSON.stringify(message));
    if (size > MAX_FRAME) throw rpcError(-32602, 'bounds_exceeded output frame');
    if (this.transfers.size >= MAX_QUEUE || this.transferBytes + size > MAX_QUEUE_BYTES) throw rpcError(-32012, 'bounds_exceeded worker transfer queue');
    this.syncIds.add(id);
    Atomics.store(this.syncHeader, 2, 0);
    Atomics.store(this.syncHeader, 1, 0); Atomics.store(this.syncHeader, 0, 1);
    this.worker.postMessage({ kind: 'sync', message, parent, timeout });
    // At timeout the worker requests cancellation, but continues waiting for the
    // authoritative claimed/committed outcome. A final watchdog is UNKNOWN, not
    // noncommit, and terminalizes the connection; the request is never replayed.
    const until = Date.now() + timeout + 30000;
    while (Atomics.load(this.syncHeader, 0) === 1) {
      Atomics.wait(this.syncHeader, 0, 1, Math.max(1, Math.min(1000, until - Date.now())));
      if (Date.now() >= until && Atomics.load(this.syncHeader, 0) === 1) {
        const error = rpcError(-32002, 'session append outcome unknown: transport worker deadline'); this.fail(error); throw error;
      }
    }
    const length = Atomics.load(this.syncHeader, 1), status = Atomics.load(this.syncHeader, 0);
    if (length < 1 || length > MAX_FRAME) { const error = rpcError(-32600, 'invalid synchronous response length'); this.fail(error); throw error; }
    const reply = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(this.syncBuffer, 16, length)));
    Atomics.store(this.syncHeader, 0, 0);
    if (status === 3) { const error = rpcError(reply.code, `session append outcome unknown: ${reply.message}`); this.fail(error); throw error; }
    this.validate(reply);
    if (status !== 2 || reply.id !== id) { const error = rpcError(-32600, 'invalid synchronous response identity'); this.fail(error); throw error; }
    if (Atomics.load(this.syncHeader, 2)) onCancel?.();
    if (reply.error) throw rpcError(reply.error.code, reply.error.message);
    return reply.result;
  }
  async close() {
    if (!this.worker) return;
    this.closed = true;
    await this.worker.terminate();
  }
  fail(error, eof = false) {
    if (this.closed) return;
    this.closed = true;
    for (const child of this.children.values()) {
      nativeClearTimeout(child.timer); child.signal?.removeEventListener('abort', child.abort); child.reject(error);
    }
    this.children.clear();
    this.currentFrame?.reject(error); this.currentFrame = undefined;
    for (const frame of this.queue.splice(0)) frame.reject(error);
    this.bytes = 0;
    for (const pending of this.transfers.values()) pending.reject(error);
    this.transfers.clear(); this.transferBytes = 0;
    if (this.worker) this.worker.terminate().catch(() => {});
    this.onLost(error, eof);
  }
}
