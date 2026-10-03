import { Console } from 'node:console';
import { Writable } from 'node:stream';
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
  constructor(writer, { onMessage, onLost }) {
    this.writer = writer;
    this.onMessage = onMessage; this.onLost = onLost;
    this.queue = []; this.bytes = 0; this.writing = false; this.closed = false;
    this.children = new Map(); this.childId = 0;
    this.buffer = Buffer.alloc(0);
    this.idleWaiters = [];
  }
  start(input = process.stdin) {
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
    if (!m || Array.isArray(m) || m.jsonrpc !== '2.0') throw rpcError(-32600, 'invalid JSON-RPC envelope');
    if (m.id !== undefined && !(typeof m.id === 'string' && Buffer.byteLength(m.id) <= 256 || Number.isSafeInteger(m.id) && m.id >= 0)) throw rpcError(-32600, 'invalid JSON-RPC id');
    if (m.method !== undefined) {
      if (typeof m.method !== 'string' || !m.method || !m.params || typeof m.params !== 'object' || Array.isArray(m.params)) throw rpcError(-32600, 'invalid request');
    } else if (m.id === undefined || ('result' in m) === ('error' in m)) throw rpcError(-32600, 'invalid response');
  }
  send(message, coalesceKey) {
    if (this.closed) return Promise.reject(rpcError(-32002, 'transport closed'));
    const line = JSON.stringify(message) + '\n', size = Buffer.byteLength(line);
    if (size - 1 > MAX_FRAME) return Promise.reject(rpcError(-32602, 'bounds_exceeded output frame'));
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
    this.bytes -= frame.size; this.writing = true;
    // write's callback, not its return value, gates the next complete frame.
    // There is never an abandoned partial line on backpressure or cancellation.
    try {
      this.writer.write(frame.line, error => {
        this.writing = false;
        if (error) { frame.reject(error); this.fail(error); }
        else { frame.resolve(); this.pump(); }
      });
    } catch (error) { this.writing = false; frame.reject(error); this.fail(error); }
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
    if (!child) return; // Cancelled/settled child IDs are never reused.
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
  idle() { return this.queue.length || this.writing ? new Promise(resolve => this.idleWaiters.push(resolve)) : Promise.resolve(); }
  fail(error, eof = false) {
    if (this.closed) return;
    this.closed = true;
    for (const child of this.children.values()) {
      nativeClearTimeout(child.timer); child.signal?.removeEventListener('abort', child.abort); child.reject(error);
    }
    this.children.clear();
    for (const frame of this.queue.splice(0)) frame.reject(error);
    this.bytes = 0;
    this.onLost(error, eof);
  }
}
