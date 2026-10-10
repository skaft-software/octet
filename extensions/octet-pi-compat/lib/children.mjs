// Pi's child facade never imports an agent runtime or obtains provider credentials.
// Native binding glue (all required, not a feature-name-only compatibility claim):
//   Runtime constructor: installChildRuntime(this).
//   shims/coding-agent.mjs: re-export ../shims/child-sdk.mjs's named exports.
//   Negotiate agent_sessions + agent_session_events_v1 + agent_session_lifetime_v1.
//   Retain agent/{spawn,events,message,follow_up,interrupt,stop} with the ORIGINAL
//   host-issued resource_owner; never rebind a captured session to a current owner.
//   Runtime retirement: retireChildSessions(this, state); native retirement must
//   stop/settle that principal's children. No callback/tool proxy is claimed here.
import { AsyncLocalStorage } from 'node:async_hooks';
import { randomUUID } from 'node:crypto';
import { bounded, fields, invalid, ownerKey, rpcError, strict, unsupported } from './errors.mjs';
import { ChildEventProjection } from './child-events.mjs';

const localHost = new AsyncLocalStorage();
let installedRuntime;
const sessions = new WeakMap();
const TOOL = Symbol('octet host standard tool');
const MANAGER = Symbol('octet child session selector');
const SECRET = Symbol('host child constructor');
const STANDARD = ['read', 'edit', 'write', 'bash'];
export const CHILD_FEATURES = Object.freeze(['agent_sessions', 'agent_session_events_v1', 'agent_session_lifetime_v1']);
export const CHILD_METHODS = Object.freeze(['agent/spawn', 'agent/events', 'agent/message', 'agent/follow_up', 'agent/interrupt', 'agent/stop']);
export const DEFAULT_CHILD_LIMITS = Object.freeze({ max_depth: 1, max_concurrent_children: 8, max_turns: 32, max_tokens: 64000, max_output_bytes: 16384, timeout_ms: 600000 });

export function installChildRuntime(runtime) {
  if (installedRuntime && installedRuntime !== runtime) invalid('only one Pi compatibility domain may own child SDK routing');
  installedRuntime = runtime;
  return () => { if (installedRuntime === runtime) installedRuntime = undefined; };
}

// Dependency injection for an admitted host/CLI driver and offline fixtures. The
// binding must already contain authenticated native authority, not caller JSON.
export function withChildHost(binding, callback) { return localHost.run(binding, callback); }
export function captureChildHost() {
  const injected = localHost.getStore();
  if (injected) return injected;
  const runtime = installedRuntime;
  if (!runtime) unsupported('createAgentSession', 'Rust owns agent sessions; a live authenticated host child binding is required (no Pi runtime fallback)');
  const store = runtime.scope.getStore();
  runtime.assertOwner(store);
  for (const feature of CHILD_FEATURES) runtime.require(feature);
  ownerKey(store.state.owner);
  return {
    identity: store.state, owner: store.state.owner, cwd: store.state.workspace,
    model: store.state.host.model_view, features: runtime.features,
    assertLive: () => { runtime.assertOwner(store); store.controller.signal.throwIfAborted(); },
    request: (method, params) => runtime.hostCall(method, params, store),
    track: promise => runtime.track(promise, store),
  };
}
function live(binding) {
  if (typeof binding?.request !== 'function' || typeof binding.assertLive !== 'function') invalid('authenticated child host binding required');
  ownerKey(binding.owner); binding.assertLive();
  for (const feature of CHILD_FEATURES) if (!binding.features?.has(feature)) unsupported(feature, 'host child contract was not negotiated');
}
function tool(name) { return Object.freeze({ name, [TOOL]: true, execute() { unsupported(`direct ${name}.execute`, 'only the native child agent may execute host tools'); } }); }
export const readTool = tool('read'), bashTool = tool('bash'), editTool = tool('edit'), writeTool = tool('write');
export const codingTools = Object.freeze([readTool, bashTool, editTool, writeTool]);
function toolsAt(cwd, tools) {
  if (cwd !== undefined && cwd !== captureChildHost().cwd) unsupported('child tools cwd', 'child cwd must inherit the authenticated host workspace');
  return tools;
}
export const createCodingTools = cwd => [...toolsAt(cwd, codingTools)];
export const createReadTool = cwd => toolsAt(cwd, readTool);
export const createBashTool = cwd => toolsAt(cwd, bashTool);
export const createEditTool = cwd => toolsAt(cwd, editTool);
export const createWriteTool = cwd => toolsAt(cwd, writeTool);

// This selector is a Pi-facing view, not a second session store. Native durable
// persistence is mandatory even for an in-memory Pi view; no Pi JSONL is written.
export class SessionManager {
  constructor(secret, cwd) { if (secret !== SECRET) unsupported('SessionManager constructor', 'use SessionManager.inMemory'); this[MANAGER] = true; this.cwd = cwd; }
  static inMemory(cwd) { return new SessionManager(SECRET, cwd); }
  static create() { unsupported('SessionManager.create', 'Pi session-file projection is not implemented; use the host-backed inMemory view'); }
  static open() { unsupported('SessionManager.open', 'Pi JSONL is not the native session format; implicit import/resume is forbidden'); }
  getSessionFile() { return undefined; }
  getSessionId() { if (!this.session?._sessionId) unsupported('SessionManager.getSessionId', 'native session identity has not been supplied yet'); return this.session._sessionId; }
  getCwd() { return this.cwd ?? this.session?._binding.cwd; }
  buildSessionContext() { return { messages: this.session?.messages ?? [], thinkingLevel: this.session?.thinkingLevel }; }
  getEntries() { unsupported('SessionManager.getEntries', 'child tree/entry projection is not implemented'); }
  appendMessage() { unsupported('SessionManager.appendMessage', 'native child persistence cannot be edited through a local JS mirror'); }
}

function policyFor(options) {
  const requested = options.tools ?? codingTools;
  if (!Array.isArray(requested) || requested.length === 0 || requested.length > STANDARD.length) invalid('child tools must be a nonempty standard-tool allowlist');
  let tools = requested.map(t => { if (!t?.[TOOL] || !STANDARD.includes(t.name)) unsupported('custom child tool', 'registered JS child callbacks have no host-mediated execution binding yet'); return t.name; });
  if (new Set(tools).size !== tools.length) invalid('duplicate child tool');
  if (options.excludeTools !== undefined) {
    if (!Array.isArray(options.excludeTools) || options.excludeTools.some(n => typeof n !== 'string')) invalid('excludeTools');
    tools = tools.filter(name => !options.excludeTools.includes(name));
  }
  if (!tools.length) unsupported('tool-free child', 'the current native child policy requires a nonempty scope');
  const policy = { ...DEFAULT_CHILD_LIMITS, tools };
  // Bounds are fixed in this preview. Unchanged Pi callers are not allowed to
  // smuggle native policy fields/authority through unvalidated SDK options.
  if (options.model || options.thinkingLevel) {
    if (options.model && (typeof options.model.id !== 'string' || typeof options.model.provider !== 'string')) invalid('child model must have provider and id');
    policy.model_selection = { provider: options.model?.provider ?? 'inherit', model: options.model?.id ?? 'inherit', reasoning: options.thinkingLevel ?? 'inherit' };
  }
  return policy;
}

export async function createAgentSession(options = {}) {
  fields(options, ['cwd', 'agentDir', 'sessionManager', 'settingsManager', 'modelRegistry', 'modelRuntime', 'model', 'tools', 'customTools', 'resourceLoader', 'excludeTools', 'thinkingLevel'], 'createAgentSession');
  const binding = captureChildHost(); live(binding);
  if (options.cwd !== undefined && options.cwd !== binding.cwd) unsupported('child cwd override', 'host workspace and effect policy must be inherited');
  if (options.customTools?.length) unsupported('customTools', 'native child tool callbacks/nesting are not yet bound; nothing was spawned');
  if (options.customTools !== undefined && !Array.isArray(options.customTools)) invalid('customTools');
  if (options.modelRuntime !== undefined) unsupported('modelRuntime', 'custom JS runtimes cannot replace the host-owned provider');
  if (options.settingsManager !== undefined) unsupported('settingsManager', 'child settings must currently inherit native host settings');
  if (options.resourceLoader !== undefined) unsupported('resourceLoader', 'child resource/system-prompt overrides require a native admission binding');
  if (options.agentDir !== undefined) unsupported('agentDir', 'Pi agent-directory discovery is not performed by native children');
  if (options.modelRegistry !== undefined) unsupported('modelRegistry', 'a caller-provided registry cannot change the native provider catalog');
  const manager = options.sessionManager ?? SessionManager.inMemory(binding.cwd);
  if (!manager?.[MANAGER] || manager.session) invalid('fresh host-backed SessionManager required');
  if (manager.cwd !== undefined && manager.cwd !== binding.cwd) unsupported('SessionManager cwd', 'host workspace must be inherited');
  const policy = policyFor(options);
  if (policy.model_selection && !binding.features.has('agent_model_selection_v1')) unsupported('child model selection', 'agent_model_selection_v1 was not negotiated');
  const session = new AgentSession(SECRET, binding, manager, policy, options.model, options.thinkingLevel);
  let owned = sessions.get(binding.identity ?? binding); if (!owned) { owned = new Set(); sessions.set(binding.identity ?? binding, owned); }
  if (owned.size >= 32) throw rpcError(-32012, 'bounds_exceeded child SDK handles');
  manager.session = session;
  owned.add(session); session._owned = owned;
  return { session, extensionsResult: { extensions: [], errors: [] } };
}

export class AgentSession {
  constructor(secret, binding, manager, policy, model, thinkingLevel) {
    if (secret !== SECRET) unsupported('AgentSession constructor', 'use createAgentSession');
    this._binding = binding; this.sessionManager = manager; this._policy = policy; this.model = model;
    this.thinkingLevel = thinkingLevel; this._agentId = undefined; this._sessionReference = undefined;
    this._disposed = false; this._busy = false; this._sequence = 0; this._listeners = new Set(); this._name = undefined;
    this._projection = new ChildEventProjection(event => { for (const listener of [...this._listeners]) { const result = listener(event); if (result?.then) this._binding.track?.(result); } });
    const self = this;
    this.agent = strict({
      get state() { return { messages: self.messages, model: self.model, thinkingLevel: self.thinkingLevel, isStreaming: self.isStreaming }; },
      get beforeToolCall() { return undefined; },
      set beforeToolCall(_) { unsupported('session.agent.beforeToolCall', 'host effect admission cannot be replaced by an unbound local hook'); },
      abort: () => self.abort(),
    }, 'session.agent');
  }
  get messages() { return this._projection.messages.map(message => structuredClone(message)); }
  get isStreaming() { return this._busy; }
  get sessionId() { return this.sessionManager.getSessionId(); }
  get sessionFile() { return undefined; }
  get sessionName() { return this._name; }
  getActiveToolNames() { return [...this._policy.tools]; }
  getAllTools() { return this._policy.tools.map(name => ({ name })); }
  setActiveTools() { unsupported('session.setActiveTools', 'child scope is pinned by the host at spawn'); }
  setSessionName(name) {
    bounded(name, 'child session name', 48);
    if (this._agentId) unsupported('session.setSessionName after spawn', 'native child renaming is not bound');
    if (!/^[a-z][a-z0-9_-]{0,47}$/.test(name)) unsupported('session name', 'native task labels require lowercase ASCII letters, digits, underscore or hyphen');
    this._name = name;
  }
  async bindExtensions() { unsupported('session.bindExtensions', 'child extension callbacks and custom tools are not yet bound'); }
  subscribe(listener) {
    this._assert(); if (typeof listener !== 'function') invalid('child event listener');
    if (this._listeners.size >= 64) throw rpcError(-32012, 'bounds_exceeded child listeners');
    this._listeners.add(listener); return () => this._listeners.delete(listener);
  }
  _assert() { if (this._disposed) throw rpcError(-32002, 'child session disposed'); live(this._binding); }
  async _request(method, params) { this._assert(); const result = await this._binding.request(method, params); this._assert(); return result; }
  _stop() { return this._stopPromise ??= this._request('agent/stop', { target: this._agentId }); }
  async prompt(text, options = {}) {
    fields(options, ['streamingBehavior'], 'session.prompt');
    bounded(text, 'child prompt', 131072, { controls: true });
    if (!text.trim()) invalid('child prompt cannot be empty');
    this._assert();
    if (this._disposeRequested) throw rpcError(-32002, 'child session is disposing');
    if (this._busy) {
      if (options.streamingBehavior === 'steer') return this.steer(text);
      if (options.streamingBehavior === 'followUp') return this.followUp(text);
      invalid('child prompt is already running; choose steer or followUp explicitly');
    }
    if (options.streamingBehavior !== undefined) unsupported('streamingBehavior while idle');
    this._busy = true;
    try {
      if (!this._agentId) {
        // An ambiguous failure is not retried automatically. An operator may
        // inspect the native owner tree using this stable idempotency key.
        this._spawnKey ??= randomUUID();
        this._spawnPromise = this._request('agent/spawn', { task_name: this._name ?? `pi-${this._spawnKey.slice(0, 12)}`, message: text, idempotency_key: this._spawnKey, policy: this._policy });
        this._admissionPromise = this._spawnPromise;
        const result = await this._spawnPromise;
        if (typeof result?.agent_id !== 'string' || !result.agent_id) invalid('native child spawn omitted agent_id');
        this._agentId = result.agent_id;
        const effective = result.policy;
        if (!effective || !Array.isArray(effective.tools) || !effective.tools.length || effective.tools.some(name => !this._policy.tools.includes(name))) invalid('native child omitted an effective tool policy');
        for (const key of ['max_turns', 'max_tokens', 'timeout_ms', 'max_output_bytes', 'max_depth', 'max_concurrent_children']) {
          if (!Number.isSafeInteger(effective[key]) || effective[key] <= 0 || effective[key] > this._policy[key]) invalid(`native child effective ${key} exceeds requested finite limit`);
        }
        this._policy = { ...effective, tools: [...effective.tools] };
        this._sessionReference = result.session;
        const selected = result.resolved_model;
        if (selected?.model && selected?.provider) {
          this.model = { ...(this.model ?? {}), id: selected.model, provider: selected.provider };
          this._projection.model = selected.model; this._projection.provider = selected.provider;
        }
        if (this._disposeRequested) { await this._disposePromise; throw rpcError(-32800, 'child session disposed while spawning'); }
      } else {
        this._admissionPromise = this._request('agent/follow_up', { target: this._agentId, message: text });
        await this._admissionPromise;
      }
      await this._observe();
    } catch (error) {
      // If observation cannot be lossless, do not leave a hidden autonomous
      // worker running. Preserve the original failure and expose stop failure.
      if (this._agentId && !this._disposed) {
        try { await this._stop(); }
        catch (stopError) { throw new AggregateError([error, stopError], 'child operation and host cancellation both failed'); }
      }
      throw error;
    } finally { this._busy = false; }
  }
  async _observe() {
    for (;;) {
      const result = await this._request('agent/events', { target: this._agentId, after_sequence: this._sequence, timeout_ms: 25000 });
      if (result?.agent_id !== this._agentId || !Array.isArray(result.events) || result.events.length > 256 || typeof result.has_more !== 'boolean') invalid('invalid native child event batch');
      if (result.session_id != null) {
        bounded(result.session_id, 'native child session id', 512);
        if (this._sessionId && this._sessionId !== result.session_id) throw rpcError(-32002, 'native child session identity changed');
        this._sessionId = result.session_id;
      }
      for (const entry of result.events) {
        if (entry.sequence !== this._sequence + 1) throw rpcError(-32002, 'child event sequence gap/replay; refusing partial observations');
        this._projection.accept(entry.event); this._sequence = entry.sequence;
      }
      if (result.next_sequence !== this._sequence) invalid('native child event cursor mismatch');
      if (result.has_more) continue;
      const state = result.status?.state;
      if (['pending', 'running'].includes(state)) continue;
      if (state === 'completed') { this._projection.settled(); return; }
      if (['interrupted', 'shutdown'].includes(state)) { this._projection.settled(); return; }
      if (['failed', 'timed_out', 'limit_reached', 'awaiting_approval', 'detached'].includes(state)) {
        throw rpcError(-32002, `native child ${state}${result.status.error ? `: ${result.status.error}` : ''}`);
      }
      invalid(`unexpected native child state ${state}`);
    }
  }
  async steer(text) { bounded(text, 'child steering', 131072, { controls: true }); if (!this._agentId) invalid('child has not started'); return this._request('agent/message', { target: this._agentId, message: text }); }
  async followUp(text) { bounded(text, 'child follow-up', 131072, { controls: true }); if (!this._busy) return this.prompt(text); return this._request('agent/follow_up', { target: this._agentId, message: text }); }
  async abort() {
    this._assert();
    if (this._busy && this._admissionPromise) await this._admissionPromise;
    if (this._agentId) return this._request('agent/interrupt', { target: this._agentId });
  }
  dispose() {
    if (this._disposed) return Promise.resolve();
    if (this._disposePromise) return this._disposePromise;
    this._disposeRequested = true;
    this._disposePromise = (async () => {
      // An ambiguous admission is not a successful disposal and is not replayed.
      if (this._admissionPromise) await this._admissionPromise;
      // A rejected stop is a real failure, never successful local-only disposal.
      if (this._agentId) await this._stop();
      this._retire();
    })();
    return this._disposePromise;
  }
  _retire() { this._disposed = true; this._listeners.clear(); this._owned?.delete(this); }
}

export function retireChildSessions(runtime, state) {
  // Native process/session retirement owns cancellation and usage settlement.
  // This only revokes local objects; it never sends a stale-owner request.
  for (const session of sessions.get(state) ?? []) session._retire();
  if (runtime === installedRuntime && !state) for (const s of runtime.states?.values() ?? []) retireChildSessions(runtime, s);
}
