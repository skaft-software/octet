import { diagnosticConsole } from './diagnostics.mjs';
import { setTimeout as delay } from 'node:timers/promises';
import { json, schema, matches, object, own } from './schema.mjs';
import {createHash} from 'node:crypto';
import {closed, text, opaque, diagnosticSummary, mediaParts, blobSchema} from './values.mjs';
import {resourceSlots, Resources} from './resources.mjs';
import {HOOK_FEATURES, SERVICES, hookResult, sessionLeaf} from './hooks.mjs';
export {validateDiagnostics, diagnosticSummary, blobSchema, validateBlob} from './values.mjs';
export {resourceType} from './resources.mjs';

export const API_VERSION = '0.4';
const FRAME_BYTES = 1_048_576; // Stateful host bound includes the LF.
const REQUIRED = ['request_cancellation', 'content_parts'];
const SUPPORTED = new Set([...REQUIRED, 'request_progress']);
const SERVICE_FEATURES = new Set([...SERVICES.values()].filter(Boolean));
export class HostRequestError extends Error {
  constructor(code, message, data) { super(message); this.name = 'HostRequestError'; this.code = code; this.data = data; }
}
const identifier = value => typeof value === 'string' && /^[A-Za-z_][A-Za-z0-9_.-]{0,63}$/.test(value);
const idValid = value => Number.isSafeInteger(value) && value >= 0; // Host-issued IDs are numeric.
const strings = value => Array.isArray(value) && value.every(v => typeof v === 'string') && new Set(value).size === value.length;
const errors = new Map([[-32700, 'Parse error'], [-32600, 'Invalid Request'], [-32601, 'Method not found'],
  [-32602, 'Invalid params'], [-32603, 'Internal error'], [-32800, 'Request cancelled'], [-32000, 'Server error']]);
class RpcError extends Error { constructor(code) { super(errors.get(code)); this.code = code; } }
export class CancelledError extends Error { constructor() { super('Request cancelled'); this.name = 'CancelledError'; } }
export class ToolError extends Error { constructor(message) { super(message); this.name = 'ToolError'; } }
export class UnsupportedFeatureError extends Error { constructor(feature) { super(`Unsupported feature: ${feature}`); this.name = 'UnsupportedFeatureError'; } }

function fields(value, allowed) {
  if (!object(value) || Object.keys(value).some(key => !allowed.includes(key))) throw new RpcError(-32602);
}
function contextValid(value) {
  if (!object(value) || typeof value.workspace !== 'string' || !object(value.host) ||
      !(value.execution_scope === undefined || value.execution_scope === null || typeof value.execution_scope === 'string')) return false;
  const owner = value.resource_owner;
  return owner === undefined || owner === null || object(owner) &&
    typeof owner.session_id === 'string' && owner.session_id.length > 0 &&
    typeof owner.extension_instance_id === 'string' && owner.extension_instance_id.length > 0 &&
    idValid(owner.process_generation);
}

/** Current feature-negotiated process authoring; unrelated to canonical API 0.3. */
export class Extension {
  #tools = new Map();
  #commands = new Map();
  #hooks = new Map();
  #features = new Set();
  #running = false;
  #shutdown;
  #concurrency;
  constructor(options = {}) {
    fields(options, ['maxConcurrentRequests', 'features']);
    if (own(options, 'features')) {
      if (!strings(options.features) || options.features.some(f => !SERVICE_FEATURES.has(f) || f === 'resource_refs_v1')) throw new TypeError('Invalid service features');
      this.#features = new Set(options.features);
    }
    const concurrency = options.maxConcurrentRequests ?? 4;
    if (!Number.isSafeInteger(concurrency) || concurrency < 1 || concurrency > 64) throw new TypeError('maxConcurrentRequests must be 1..64');
    this.#concurrency = concurrency;
  }
  tool(definition, handler) {
    this.#register(this.#tools, definition, handler, ['name', 'description', 'parameters', 'outputSchema', 'receiver']);
    const parameters = schema(definition.parameters);
    const output = own(definition, 'outputSchema') ? {output_schema: schema(definition.outputSchema, false)} : {};
    const inputs = resourceSlots(definition.parameters), outputs = resourceSlots(definition.outputSchema);
    if (own(definition, 'receiver') && !inputs.some(s => s.path === definition.receiver)) throw new TypeError('Receiver must name a resource input');
    const operation = inputs.length || outputs.length ? {operation: {id: definition.name,
      ...(own(definition, 'receiver') ? {receiver: definition.receiver} : {}),
      resource_inputs: inputs.map(({path, type}) => ({path, type, access: 'exclusive'})),
      resource_outputs: outputs.map(({path, type}) => ({path, type}))}} : {};
    if (operation.operation) { this.#features.add('resource_refs_v1'); this.#features.add('operation_descriptors_v1'); }
    const hasBlob = s => s === blobSchema || Object.values(s?.properties ?? {}).some(hasBlob) || !!s?.items && hasBlob(s.items);
    if (hasBlob(definition.parameters) || hasBlob(definition.outputSchema)) this.#features.add('bulk_objects_v1');
    this.#tools.set(definition.name, { definition: {name: definition.name, description: definition.description, parameters, ...output, ...operation}, inputs, outputs, handler });
    return this;
  }
  /** Return a typed value with an explicit, bounded model-facing projection. */
  typedTool(definition, handler, project) {
    if (!own(definition, 'outputSchema') || typeof handler !== 'function' || typeof project !== 'function') {
      throw new TypeError('typedTool requires outputSchema, handler, and a text projection');
    }
    const output = schema(definition.outputSchema, false);
    return this.tool(definition, async (arguments_, context) => {
      const value = await handler(arguments_, context);
      context.throwIfCancelled();
      json(value, 262144);
      if (!matches(output, value)) throw new TypeError('Typed output does not match outputSchema');
      const text = project(value);
      if (typeof text !== 'string' || Buffer.byteLength(text) > 65536) throw new TypeError('Text projection exceeds 64 KiB');
      return {text, structuredContent: value};
    });
  }
  command(definition, handler) {
    this.#register(this.#commands, definition, handler, ['name', 'description', 'usage']);
    if (own(definition, 'usage') && typeof definition.usage !== 'string') throw new TypeError('usage must be a string');
    this.#commands.set(definition.name, { definition: {...definition}, handler });
    return this;
  }
  hook(name, handler) {
    if (this.#running || !HOOK_FEATURES.has(name) || this.#hooks.has(name) || typeof handler !== 'function') throw new TypeError('Invalid or duplicate hook');
    this.#hooks.set(name, handler);
    const feature = HOOK_FEATURES.get(name); if (feature) this.#features.add(feature);
    return this;
  }
  onShutdown(handler) {
    if (this.#running || this.#shutdown || typeof handler !== 'function') throw new TypeError('Register one shutdown handler before run');
    this.#shutdown = handler;
    return this;
  }
  #register(catalog, definition, handler, allowed) {
    if (this.#running) throw new TypeError('Registration is frozen at run');
    fields(definition, allowed);
    if (!identifier(definition.name) || typeof definition.description !== 'string' || !definition.description.trim() ||
        typeof handler !== 'function' || catalog.has(definition.name) || catalog.size >= 256) throw new TypeError('Invalid or duplicate registration');
  }
  /** Used by the local manifest tool. Does not start a process or run handlers. */
  contributions() { return {tools: [...this.#tools.keys()], commands: [...this.#commands.keys()], hooks: [...this.#hooks.keys()]}; }
  /** Owns process stdio and exits on shutdown, EOF, or terminal transport failure. */
  run() {
    if (this.#running) throw new TypeError('Extension is already running');
    this.#running = true;
    new Runtime(this.#tools, this.#commands, this.#concurrency, this.#shutdown, this.#hooks, this.#features).start();
  }
}

class Runtime {
  constructor(tools, commands, concurrency, shutdown, hooks, requestedFeatures) {
    this.hooks = hooks; this.requestedFeatures = requestedFeatures;
    this.resources = new Resources(tools); this.children = new Map(); this.childSequence = 0;
    this.tools = tools; this.commands = commands; this.concurrency = concurrency; this.shutdown = shutdown;
    this.state = 'initializing'; this.features = new Set(); this.active = new Map(); this.pending = [];
    this.outbound = []; this.writing = false; this.frame = Buffer.allocUnsafe(FRAME_BYTES - 1); this.bytes = 0;
    this.write = process.stdout.write.bind(process.stdout);
    this.diagnostics = diagnosticConsole();
  }
  log(...args) { this.diagnostics.error(...args); }
  start() {
    globalThis.console = this.diagnostics;
    process.stdout.write = () => { throw new Error('stdout is reserved; use console.error or context.progress'); };
    process.stdout.on('error', () => this.fatal('stdout transport failed'));
    process.stdin.on('error', () => this.fatal('stdin transport failed'));
    process.stdin.on('data', chunk => this.read(chunk));
    process.stdin.on('end', () => {
      if (this.bytes) this.log('Discarded incomplete final frame');
      if (this.state !== 'draining') void this.stop(undefined, 'transport_lost');
    });
    this.initializeTimer = setTimeout(() => this.fatal('initialize deadline exceeded'), 30_000);
  }
  fatal(message) {
    this.log(message);
    process.exit(1);
  }
  send(message) {
    let frame;
    try { frame = json(message, FRAME_BYTES - 1) + '\n'; }
    catch { return Promise.reject(new RpcError(-32603)); }
    if (this.outbound.length >= 128) this.fatal('writer queue exceeded 128 frames');
    return new Promise((resolve, reject) => {
      this.outbound.push({frame, resolve, reject});
      this.flush();
    });
  }
  flush() {
    if (this.writing || !this.outbound.length) return;
    this.writing = true;
    const item = this.outbound.shift();
    this.write(item.frame, error => {
      if (error) this.fatal('writer transport failed');
      this.writing = false;
      item.resolve();
      this.flush();
    });
  }
  reply(id, value, error = false) {
    const message = {jsonrpc: '2.0', id, ...(error ? {error: {code: value, message: errors.get(value)}} : {result: value})};
    return this.send(message).catch(() => {
      if (error) this.fatal('cannot serialize error response');
      return this.reply(id, -32603, true);
    });
  }
  read(chunk) {
    if (this.state === 'stopped') return;
    let start = 0;
    while (start < chunk.length) {
      const lf = chunk.indexOf(10, start);
      const end = lf < 0 ? chunk.length : lf;
      const fragment = chunk.subarray(start, end);
      if (this.bytes + fragment.length >= FRAME_BYTES) return this.fatal('frame exceeds 1 MiB including LF');
      fragment.copy(this.frame, this.bytes); this.bytes += fragment.length;
      if (lf < 0) return;
      let text;
      try { text = new TextDecoder('utf-8', {fatal: true, ignoreBOM: true}).decode(this.frame.subarray(0, this.bytes)); }
      catch { return this.fatal('invalid UTF-8 frame'); }
      this.bytes = 0;
      let message;
      try { message = JSON.parse(text); }
      catch { void this.reply(null, -32700, true); start = lf + 1; continue; }
      this.receive(message);
      start = lf + 1;
    }
  }
  receive(message) {
    if (object(message) && !own(message, 'method') && (own(message, 'result') || own(message, 'error'))) { this.receiveResponse(message); return; }
    const hasId = object(message) && own(message, 'id');
    const id = hasId && idValid(message.id) ? message.id : null;
    // An envelope ID owns a response channel even if its envelope/params are
    // malformed. Never reject a duplicate on the live original's channel.
    // Cancellation notifications have no envelope ID; their params.id is a target.
    if (hasId && id !== null && this.active.has(id)) { this.fatal('duplicate active host request ID'); return; }
    try { json(message); }
    catch { void this.reply(id, -32600, true); return; }
    if (!object(message) || message.jsonrpc !== '2.0' || typeof message.method !== 'string' || !message.method ||
        hasId && id === null || Object.keys(message).some(key => !['jsonrpc', 'id', 'method', 'params'].includes(key))) {
      void this.reply(id, -32600, true); return;
    }
    const {method} = message;
    const params = own(message, 'params') ? message.params : {};
    if (method === '$/cancelRequest') {
      if (hasId || !object(params) || !(idValid(params.id) || typeof params.id === 'string' && Buffer.byteLength(params.id) <= 256) ||
          Object.keys(params).some(key => !['id', 'reason'].includes(key)) ||
          own(params, 'reason') && (typeof params.reason !== 'string' || Buffer.byteLength(params.reason) > 4096)) {
        if (hasId) void this.reply(id, -32602, true); else this.log('Invalid cancellation notification');
        return;
      }
      if (typeof params.id === 'string') { this.children.get(params.id)?.cancel(false); return; }
      const job = this.active.get(params.id);
      if (job && !job.controller.signal.aborted) {
        job.controller.abort(new CancelledError());
        if (!job.started) { this.pending = this.pending.filter(item => item !== job); void this.finish(job, undefined, -32800); }
        else job.cancelTimer = setTimeout(() => this.fatal('handler did not settle within cancellation grace'), 2000);
      }
      return;
    }
    if (!hasId) { this.log('Unsupported notification'); return; }
    if (method === 'initialize') {
      if (this.state !== 'initializing') { void this.reply(id, -32600, true); return; }
      try {
        const result = this.initialize(params);
        try { json({jsonrpc: '2.0', id, result}, FRAME_BYTES - 1); } catch { throw new RpcError(-32603); }
        this.state = 'ready'; clearTimeout(this.initializeTimer);
        void this.reply(id, result);
      } catch (error) { void this.reply(id, error instanceof RpcError ? error.code : -32602, true); }
      return;
    }
    if (this.state !== 'ready') { void this.reply(id, this.state === 'draining' ? -32000 : -32600, true); return; }
    if (method === 'shutdown') {
      try { fields(params, []); } catch { void this.reply(id, -32602, true); return; }
      void this.stop(id, 'shutdown'); return;
    }
    if (!['tool/call', 'command/execute', 'hook/run', ...(this.features.has('resource_refs_v1') ? ['resource/dispose'] : [])].includes(method)) { void this.reply(id, -32601, true); return; }
    if (this.active.size >= 64) { void this.reply(id, -32000, true); return; }
    const job = {id, method, params, controller: new AbortController(), started: false, sequence: 0};
    if (method === 'resource/dispose') {
      try { job.retired = this.resources.retire(params); } catch { void this.reply(id, -32602, true); return; }
    }
    this.active.set(id, job); this.pending.push(job); this.pump();
  }
  initialize(params) {
    if (!object(params) || params.api_version !== API_VERSION ||
        process.env.OCTET_EXTENSION_API_VERSION && process.env.OCTET_EXTENSION_API_VERSION !== API_VERSION) throw new RpcError(-32000);
    if (!object(params.extension) || !identifier(params.extension.name) || typeof params.extension.version !== 'string' ||
        typeof params.workspace !== 'string' || !object(params.capabilities) || !object(params.host) || !object(params.contributes)) throw new RpcError(-32602);
    const contributes = params.contributes;
    for (const [name, catalog] of [['tools', this.tools], ['commands', this.commands], ['hooks', this.hooks]]) {
      const names = contributes[name] ?? [];
      if (!strings(names) || names.length !== catalog.size || names.some(n => !catalog.has(n))) throw new RpcError(-32602);
    }
    // Do not accept declarations we cannot serve, rather than returning no-op handlers.
    for (const [key, value] of Object.entries(contributes)) {
      if (['tools', 'commands', 'hooks'].includes(key)) continue;
      if (Array.isArray(value) ? value.length !== 0 : value !== false && value !== null) throw new RpcError(-32602);
    }
    const p = params.protocol;
    const supported = new Set([...SUPPORTED, ...this.requestedFeatures]);
    if (!object(p) || p.version !== API_VERSION || !strings(p.required_features) || !strings(p.optional_features) ||
        p.required_features.some(f => !supported.has(f)) || REQUIRED.some(f => !p.required_features.includes(f)) ||
        p.optional_features.some(f => p.required_features.includes(f)) || !object(p.limits) ||
        !Number.isSafeInteger(p.limits.max_concurrent_requests) || p.limits.max_concurrent_requests < 1) throw new RpcError(-32602);
    if ([...this.requestedFeatures].some(f => !p.required_features.includes(f) && !p.optional_features.includes(f))) throw new RpcError(-32602);
    this.features = new Set([...p.required_features, ...p.optional_features.filter(f => supported.has(f))]);
    if (this.features.has('resource_refs_v1')) {
      const limits = p.limits.resource_refs_v1;
      if (!object(limits) || limits.max_records !== 256 || limits.max_registrations_per_parent !== 32) throw new RpcError(-32602);
    }
    if (this.features.has('bulk_objects_v1')) {
      const bulk = p.bulk_objects_v1;
      if (!object(bulk) || bulk.profile !== 'local-file.v1' || typeof bulk.transfer_directory !== 'string' || !bulk.transfer_directory.startsWith('/') || !object(bulk.limits) ||
          ['object_bytes', 'owner_bytes', 'write_tickets_per_generation', 'read_leases_per_generation', 'blobs_per_owner'].some(k => !Number.isSafeInteger(bulk.limits[k]) || bulk.limits[k] < 1)) throw new RpcError(-32602);
      this.bulk = Object.freeze(JSON.parse(json(bulk)));
    }
    this.concurrency = Math.min(this.features.has('resource_refs_v1') ? 1 : this.concurrency, p.limits.max_concurrent_requests);
    return {api_version: API_VERSION, tools: [...this.tools.values()].map(t => t.definition),
      commands: [...this.commands.values()].map(c => c.definition),
      protocol: {version: API_VERSION, features: [...this.features], limits: {max_concurrent_requests: this.concurrency,
        ...(this.features.has('resource_refs_v1') ? {resource_refs_v1: {max_records: 256, max_registrations_per_parent: 32}} : {})}}};
  }
  receiveResponse(message) {
    try {
      json(message);
      closed(message, ['jsonrpc', 'id'], ['result', 'error']);
      if (message.jsonrpc !== '2.0' || typeof message.id !== 'string' || own(message, 'result') === own(message, 'error')) throw new TypeError();
      if (own(message, 'error')) {
        closed(message.error, ['code', 'message'], ['data']);
        if (!Number.isSafeInteger(message.error.code) || typeof message.error.message !== 'string' || Buffer.byteLength(message.error.message) > 4096) throw new TypeError();
      }
    } catch { this.fatal('invalid host response'); return; }
    const child = this.children.get(message.id);
    if (!child) return; // Late replies never restore a retired parent or reuse an ID.
    child.settle(own(message, 'error') ? new HostRequestError(message.error.code, message.error.message, message.error.data) : undefined, message.result);
  }
  request(job, method, params) {
    let grant;
    try {
      if (job.controller.signal.aborted) throw new CancelledError();
      if (this.state !== 'ready' || this.active.get(job.id) !== job || !job.started) throw new Error('Request already settled');
      if (!SERVICES.has(method)) throw new UnsupportedFeatureError(method);
      const feature = SERVICES.get(method);
      if (feature && !this.features.has(feature)) throw new UnsupportedFeatureError(feature);
      if (!object(params) || ['parent_request_id', 'resource_owner', 'session_leaf'].some(k => own(params, k))) throw new TypeError('Authority is host-derived');
      if ((method.startsWith('resource/') || method.startsWith('bulk/')) && job.method !== 'tool/call') throw new TypeError('Service requires a tool parent');
      json(params);
      if (method === 'session/append_entry' && job.leaf) {
        if (!job.leaf.grant) throw new Error('Session leaf exhausted or pending');
        grant = job.leaf.grant;
      }
      if (this.children.size >= 32 || this.childSequence >= 65536) throw new Error('Host request quota exceeded');
    } catch (error) { return Promise.reject(error); }
    const id = `sdk:${++this.childSequence}`;
    const wire = {...params, parent_request_id: job.id};
    if (grant) {
      job.leaf.grant = null;
      const {grant_id, activation_epoch, operation_id} = grant;
      wire.session_leaf = {grant_id, activation_epoch, operation_id};
    }
    const promise = new Promise((resolve, reject) => {
      const child = {job};
      child.settle = (error, result) => {
        if (this.children.get(id) !== child) return;
        this.children.delete(id); clearTimeout(child.timer); job.controller.signal.removeEventListener('abort', abort);
        if (!error && grant) {
          try {
            closed(result, ['entry_id', 'head', 'successor']); text(result.entry_id, 256);
            if (result.head !== result.entry_id) throw new TypeError('Session leaf commit mismatch');
            job.leaf.grant = result.successor === null ? null : sessionLeaf(result.successor, job.params.context.resource_owner, grant, result.head);
          } catch (invalid) { error = invalid; }
        }
        if (error) reject(error); else resolve(result);
      };
      child.cancel = notify => {
        if (this.children.get(id) !== child) return;
        if (notify) void this.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id}}).catch(() => this.fatal('child cancellation write failed'));
        child.settle(new CancelledError());
      };
      const abort = () => child.cancel(true);
      this.children.set(id, child); job.controller.signal.addEventListener('abort', abort, {once: true});
      child.timer = setTimeout(() => child.cancel(true), 30000);
      void this.send({jsonrpc: '2.0', id, method, params: wire}).catch(error => child.settle(error));
    });
    // Unawaited author promises are cancelled at parent settlement, not leaked.
    void promise.catch(() => {});
    return promise;
  }
  pump() {
    if (this.state !== 'ready') return;
    let running = [...this.active.values()].filter(job => job.started).length;
    while (running < this.concurrency && this.pending.length) {
      const job = this.pending.shift(); job.started = true; running++;
      // Async handlers must yield; CPU-bound JS still needs host process supervision.
      void this.execute(job);
    }
  }
  context(job) {
    const throwIfCancelled = () => { if (job.controller.signal.aborted) throw new CancelledError(); };
    const live = () => { throwIfCancelled(); if (this.active.get(job.id) !== job || !job.started) throw new Error('Request already settled'); };
    const request = (method, params = {}) => this.request(job, method, params);
    return Object.freeze({...JSON.parse(json(job.params.context)), signal: job.controller.signal, throwIfCancelled,
      supports: feature => this.features.has(feature), request,
      bulk: this.bulk ? JSON.parse(json(this.bulk)) : undefined,
      exportResource: (type, value) => { live(); return this.resources.export(job, type, value, request); },
      resource: ref => { live(); return this.resources.resolve(job, ref); },
      releaseResource: ref => { live(); this.resources.lookup(job, ref); return request('resource/release', {resource: ref}); },
      publishArtifact: async (bytes, mimeType) => {
        live();
        if (!(bytes instanceof Uint8Array) || bytes.byteLength > 262144) throw new TypeError('Inline artifact exceeds 256 KiB');
        text(mimeType, 128, false, true);
        const snapshot = Buffer.from(bytes);
        const result = await request('artifact/publish', {mime_type: mimeType, size: snapshot.length,
          sha256: createHash('sha256').update(snapshot).digest('hex'), data: {encoding: 'base64', data: snapshot.toString('base64')}});
        closed(result, ['artifact_id']); if (!opaque(result.artifact_id)) throw new TypeError('Invalid artifact identity');
        return result.artifact_id;
      },
      supportsProgress: this.features.has('request_progress'),
      sleep: async ms => {
        if (!Number.isSafeInteger(ms) || ms < 0 || ms > 2_147_483_647) throw new TypeError('Invalid sleep duration');
        throwIfCancelled();
        try { await delay(ms, undefined, {signal: job.controller.signal}); } catch { throwIfCancelled(); }
        throwIfCancelled();
      },
      progress: async (message, counters = {}) => {
        throwIfCancelled();
        if (this.active.get(job.id) !== job || !job.started) throw new Error('Request already settled');
        if (!this.features.has('request_progress')) throw new UnsupportedFeatureError('request_progress');
        fields(counters, ['current', 'total', 'unit']);
        if (typeof message !== 'string' || Buffer.byteLength(message) > 8192 ||
            own(counters, 'unit') && (typeof counters.unit !== 'string' || Buffer.byteLength(counters.unit) > 256) ||
            ['current', 'total'].some(k => own(counters, k) && !idValid(counters[k]))) throw new TypeError('Invalid progress status');
        await this.send({jsonrpc: '2.0', method: '$/progress', params: {request_id: job.id, sequence: ++job.sequence,
          event: {type: 'status', message, ...counters}}});
      },
    });
  }
  async execute(job) {
    try {
      const p = job.params;
      if (job.method === 'resource/dispose') { await this.finish(job, await this.resources.dispose(job.retired)); return; }
      if (job.method === 'hook/run') {
        fields(p, ['hook', 'payload', 'context', 'session_leaf']);
        if (!contextValid(p.context) || !object(p.payload)) throw new RpcError(-32602);
        const handler = this.hooks.get(p.hook); if (!handler) throw new RpcError(-32601);
        if (own(p, 'session_leaf')) {
          try { job.leaf = {grant: sessionLeaf(p.session_leaf, p.context.resource_owner)}; }
          catch { throw new RpcError(-32602); }
        }
        const ctx = this.context(job); ctx.throwIfCancelled();
        const result = hookResult(p.hook, await handler(p.payload, ctx)); ctx.throwIfCancelled();
        await this.finish(job, result); return;
      }
      fields(p, ['name', 'arguments', 'context']);
      if (!identifier(p.name) || !contextValid(p.context)) throw new RpcError(-32602);
      const isTool = job.method === 'tool/call';
      const registration = (isTool ? this.tools : this.commands).get(p.name);
      if (!registration) throw new RpcError(-32601);
      if (isTool ? !matches(registration.definition.parameters, p.arguments) :
          !Array.isArray(p.arguments) || !p.arguments.every(v => typeof v === 'string')) throw new RpcError(-32602);
      if (isTool) { try { this.resources.admit(job, registration); } catch { throw new RpcError(-32602); } }
      const ctx = this.context(job); ctx.throwIfCancelled();
      let value;
      try { value = await registration.handler(p.arguments, ctx); }
      catch (error) {
        if (isTool && error instanceof ToolError) value = {text: error.message, isError: true};
        else throw error;
      }
      ctx.throwIfCancelled();
      const result = isTool ? this.toolResult(value, registration.definition.output_schema) : this.commandResult(value);
      await this.finish(job, result);
    } catch (error) {
      const code = job.controller.signal.aborted || error instanceof CancelledError ? -32800 : error instanceof RpcError ? error.code : -32603;
      await this.finish(job, undefined, code);
    }
  }
  toolResult(value, outputSchema) {
    if (typeof value === 'string') value = {text: value};
    if (!object(value) || Object.keys(value).some(key => !['text', 'isError', 'structuredContent', 'diagnostics', 'media'].includes(key))) throw new RpcError(-32603);
    if (typeof value.text !== 'string' || own(value, 'isError') && typeof value.isError !== 'boolean') throw new RpcError(-32603);
    const structured = own(value, 'structuredContent');
    if (structured) {
      json(value.structuredContent, 262144);
      if (!outputSchema || !matches(outputSchema, value.structuredContent)) throw new RpcError(-32603);
    } else if (outputSchema && !value.isError) throw new RpcError(-32603);
    const content = [{type: 'text', text: value.text}];
    if (own(value, 'media')) {
      if (!this.features.has('artifacts')) throw new UnsupportedFeatureError('artifacts');
      mediaParts(value.media); content.push(...value.media);
    }
    let metadata;
    if (own(value, 'diagnostics')) {
      const summary = diagnosticSummary(value.diagnostics);
      metadata = {octet_diagnostics_v1: value.diagnostics};
      if (summary) content.push({type: 'text', text: summary});
    }
    return {content, is_error: value.isError ?? false, ...(metadata ? {metadata} : {}),
      ...(structured ? {structured_content: value.structuredContent} : {})};
  }
  commandResult(value) {
    if (typeof value !== 'string') throw new RpcError(-32603);
    return {text: value, context: [], notifications: []};
  }
  async finish(job, result, code) {
    clearTimeout(job.cancelTimer);
    // Claim the terminal outcome before any await; exactly one result/error wins.
    if (this.active.get(job.id) !== job) return;
    this.active.delete(job.id);
    for (const child of [...this.children.values()]) if (child.job === job) child.cancel(true);
    await this.reply(job.id, code ?? result, code !== undefined);
    this.pump();
  }
  async stop(id, reason) {
    this.state = 'draining'; clearTimeout(this.initializeTimer);
    // Stay within the host's 1.4 s coordinated-signal cap, including writer flush.
    const deadline = setTimeout(() => process.exit(1), 1300);
    const jobs = [...this.active.values()];
    for (const job of jobs) {
      clearTimeout(job.cancelTimer); job.controller.abort(new CancelledError());
      if (!job.started) void this.finish(job, undefined, -32800);
    }
    this.pending = [];
    const drain = async () => {
      while (this.active.size) await delay(5);
      await this.resources.shutdown();
      if (this.shutdown) await this.shutdown({reason});
    };
    let clean = false;
    try { clean = await Promise.race([drain().then(() => true), delay(1000).then(() => false)]); }
    catch { this.log('shutdown handler failed'); }
    if (!clean) this.log('bounded shutdown drain expired');
    if (id !== undefined) await this.reply(id, {});
    while (this.writing || this.outbound.length) await delay(5);
    this.state = 'stopped'; clearTimeout(deadline);
    process.exit(clean ? 0 : 1);
  }
}
