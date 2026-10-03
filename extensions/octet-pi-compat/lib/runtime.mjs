import { AsyncLocalStorage } from 'node:async_hooks';
import { createJiti } from 'jiti';
import { readFileSync, realpathSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { bounded, fields, invalid, ownerKey, plainJSON, rpcError, strict, unsupported } from './errors.mjs';
import { createAPI, createContext, hookEvents, notificationEvents, textOnly } from './api.mjs';
import { RemoteUI } from './remote-ui.mjs';
import { Timers, deadline } from './timers.mjs';

const retainedMethods = new Set(['ui/open', 'ui/close', 'composer/get', 'composer/set', 'composer/insert', 'shortcut/register', 'session/append_entry', 'session/set_name', 'session/set_label', 'session/send_message', 'session/send_user_message', 'tools/set_active']);
const supportedFeatures = new Set(['request_cancellation', 'content_parts', 'request_progress', 'remote_ui', 'lifecycle_events', 'lifecycle_events_v2', 'editor_handoff', 'composer', 'shortcuts', 'session_entries', 'message_injection', 'active_tools']);
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
const cancelled = () => rpcError(-32800, 'request cancelled');

class SharedBus {
  constructor(runtime) { this.runtime = runtime; this.listeners = new Map(); }
  facade(factory) {
    const on = (topic, handler, once = false) => {
      bounded(topic, 'shared event topic', 256, { controls: true }); if (typeof handler !== 'function') invalid('shared event listener');
      const entries = this.listeners.get(topic) || [];
      if ([...this.listeners.values()].reduce((n, xs) => n + xs.length, 0) >= 1024) throw rpcError(-32012, 'bounds_exceeded shared listeners');
      const captured = this.runtime.scope.getStore();
      const entry = { handler, factory, captured: captured?.state ? captured : null, once };
      entries.push(entry); this.listeners.set(topic, entries);
      const off = () => { const i = entries.indexOf(entry); if (i >= 0) entries.splice(i, 1); };
      entry.off = off; return off;
    };
    return strict({
      on: (topic, handler) => on(topic, handler), once: (topic, handler) => on(topic, handler, true),
      off: (topic, handler) => { for (const entry of [...(this.listeners.get(topic) || [])]) if (entry.handler === handler && entry.factory === factory) entry.off(); },
      emit: (topic, data) => {
        const emitter = this.runtime.scope.getStore();
        for (const entry of [...(this.listeners.get(topic) || [])]) {
          if (entry.captured?.state && !entry.captured.state.alive) continue;
          if (entry.once) entry.off();
          const s = { ...(entry.captured || emitter), factory: entry.factory };
          const result = this.runtime.scope.run(s, () => entry.handler(data));
          // Objects/functions never serialize here. Cross-factory object identity
          // and synchronous listener ordering are the actual in-process Pi bus.
          if (result?.then) this.runtime.track(result, s);
        }
      },
    }, 'pi.events');
  }
  ownerEnded(state) { for (const entries of this.listeners.values()) for (const e of [...entries]) if (e.captured?.state === state) e.off(); }
}

export class Runtime {
  constructor(config, transport) {
    this.config = config; this.transport = transport;
    this.scope = new AsyncLocalStorage(); this.tools = new Map(); this.commands = new Map(); this.shortcuts = new Map(); this.flags = new Map(); this.events = new Map();
    this.bus = new SharedBus(this); this.ui = new RemoteUI(this); this.timers = new Timers(this);
    this.states = new Map(); this.active = new Map(); this.features = new Set(); this.flagValues = new Map();
    this.loaded = false; this.initialized = false; this.stopping = false; this.hookTail = Promise.resolve(); this.hookQueued = 0;
    this.maxConcurrent = 8; this.foreground = null;
  }
  mouseIntent(enabled) {
    this.require('remote_ui'); const store = this.scope.getStore(); this.assertOwner(store);
    if (store.surface?.opened && enabled !== Boolean(store.mouseCapture)) unsupported('changing live mouse capture', 'declare capture before opening the component');
    store.mouseCapture = enabled;
  }
  require(feature) { if (!this.features.has(feature)) unsupported(feature, 'feature was not negotiated'); }
  assertOwner(store) {
    if (!store?.state?.owner || !store.state.alive) throw rpcError(-32002, 'not_foreground_owner retained context is unavailable');
    if (this.foreground && store.state !== this.foreground) throw rpcError(-32002, 'not_foreground_owner context belongs to an inactive session');
  }
  current(factory) {
    const s = this.scope.getStore();
    this.assertOwner(s); return { ...s, factory };
  }
  hostCall(method, params, store = this.scope.getStore()) {
    if (this.stopping) throw rpcError(-32002, 'host is draining');
    if (!Number.isSafeInteger(store?.id) || store.id < 0) throw rpcError(-32002, 'active numeric parent_request_id required');
    store.controller.signal.throwIfAborted();
    const live = this.active.has(store.id) && store.live;
    if (!live && !retainedMethods.has(method)) throw rpcError(-32002, `not_foreground_owner ${method} requires a live request`);
    if (retainedMethods.has(method) && method !== 'shortcut/register') this.assertOwner(store);
    return this.transport.request(method, {
      parent_request_id: store.id, ...(store.state?.owner && retainedMethods.has(method) ? { resource_owner: store.state.owner } : {}), ...params,
    }, { parent: live ? store.id : undefined, signal: store.controller.signal });
  }
  track(promise, store = this.scope.getStore()) {
    const p = Promise.resolve(promise);
    // Observe even void setters: failures are returned at a live host boundary or
    // surfaced as bounded background diagnostics, never swallowed.
    p.catch(error => {
      if (store?.live && this.active.has(store.id)) store.errors.push(error);
      else this.backgroundError(error);
    });
    store?.pending?.add(p);
    p.then(() => store?.pending?.delete(p), () => store?.pending?.delete(p));
    return p;
  }
  async flush(store) {
    while (store.pending.size) await Promise.all([...store.pending]);
    if (store.errors.length) throw store.errors[0];
  }
  backgroundError(error) {
    console.error(`[pi-compat] ${String(error?.message || error).slice(0, 4096)}`);
    if (!this.stopping && !this.transport.closed) this.transport.notify('notification', { level: 'error', title: 'Pi compatibility error', message: String(error?.message || error).slice(0, 4096) }).catch(e => this.transport.fail(e));
  }
  async load() {
    if (this.loaded) return;
    const extensions = this.config.extensions;
    if (!Array.isArray(extensions) || extensions.length > 64 || extensions.some(p => typeof p !== 'string')) invalid('bridge extensions must be an explicit list');
    const aliases = {};
    for (const prefix of ['@earendil-works', '@mariozechner']) {
      for (const [pkg, shim] of [['pi-coding-agent', 'coding-agent'], ['pi-ai', 'ai'], ['pi-tui', 'tui']]) aliases[`${prefix}/${pkg}`] = fileURLToPath(new URL(`../shims/${shim}.mjs`, import.meta.url));
    }
    // These are different schema libraries, not interchangeable version aliases.
    aliases.typebox = fileURLToPath(new URL('../node_modules/typebox/build/index.mjs', import.meta.url));
    aliases['@sinclair/typebox'] = fileURLToPath(new URL('../node_modules/@sinclair/typebox/build/esm/index.mjs', import.meta.url));
    this.jiti = createJiti(import.meta.url, { alias: aliases, moduleCache: true, fsCache: false, tryNative: false });
    for (let factory = 0; factory < extensions.length; factory++) {
      const entry = realpathSync(extensions[factory]);
      if (this.config.entrypoint_sha256?.[entry]) {
        const actual = createHash('sha256').update(readFileSync(entry)).digest('hex');
        if (actual !== this.config.entrypoint_sha256[entry]) invalid(`reviewed factory changed: ${entry}; configure again`);
      }
      const fn = await this.jiti.import(entry, { default: true });
      if (typeof fn !== 'function') invalid(`default export must be a factory: ${entry}`);
      const s = { id: this.initializingId, controller: new AbortController(), factory, pending: new Set(), errors: [], live: false };
      await this.scope.run(s, () => fn(createAPI(this, factory)));
    }
    this.loaded = true;
    if (this.tools.size > 256 || this.commands.size > 256 || this.flags.size > 64 || this.shortcuts.size > 64) throw rpcError(-32012, 'bounds_exceeded registrations');
  }
  metadata() {
    const hooks = [...new Set([...this.events.keys()].filter(e => hookEvents[e]).map(e => hookEvents[e]))];
    if (hooks.includes('session_start') || hooks.includes('session_end')) for (const name of ['session_start', 'session_end']) if (!hooks.includes(name)) hooks.push(name);
    return {
      tools: [...this.tools].map(([name, { definition: d }]) => ({ name, description: d.description, parameters: JSON.parse(JSON.stringify(d.parameters)), ...(d.output_schema ? { output_schema: d.output_schema } : {}) })),
      commands: [...this.commands].map(([name, { definition: d }]) => ({ name, description: d.description || name, ...(d.usage ? { usage: d.usage } : {}) })),
      hooks: hooks.sort(), flags: [...this.flags].map(([name, { definition }]) => ({ name, ...definition })),
      shortcuts: [...this.shortcuts].map(([key, { definition: d }], i) => ({ id: `pi-shortcut:${i}`, key, description: d.description || key })),
      events: [...this.events.keys()].sort(),
      tool_renderers: [...this.tools].filter(([, t]) => t.definition.renderCall || t.definition.renderResult).map(([name]) => name),
    };
  }
  async initialize(params, store) {
    if (this.initialized) invalid('already initialized');
    if (params.api_version !== '0.4' || params.protocol?.version !== '0.4') throw rpcError(-32010, 'requires feature-negotiated API 0.4');
    const required = params.protocol.required_features || [];
    if (required.some(f => !supportedFeatures.has(f))) unsupported('initialize required feature');
    this.features = new Set([...required, ...(params.protocol.optional_features || [])].filter(f => supportedFeatures.has(f)));
    for (const f of ['request_cancellation', 'content_parts']) this.require(f);
    this.maxConcurrent = Math.min(8, params.protocol.limits?.max_concurrent_requests || 1);
    this.initialHost = params.host || {}; this.workspace = params.workspace; this.initializingId = store.id;
    for (const flag of params.flag_values || []) this.flagValues.set(flag.name, flag.value);
    await this.load();
    const metadata = this.metadata(), declared = params.contributes || {};
    for (const [kind, names] of [['tools', metadata.tools.map(t => t.name)], ['commands', metadata.commands.map(c => c.name)], ['hooks', metadata.hooks], ['tool_renderers', metadata.tool_renderers]]) {
      if (JSON.stringify([...(declared[kind] || [])].sort()) !== JSON.stringify([...names].sort())) invalid(`manifest ${kind} differs from reviewed registrations: ${names.join(', ')}`);
    }
    const staticMetadata = this.config.registrations;
    if (staticMetadata && JSON.stringify(staticMetadata) !== JSON.stringify(metadata)) invalid('reviewed registration metadata changed; configure again');
    for (const [name, { definition: d }] of this.flags) {
      const value = this.flagValues.get(name) ?? d.default;
      if (d.type === 'integer' ? !Number.isSafeInteger(value) : typeof value !== d.type) invalid(`flag ${name} type`);
    }
    if (metadata.shortcuts.length) {
      this.require('shortcuts');
      for (const shortcut of metadata.shortcuts) await this.hostCall('shortcut/register', shortcut, store);
    }
    this.initialized = true;
    return {
      api_version: '0.4', tools: metadata.tools, commands: metadata.commands,
      protocol: { version: '0.4', features: [...this.features], limits: { max_concurrent_requests: this.maxConcurrent },
        ...(this.features.has('lifecycle_events') ? { lifecycle_events: ['turn/started', 'turn/settled', 'tool/started', 'tool/settled'] } : {}) },
    };
  }
  bind(params, store) {
    const context = params.context || {};
    const owner = context.resource_owner || params.payload?.binding;
    if (!owner) return;
    const key = ownerKey(owner);
    let state = this.states.get(key);
    if (!state) {
      state = { key, owner: Object.freeze({ ...owner }), alive: true, workspace: context.workspace || this.workspace, host: { ...this.initialHost, ...(context.host || {}) }, statuses: new Map(), branchListeners: new Set(), uiQueues: new Map(), parent: store.id };
      this.states.set(key, state);
    } else {
      if (!state.alive) throw rpcError(-32002, 'not_foreground_owner settled owner');
      state.workspace = context.workspace || state.workspace;
      state.host = { ...state.host, ...(context.host || {}) }; state.parent = store.id;
    }
    if (this.foreground && this.foreground !== state) this.retire(this.foreground).catch(e => this.backgroundError(e));
    this.foreground = state; store.state = state;
  }
  async retire(state) {
    state.alive = false; this.timers.owner(state); this.bus.ownerEnded(state); state.branchListeners.clear();
    await this.ui.ownerEnded(state);
  }
  updated(params) {
    this.require('remote_ui'); const key = ownerKey(params.resource_owner), state = this.states.get(key);
    if (!state || !state.alive || state !== this.foreground) throw rpcError(-32002, 'not_foreground_owner context/updated');
    const previous = state.host.git_branch; state.host = { ...state.host, ...plainJSON(params.host, 'host snapshot', 524288) };
    if (previous !== state.host.git_branch) for (const listener of state.branchListeners) listener();
    for (const surface of this.ui.surfaces.values()) if (surface.store.state === state) this.scope.run(surface.store, () => { surface.tui.invalidate(); surface.requestRender(); });
  }
  async runEvent(event, value, store, { veto = false } = {}) {
    for (const entry of this.events.get(event) || []) {
      store.controller.signal.throwIfAborted();
      const childStore = { ...store, factory: entry.factory };
      const result = await this.scope.run(childStore, () => entry.handler(value, createContext(this, childStore)));
      store.controller.signal.throwIfAborted();
      if (result !== undefined) {
        if (veto && result && typeof result === 'object' && Object.keys(result).every(k => ['block', 'reason'].includes(k))) {
          if (result.block) return { action: 'deny', reason: result.reason || 'Blocked by Pi extension' };
        } else unsupported(`${event} result`, 'wire cannot apply this event transformation or veto');
      }
    }
    return { action: 'continue' };
  }
  queued(store, work) {
    if (++this.hookQueued > 128) { this.hookQueued--; throw rpcError(-32012, 'bounds_exceeded ordered hook queue'); }
    const result = this.hookTail.then(() => { store.controller.signal.throwIfAborted(); return work(); });
    this.hookTail = result.catch(() => {});
    return result.finally(() => { this.hookQueued--; });
  }
  async dispatch(message, store) {
    const p = message.params;
    if (message.method === 'initialize') return this.initialize(p, store);
    if (!this.initialized) invalid('not initialized');
    this.bind(p, store);
    if (message.method === 'command/execute') {
      const cmd = this.commands.get(p.name); if (!cmd) invalid(`unknown command ${p.name}`);
      const opened = deferred(); store.detach = () => { store.detached = true; opened.resolve({ text: '', notifications: [], context: [] }); };
      store.factory = cmd.factory;
      const run = this.scope.run(store, async () => {
        if (this.features.has('composer')) {
          store.state.host.composer_text = (await this.hostCall('composer/get', {}, store)).text;
        }
        const result = await cmd.definition.handler((p.arguments || []).join(' '), createContext(this, store));
        if (result !== undefined) unsupported('command result', 'Pi command handlers return void');
        await this.flush(store); return { text: '', notifications: [], context: [] };
      });
      run.catch(error => { if (store.detached && !store.controller.signal.aborted) this.backgroundError(error); });
      return Promise.race([run, opened.promise]);
    }
    if (message.method === 'tool/call') {
      const tool = this.tools.get(p.name); if (!tool) invalid(`unknown tool ${p.name}`);
      store.factory = tool.factory; let sequence = 0;
      const update = this.features.has('request_progress') ? result => {
        fields(result, ['content', 'details'], 'tool update'); store.controller.signal.throwIfAborted();
        if (result.details !== undefined) unsupported('tool update details', 'host progress has no details payload');
        return this.track(this.transport.notify('$/progress', { request_id: store.id, sequence: ++sequence,
          event: { type: 'status', message: textOnly(result.content).map(p => p.text).join('\n') } }), store);
      } : undefined;
      const result = await this.scope.run(store, () => tool.definition.execute(String(store.id), p.arguments, store.controller.signal, update, createContext(this, store)));
      fields(result, ['content', 'details', 'isError', 'structured_content'], 'tool result');
      if (result.isError !== undefined && typeof result.isError !== 'boolean') invalid('tool isError must be boolean');
      if (Boolean(tool.definition.output_schema) !== (result.structured_content !== undefined)) invalid('structured_content must match the tool output_schema contract');
      await this.flush(store);
      return { content: textOnly(result.content), is_error: result.isError ?? false,
        ...(result.details === undefined ? {} : { metadata: { pi_details: plainJSON(result.details, 'tool details') } }),
        ...(result.structured_content === undefined ? {} : { structured_content: plainJSON(result.structured_content, 'structured content', 262144) }),
      };
    }
    if (message.method === 'hook/run') return this.queued(store, async () => {
      const events = Object.keys(hookEvents).filter(e => hookEvents[e] === p.hook);
      if (!this.metadata().hooks.includes(p.hook)) invalid(`unknown hook ${p.hook}`);
      let disposition = { action: 'continue' };
      const payload = p.payload || {};
      for (const event of events) {
        const value = p.hook === 'before_tool_call' ? { type: event, toolName: payload.name, input: payload.arguments }
          : p.hook === 'after_tool_call' ? { type: event, toolName: payload.name, input: payload.arguments, content: [{ type: 'text', text: payload.output }], isError: payload.is_error }
          : strict({ type: event, ...payload }, `${event} event`);
        disposition = await this.runEvent(event, value, store, { veto: p.hook === 'before_tool_call' });
        if (disposition.action === 'deny') break;
      }
      await this.flush(store);
      if (p.hook === 'session_end' && store.state) await this.retire(store.state);
      return { disposition, context: [], notifications: [] };
    });
    if (message.method === 'tool/render') {
      const tool = this.tools.get(p.name); if (!tool) invalid('unknown renderer');
      const components = [];
      if (tool.definition.renderCall) components.push(tool.definition.renderCall(p.arguments, (await import('./theme.mjs')).theme));
      if (tool.definition.renderResult) components.push(tool.definition.renderResult({ content: [{ type: 'text', text: p.output }], details: p.details }, { expanded: false, isPartial: false, isError: p.is_error }, (await import('./theme.mjs')).theme));
      const { safeLines } = await import('./remote-ui.mjs');
      const text = components.flatMap(c => safeLines(c.render(80))).join('\n').replace(/\x1b\[[0-9;]*m/g, '');
      // This semantic wire intentionally has no ANSI. It cannot drive the TUI
      // transcript renderer; the host keeps it as provenance, never fake chrome.
      return { segments: [{ text, style_role: null }] };
    }
    throw rpcError(-32601, `unknown method ${message.method}`);
  }
  async notify(message) {
    const p = message.params;
    if (message.method === 'ui/editor-state') {
      this.require('editor_handoff'); const state = this.foreground;
      // The host can publish its first composer snapshot before any request
      // has issued an owner. There is no retained context to update yet; the
      // first command/editor mount reads the current composer from the host.
      if (!state?.alive) return;
      if (state.editorRevision !== undefined && p.revision <= state.editorRevision) return;
      state.editorRevision = p.revision;
      const text = bounded(p.text, 'editor text', 262144);
      const editors = [...this.ui.surfaces.values()].filter(surface => surface.placement === 'editor' && surface.store.state === state);
      // An echo acknowledges an earlier host checkpoint, not input that is still
      // in flight. Replacing a newer local draft here drops characters and
      // causes setText/onChange to send an echo back to the host.
      if (editors.some(surface => surface.editor?.pending)) return;
      state.host.composer_text = text;
      for (const surface of editors) {
        const editor = surface.editor;
        if (editor) editor.applyingHost = true;
        try {
          if (surface.component?.getText?.() !== text) surface.component?.setText?.(text);
        } finally { if (editor) editor.applyingHost = false; }
        surface.requestRender();
      }
      return;
    }
    if (message.method.startsWith('ui/')) { this.ui.handle(message.method, p); return; }
    if (message.method === 'context/updated') { this.updated(p); return; }
    let event = notificationEvents[message.method];
    let handler;
    if (message.method === 'shortcut/trigger') {
      this.require('shortcuts'); const meta = this.metadata().shortcuts.find(s => s.id === p.id);
      if (!meta) invalid('unknown shortcut trigger'); handler = this.shortcuts.get(meta.key);
    } else if (!event) throw rpcError(-32601, `unsupported_feature notification ${message.method}`);
    if (event) this.require(['turn/started', 'turn/settled', 'tool/started', 'tool/settled'].includes(message.method) ? 'lifecycle_events' : 'lifecycle_events_v2');
    const state = p.resource_owner ? this.states.get(ownerKey(p.resource_owner)) : this.foreground;
    if (!state?.alive) throw rpcError(-32002, 'not_foreground_owner lifecycle notification');
    const store = { id: state.parent, state, controller: new AbortController(), pending: new Set(), errors: [], live: false, method: message.method };
    if (p.host) state.host = { ...state.host, ...p.host };
    if (message.method === 'model/selected' && p.model) state.host.model = p.model;
    if (message.method === 'reasoning/selected' && p.reasoning) state.host.reasoning = p.reasoning;
    return this.queued(store, async () => {
      if (handler) {
        store.factory = handler.factory;
        await this.scope.run(store, () => handler.definition.handler(createContext(this, store)));
      } else await this.runEvent(event, strict({ type: event, ...p }, `${event} event`), store);
      await this.flush(store);
    });
  }
  cancel(id) {
    this.active.get(id)?.controller.abort(cancelled()); this.transport.cancel(id);
    this.ui.cancelParent(id).catch(e => this.backgroundError(e));
  }
  async receive(message) {
    if (message.method === '$/cancelRequest') { this.cancel(message.params.id); return; }
    if (message.method === 'shutdown') { await this.shutdown(message.id); return; }
    if (this.stopping) return;
    if (message.id === undefined) {
      try { await this.notify(message); } catch (error) { this.backgroundError(error); }
      return;
    }
    if (this.active.has(message.id)) throw rpcError(-32600, 'duplicate active request id');
    if (this.active.size >= this.maxConcurrent) {
      await this.transport.send({ jsonrpc: '2.0', id: message.id, error: { code: -32012, message: 'bounds_exceeded concurrent requests' } }); return;
    }
    const store = { id: message.id, method: message.method, controller: new AbortController(), pending: new Set(), errors: [], live: true };
    this.active.set(message.id, store);
    try {
      const result = await this.scope.run(store, () => this.dispatch(message, store));
      store.controller.signal.throwIfAborted();
      if (!this.stopping) await this.transport.send({ jsonrpc: '2.0', id: message.id, result });
    } catch (error) {
      if (!this.stopping) await this.transport.send({ jsonrpc: '2.0', id: message.id, error: {
        code: store.controller.signal.aborted ? -32800 : (Number.isInteger(error.code) ? error.code : -32603),
        message: store.controller.signal.aborted ? 'request cancelled' : String(error?.message || error).slice(0, 4096),
      } });
    } finally { store.live = false; this.active.delete(message.id); this.transport.settleParent(message.id); }
  }
  async shutdown(id) {
    if (this.stopping) return;
    this.stopping = true;
    for (const store of this.active.values()) store.controller.abort(cancelled());
    this.timers.all(); await this.ui.shutdown();
    for (const state of this.states.values()) state.alive = false;
    try { await deadline(this.transport.send({ jsonrpc: '2.0', id, result: {} }), 750); await deadline(this.transport.idle(), 750); }
    finally { process.exit(0); }
  }
  lost(error, eof) {
    this.stopping = true; this.timers.all();
    for (const store of this.active.values()) store.controller.abort(cancelled());
    this.ui.shutdown().finally(() => { if (!eof) console.error(`[pi-compat transport] ${error.message}`); process.exit(eof ? 0 : 1); });
  }
}
