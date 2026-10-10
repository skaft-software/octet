import { AsyncLocalStorage } from 'node:async_hooks';
import { SessionTransport, SESSION_FEATURES } from './session-transport.mjs';
import { withContextLimits } from './context-limits.mjs';
import { createJiti } from 'jiti';
import { readFileSync, realpathSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { dirname, extname, isAbsolute, join, resolve } from 'node:path';
import { bounded, facade, fields, invalid, ownerKey, plainJSON, rpcError, strict, unsupported } from './errors.mjs';
import { createAPI, createContext, extensionDisplayName, followingEvents, hookEvents, notificationEvents, textOnly } from './api.mjs';
import { commandCompletions, ensureCompletionChain, retireAutocomplete, startCompletionChain } from './completions.mjs';
import { BUILTIN_TOOL_NAMES, toolWire, executeRegisteredTool, prepareRegisteredArguments, createToolContext, toolContent } from './tools.mjs';
import { discoverResources } from './resources.mjs';
import { cancellable, projectContext } from './provider-context.mjs';
import { checkpointFactoryProviders, validateProviderInitialization, startProviderRegistration, prepareProviderStream, startProviderStream, cancelProviderStream } from './providers.mjs';
import { transformInput } from './context-api.mjs';
import { forgetMcpRegistrations, hasMcpRegistrations, startMcpSession, retireMcpSession } from './mcp.mjs';
import { providerPipeline, pipelineHooks } from './provider-pipeline.mjs';
import { sessionOperation, sessionOperationHooks, sessionReplacement, sessionReplacementHooks } from './session-operations.mjs';
import { modelTurn, modelTurnHooks } from './model-turns.mjs';
import { runMessageMethods, handleRunMessage, rememberPrompt } from './run-messages.mjs';
import { compactionCallbackStore, settleCompactions, retireCompactions, cancelCompactions } from './compaction.mjs';
import { beforeAgentStart } from './before-agent-start.mjs';
import { installChildRuntime, retireChildSessions, CHILD_FEATURES, CHILD_METHODS } from './children.mjs';
import { appendReply, entryPayload, leafGrant } from './session-leaf.mjs';
import { RemoteUI } from './remote-ui.mjs';
import { TranscriptRenderers } from './transcript-renderers.mjs';
import { Timers, deadline } from './timers.mjs';
import { activateInstalledPi, classifyLoadFailure, fallbackData, installedPiAliases, piRuntimeMode, scanPiImportGaps } from './installed-pi.mjs';
import { emulatedPiAliases, typeboxAliases } from './pi-modules.mjs';
import { discoverPiSetup, resolveThemeFile, SOURCE_EXTENSIONS } from './pi-setup.mjs';
import { readPiTheme } from './theme-palette.mjs';
import { configureAgentDir } from './public-helpers.mjs';
import { ExtensionIssues } from './issues.mjs';
import { installHostKeybindings } from './keybindings.mjs';
import { TerminalInputListeners } from './terminal-input.mjs';
import { bindHostTheme, configureBridgeTheme, nativeTheme } from './theme.mjs';
import { tracePhase } from './startup-trace.mjs';
import { jitiCacheOptions } from './startup-cache.mjs';

const retainedMethods = new Set([...CHILD_METHODS, 'ui/open', 'ui/close', 'composer/get', 'composer/set', 'composer/insert', 'composer/history', 'shortcut/register', 'session/append_entry', 'session/set_name', 'session/set_label', 'session/send_message', 'session/send_user_message', 'session/compact', 'mcp/replace', 'tools/set_active', 'model/select', 'process/exec', 'provider/credentials']);
const supportedFeatures = new Set([...SESSION_FEATURES, ...CHILD_FEATURES, 'terminal_input_intercept_v1', 'request_cancellation', 'content_parts', 'request_progress', 'dynamic_tools', 'dynamic_tool_renderers', 'builtin_tool_overrides_v1', 'runtime_commands', 'artifacts', 'remote_ui', 'transcript_render_v1', 'lifecycle_events', 'lifecycle_events_v2', 'editor_handoff', 'composer', 'shortcuts', 'session_entries', 'message_injection', 'active_tools', 'autocomplete', 'autocomplete_edit_v1', 'tool_prompt_metadata_v1', 'resource_paths_v1', 'session_control_v1', 'session_compaction_v1', 'pipeline_hooks_v1', 'before_prompt_state_v1', 'input_transform_v1', 'process_exec_v1', 'mcp_registration_v1', 'tool_composition_v1', 'provider_proxy_v1', 'notification_source_v1', 'provider_credentials']);
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
const cancelled = () => rpcError(-32800, 'request cancelled');

// Review the bounded relative-import source closure without executing it. This
// is a change detector for reviewed code, not a sandbox or computed-import gate.
export function reviewedSourceHashes(entry) {
  const hashes = {}, queue = [realpathSync(entry)];
  while (queue.length) {
    const file = queue.shift();
    if (Object.hasOwn(hashes, file)) continue;
    if (Object.keys(hashes).length >= 64 || statSync(file).size > 1048576) invalid('reviewed factory source bounds; configure entrypoints explicitly');
    const bytes = readFileSync(file), source = bytes.toString('utf8');
    hashes[file] = createHash('sha256').update(bytes).digest('hex');
    const imports = source.matchAll(/\b(?:from\s*|import\s*\(?\s*|require\s*\(\s*)['"](\.{1,2}\/[^'"\n]+)['"]/g);
    for (const [, specifier] of imports) {
      const base = resolve(dirname(file), specifier), stem = base.slice(0, base.length - extname(base).length);
      for (const candidate of [base, ...[...SOURCE_EXTENSIONS].map(extension => base + extension), ...[...SOURCE_EXTENSIONS].map(extension => stem + extension), ...[...SOURCE_EXTENSIONS].map(extension => join(base, `index${extension}`))]) {
        let regular = false;
        try { regular = statSync(candidate).isFile(); } catch { continue; }
        if (regular) { queue.push(realpathSync(candidate)); break; }
      }
    }
  }
  return hashes;
}

class SharedBus {
  constructor(runtime) { this.runtime = runtime; this.listeners = new Map(); }
  facade(factory) {
    const on = (topic, handler, once = false) => {
      this.runtime.assertFactory(factory);
      bounded(topic, 'shared event topic', 256, { controls: true }); if (typeof handler !== 'function') invalid('shared event listener');
      const entries = this.listeners.get(topic) || [];
      if ([...this.listeners.values()].reduce((n, xs) => n + xs.length, 0) >= 1024) throw rpcError(-32012, 'bounds_exceeded shared listeners');
      const captured = this.runtime.scope.getStore();
      const entry = { handler, factory, captured: captured?.state ? captured : null, once };
      entries.push(entry); this.listeners.set(topic, entries);
      const off = () => { const i = entries.indexOf(entry); if (i >= 0) entries.splice(i, 1); };
      entry.off = off; return off;
    };
    return facade({
      on: (topic, handler) => on(topic, handler), once: (topic, handler) => on(topic, handler, true),
      // Pi's bus clears every listener process-wide; this bus is shared by all
      // reviewed factories, so one factory cannot retire another's listeners.
      clear() { unsupported('pi.events.clear', 'the shared bus is retired per owner, never cleared process-wide'); },
      off: (topic, handler) => { for (const entry of [...(this.listeners.get(topic) || [])]) if (entry.handler === handler && entry.factory === factory) entry.off(); },
      emit: (topic, data) => {
        this.runtime.assertFactory(factory);
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
    configureAgentDir(config.pi_agent_dir);
    this.scope = new AsyncLocalStorage(); this.tools = new Map(); this.commands = new Map(); this.shortcuts = new Map(); this.flags = new Map(); this.events = new Map();
    this.terminalInput = new TerminalInputListeners(this);
    this.bus = new SharedBus(this); this.ui = new RemoteUI(this); this.transcript = new TranscriptRenderers(this); this.timers = new Timers(this);
    this.issues = new ExtensionIssues(this); this.initializeEpilogue = Promise.resolve();
    this.states = new Map(); this.active = new Map(); this.features = new Set(); this.flagValues = new Map(); this.failedFactories = new Set(); this.extensionNames = new Map();
    this.loaded = false; this.initialized = false; this.stopping = false; this.hookTail = Promise.resolve(); this.hookQueued = 0;
    this.maxConcurrent = 8; this.foreground = null; this.foregroundWaiters = new Set(); this.autocompleteRegistration = null; this.autocompleteStarted = false; this.autocompleteSettle = null;
    this.entrypoints = [];
    this.uninstallChildren = installChildRuntime(this);
    bindHostTheme(this);
  }
  /// Mirror mode is an explicit reviewed opt-in recorded in bridge.json; it is
  /// never inferred from the presence of a Pi agent directory.
  get mirrorPiSetup() { return this.config.mirror_pi_setup === true; }
  get host() { return this.scope.getStore()?.state?.host ?? this.initialHost ?? {}; }
  diagnostic(message) {
    console.error(`[pi-compat] ${String(message).slice(0, 4096)}`);
  }
  entrypoint(factory) { return this.entrypoints[factory] ?? this.config.extensions?.[factory] ?? `<factory ${factory}>`; }
  mouseIntent(enabled) {
    this.require('remote_ui'); const store = this.scope.getStore(); this.assertOwner(store);
    const surface = store.surface;
    if (surface?.closed) {
      if (enabled) unsupported('changing live mouse capture', 'component is closed');
      return; // dispose may repeat the release intent; the host owns restoration.
    }
    if (surface?.phase === 'active' && enabled && !surface.desiredMouseCapture) {
      unsupported('changing live mouse capture', 'declare capture before component activation');
    }
    // Before activation, pending admissions reconcile this latest desired value;
    // they never mutate their frozen request or pretend the host lease changed.
    // A live disable remains teardown intent, not host-authorized reconfiguration.
    if (surface) surface.desiredMouseCapture = enabled;
    (surface?.store ?? store).mouseCapture = enabled;
  }
  require(feature) { if (!this.features.has(feature)) unsupported(feature, 'feature was not negotiated'); }
  assertSessionOwner(store) {
    if (!store?.state?.owner || !store.state.alive) throw rpcError(-32002, 'not_foreground_owner retained context is unavailable');
  }
  assertOwner(store) {
    this.assertSessionOwner(store);
    if (store.state !== this.foreground) throw rpcError(-32002, 'not_foreground_owner context belongs to an inactive session');
  }
  assertFactory(factory) {
    if (this.failedFactories.has(factory)) throw rpcError(-32002, 'failed extension factory is unavailable');
  }
  current(factory) {
    this.assertFactory(factory);
    const s = this.scope.getStore();
    this.assertOwner(s); return { ...s, factory };
  }
  hostCall(method, params, store = this.scope.getStore()) {
    store = compactionCallbackStore(this, store);
    if (this.stopping) throw rpcError(-32002, 'host is draining');
    if (!Number.isSafeInteger(store?.id) || store.id < 0) throw rpcError(-32002, 'active numeric parent_request_id required');
    store.controller.signal.throwIfAborted();
    const live = this.active.get(store.id)?.controller === store.controller && store.live;
    const retained = retainedMethods.has(method) && Boolean(store.state?.owner);
    if (!live && !retained) throw rpcError(-32002, `not_foreground_owner ${method} requires a live request`);
    if (retained || retainedMethods.has(method) && method !== 'shortcut/register') this.assertOwner(store);
    const surface = store.surface, checkpoint = method === 'composer/set' && params.editor_checkpoint;
    const independentCheckpoint = retained && surface?.store === store && surface.placement === 'editor'
      && surface.opened && surface.editor && !surface.editor.retired
      && checkpoint?.surface_id === surface.id && checkpoint.mount_id === surface.mountId;
    // Only an actual fenced editor checkpoint outlives a still-live origin's
    // normal settlement (including accepted writes draining a voluntary close).
    // Other calls keep their live parent. Wire origin/owner and abort signal stay.
    return this.transport.request(method, {
      parent_request_id: store.id, ...(retained ? { resource_owner: store.state.owner } : {}), ...params,
    }, { parent: live && !independentCheckpoint ? store.id : undefined, signal: store.controller.signal,
      timeout: method === 'process/exec' ? 2147483647 : undefined });
  }
  appendEntry(type, data, store = this.scope.getStore()) {
    this.require('session_entries');
    const clean = entryPayload(type, data);
    if (this.stopping) throw rpcError(-32002, 'host is draining');
    this.assertSessionOwner(store); store.controller.signal.throwIfAborted();
    if (!Number.isSafeInteger(store.id) || store.id < 0 || !store.live || this.active.get(store.id)?.controller !== store.controller) throw rpcError(-32002, 'active numeric parent_request_id required for synchronous append');
    const grant = store.leaf?.grant;
    if (!grant && !['command/execute', 'shortcut/execute', 'tool/call'].includes(store.method)) unsupported('synchronous hook append', 'actual host session_leaf grant required');
    if (store.leaf && !grant) throw rpcError(-32002, 'session_leaf authority exhausted or revoked');
    // Consume locally before submission, including ambiguous transport failure.
    // Only an authenticated known successor may authorize another append.
    if (store.leaf) store.leaf.grant = null;
    const result = this.transport.requestSync('session/append_entry', {
      parent_request_id: store.id, resource_owner: store.state.owner, entry_type: type, data: clean,
      ...(grant ? { session_leaf: { grant_id: grant.grant_id, activation_epoch: grant.activation_epoch, operation_id: grant.operation_id } } : {}),
    }, { parent: store.id, signal: store.controller.signal, onCancel: () => store.controller.abort(cancelled()) });
    let committed;
    try { committed = appendReply(result, grant, store.state.owner, Boolean(this.sessionTransport));
      if (this.sessionTransport && grant) this.sessionTransport.append(store, result, grant, committed.successor); }
    catch (error) {
      const ambiguous = rpcError(-32002, `session append outcome unknown: invalid commit reply (${error.message})`);
      this.transport.fail(ambiguous); throw ambiguous;
    }
    if (store.leaf) store.leaf.grant = committed.successor;
    if (this.sessionTransport && grant) return undefined;
    const host = store.state.host;
    const entry = { id: committed.entryId, type: 'custom', customType: type, data: clean, ...(grant ? { parentId: grant.expected_head } : {}) };
    // A durable ACK is the only source of local read-after-write success.
    for (const entries of new Set([host.session_entries, host.session_branch])) if (Array.isArray(entries)) entries.push(entry);
    host.session_leaf_id = committed.head ?? committed.entryId;
    return undefined; // Pi appendEntry is synchronous void, never a Promise.
  }
  track(promise, store = this.scope.getStore()) {
    store = compactionCallbackStore(this, store);
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
  reportCallbackError(event, factory, error) { this.issues.callback(event, factory, error); }
  reportObservationError(event) { this.issues.observation(event); }
  async load() {
    if (this.loaded) return;
    tracePhase('adapter.load.begin');
    let extensions = this.config.extensions;
    if (this.mirrorPiSetup) {
      // Resource discovery is dynamic, but executable sources and static
      // authority stay within the last reviewed capture. Only native host trust
      // admits project factories, never bridge enablement or Pi trust files.
      if (!this.config.registrations || !this.config.entrypoint_sources) invalid('mirror static registrations require review; configure again');
      const setup = discoverPiSetup({ agentDir: this.config.pi_agent_dir, env: process.env, cwd: this.workspace, projectTrusted: this.host.project_trusted === true });
      for (const diagnostic of setup.diagnostics.slice(0, 32)) this.diagnostic(`mirror ${diagnostic}`);
      extensions = setup.extensions.map(entry => realpathSync(entry));
      if (extensions.some(entry => !this.config.extensions?.includes(entry) || !this.config.entrypoint_sources[entry])) invalid('mirror factory list changed; review the Pi setup and configure again');
      this.reviewedFactoriesOmitted = this.config.extensions.some(entry => !extensions.includes(entry));
      const themePath = resolveThemeFile(setup.themesPaths, setup.defaultTheme, setup.diagnostics);
      if (themePath?.endsWith('.json')) configureBridgeTheme({ name: readPiTheme(themePath).name, path: themePath });
      else configureBridgeTheme();
      this.diagnostic(`mirror loading ${extensions.length} enabled Pi ${extensions.length === 1 ? 'extension' : 'extensions'}`);
    }
    if (!Array.isArray(extensions) || extensions.length > 64 || extensions.some(p => typeof p !== 'string')) invalid('bridge extensions must be an explicit list');
    this.entrypoints = [...extensions];
    const advertisedHooks = new Set(this.config.subscribed_hooks ?? []);
    // The module table Pi 1.0.2's own loader uses: Pi packages resolve to the
    // emulated shims, and both TypeBox spellings to the TypeBox Pi ships.
    const aliases = { ...typeboxAliases(), ...emulatedPiAliases() };
    // jiti's content-hashed transform cache, as in Pi 1.0.2, pinned to the
    // adapter's private persistent cache directory.
    const cache = jitiCacheOptions();
    this.jiti = createJiti(import.meta.url, { alias: aliases, moduleCache: true, tryNative: false, ...cache });
    await installHostKeybindings(this);
    // Each extension runs on the route recorded at setup (`extension_runtimes`),
    // or the bridge-wide `pi_runtime`. The installed-Pi route is created only
    // when some extension needs it.
    const globalMode = piRuntimeMode(this.config);
    const routes = this.config.extension_runtimes ?? {};
    if (!routes || typeof routes !== 'object' || Array.isArray(routes)) invalid('extension_runtimes must be a route map');
    if (Object.values(routes).some(mode => mode !== 'shims' && mode !== 'installed')) invalid('extension runtime must be "shims" or "installed"');
    const piAgentDir = this.config.pi_agent_dir;
    if (piAgentDir !== undefined && (typeof piAgentDir !== 'string' || !isAbsolute(piAgentDir))) invalid('pi_agent_dir must be an absolute path');
    let installedJiti;
    const jitiFor = async mode => {
      if (mode !== 'installed') return this.jiti;
      if (!installedJiti) {
        const installed = installedPiAliases(piAgentDir ? { ...process.env, OCTET_PI_AGENT_DIR: piAgentDir } : process.env);
        const candidate = createJiti(import.meta.url, { alias: { ...aliases, ...installed.aliases }, moduleCache: true, tryNative: false, ...cache });
        await activateInstalledPi(candidate, installed.install);
        installedJiti = candidate;
      }
      return installedJiti;
    };
    // One extension that fails to load is reported and skipped; the others
    // still load, as in Pi. A single explicit entrypoint keeps the failure.
    this.loadFailures = [];
    const isolate = extensions.length > 1;
    // Every factory's read-only preparation (path identity, reviewed digest,
    // and the static import-gap gate) runs before anything registers. These
    // checks share no state, so they run concurrently; a factory's own failure
    // is still reported against that factory, in order, below. The import and
    // the `fn(pi)` invocation stay strictly sequential: Pi's registration
    // ORDER is the contract, and jiti's transform path is synchronous anyway.
    const prepared = await Promise.all(extensions.map(async (entry, factory) => {
      let resolved = entry;
      let mode = routes[entry] ?? globalMode;
      let preparationError;
      try {
        resolved = realpathSync(entry);
        mode = routes[resolved] ?? globalMode;
        if (this.mirrorPiSetup && !isDeepStrictEqual(this.config.entrypoint_sources[resolved], reviewedSourceHashes(resolved))) invalid(`reviewed factory sources changed: ${resolved}; review and configure again`);
        if (this.config.entrypoint_sha256?.[resolved]) {
          const actual = createHash('sha256').update(readFileSync(resolved)).digest('hex');
          if (actual !== this.config.entrypoint_sha256[resolved]) invalid(`reviewed factory changed: ${resolved}; configure again`);
        }
        if (mode !== 'installed' && (await scanPiImportGaps(resolved, this.jiti)).direct_tools.length) {
          unsupported('direct Pi tool execution', 'requires the reviewed installed-Pi route, not native child-tool descriptors');
        }
      } catch (error) {
        preparationError = error;
      }
      return { factory, entry: resolved, mode, preparationError };
    }));
    for (const { factory, entry, mode, preparationError } of prepared) {
      const previousTools = new Map(this.tools);
      const restoreProviders = checkpointFactoryProviders(this);
      try {
        // Preserve U28's per-extension notification attribution. Path and
        // reviewed-digest checks already ran in the concurrent preparation pass.
        this.extensionNames.set(factory, extensionDisplayName(entry));
        // A changed reviewed source must refuse before importing fallback code.
        if (preparationError && this.mirrorPiSetup) throw preparationError;
        const jiti = await jitiFor(mode);
        try {
          // Preparation failures (digest mismatch, import-gap gate) take the
          // same classification path as a load failure, exactly as they did
          // when the scan ran inline here.
          if (preparationError) throw preparationError;
          const fn = await jiti.import(entry, { default: true });
          tracePhase(`adapter.factory.import.${factory}`);
          if (typeof fn !== 'function') invalid(`default export must be a factory: ${entry}`);
          const s = { id: this.initializingId, controller: new AbortController(), factory, pending: new Set(), errors: [], live: false };
          const api = this.mirrorPiSetup ? { ...createAPI(this, factory) } : createAPI(this, factory);
          // Mirror static surfaces cannot grow after review via retained APIs.
          // Resource callbacks remain dynamic and host-admitted.
          if (this.mirrorPiSetup) for (const method of ['registerTool', 'registerCommand', 'registerShortcut', 'registerFlag', 'registerMessageRenderer', 'registerEntryRenderer']) {
            const register = api[method];
            api[method] = (...args) => {
              if (this.loaded) invalid(`mirror ${method} changed static registrations; review and configure again`);
              return register(...args);
            };
          }
          await this.scope.run(s, () => fn(api));
          tracePhase(`adapter.factory.registered.${factory}`);
          // Mirror mode advertises one fixed hook set recorded at configure time.
          // A hook outside it would be silently inert at the host, so name it and
          // skip the factory instead of claiming a partial mirror.
          if (this.mirrorPiSetup) {
            const registered = new Set();
            for (const [event, list] of this.events) {
              if (list.some(entry => entry.factory === factory) && hookEvents[event]) registered.add(hookEvents[event]);
            }
            const unsupportedHooks = [...registered].filter(hook => !advertisedHooks.has(hook)).sort();
            if (unsupportedHooks.length) {
              throw rpcError(-32601, `unsupported_feature mirror hook ${unsupportedHooks.join(', ')}: not advertised by this mirror configuration; re-run configure.mjs --mirror after reviewing the Pi setup`);
            }
          }
        } catch (error) { throw mode === 'installed' ? error : await classifyLoadFailure(error, entry, this.jiti); }
      } catch (error) {
        this.forgetFactory(factory, entry);
        if (!isolate) throw error;
        this.tools = previousTools;
        restoreProviders();
        const failure = { entry, route: mode, error: String(error?.message || error).slice(0, 1024), code: error?.code, name: error?.name };
        this.loadFailures.push(failure);
        // configure consumes this exact review-only diagnostic; capture has no
        // prompt or session. Live startup uses the safe grouped issue block.
        if (this.initializingId === undefined) console.error(`[pi-compat] skipped ${entry}: ${failure.error}`);
      }
    }
    this.loaded = true;
    tracePhase('adapter.load.ready', `factories=${extensions.length} failures=${this.loadFailures.length}`);
    if (this.tools.size > 256 || this.commands.size > 256 || this.flags.size > 64 || this.shortcuts.size > 64) throw rpcError(-32012, 'bounds_exceeded registrations');
  }
  // Removes everything a factory registered before its load failed.
  forgetFactory(factory, entry) {
    this.failedFactories.add(factory);
    for (const map of [this.tools, this.commands, this.shortcuts, this.flags]) {
      for (const [key, value] of [...map]) if (value.factory === factory) map.delete(key);
    }
    for (const [event, list] of [...this.events]) {
      const kept = list.filter(value => value.factory !== factory);
      if (kept.length) this.events.set(event, kept); else this.events.delete(event);
    }
    for (const entries of this.bus.listeners.values()) for (const value of [...entries]) if (value.factory === factory) value.off();
    this.transcript.factories.delete(factory);
    this.timers.clearWhere(store => store?.factory === factory);
    forgetMcpRegistrations(this, entry);
  }
  metadata() {
    const hooks = [...new Set(this.config.subscribed_hooks ?? [...this.events.keys()].filter(e => hookEvents[e]).map(e => hookEvents[e]))];
    // Message history needs the real prompt and durable assistant/tool commits,
    // even when the factory subscribes only to Pi notification events. These
    // native inputs do not create public Pi callbacks or bypass feature checks.
    if (['agent_end', 'message_start', 'message_update', 'message_end'].some(event => this.events.has(event))) for (const name of ['before_prompt', 'model_turn_end']) if (!hooks.includes(name)) hooks.push(name);
    const notifications = [...Object.values(notificationEvents), ...Object.values(followingEvents)].some(event => this.events.has(event));
    // Commands and notification-only factories need real owner lifecycle
    // bindings even when they have no Pi session_start/session_end callbacks.
    if (this.commands.size || this.shortcuts.size || this.transcript.factories.size || [...this.tools.values()].some(tool => tool.definition.renderCall || tool.definition.renderResult) || hasMcpRegistrations(this) || this.events.has('mcp_servers_change') || notifications || hooks.length) for (const name of ['session_start', 'session_end']) if (!hooks.includes(name)) hooks.push(name);
    const completions = [...this.commands].filter(([, command]) => command.definition.getArgumentCompletions).map(([name]) => name);
    return {
      ...(completions.length ? { argument_completions: completions } : {}),
      ...(this.transcript.factories.size ? { transcript_renderers: this.transcript.metadata() } : {}),
      tools: [...this.tools].map(([name, tool]) => toolWire(name, tool)),
      commands: [...this.commands].map(([name, { definition: d }]) => ({ name, description: d.description || name, ...(d.usage ? { usage: d.usage } : {}) })),
      hooks: hooks.sort(), flags: [...this.flags].map(([name, { definition }]) => ({ name, ...definition })),
      shortcuts: [...this.shortcuts].map(([key, { definition: d }], i) => ({ name: this.config.registrations?.shortcuts?.find(shortcut => shortcut.key === key)?.name ?? `pi-shortcut-${i}`, key, description: d.description || key })),
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
    if (this.features.has('session_compaction_v1')) this.require('session_control_v1');
    if (this.features.has('transcript_render_v1') || this.features.has('terminal_input_intercept_v1')) this.require('remote_ui');
    const builtinOverrides = params.capabilities?.builtin_tool_overrides === undefined ? [] : params.capabilities.builtin_tool_overrides;
    if (!Array.isArray(builtinOverrides) || builtinOverrides.length > BUILTIN_TOOL_NAMES.length
        || new Set(builtinOverrides).size !== builtinOverrides.length
        || builtinOverrides.some(name => !BUILTIN_TOOL_NAMES.includes(name))) invalid('builtin tool override grant');
    this.builtinToolOverrides = new Set(builtinOverrides);
    if (builtinOverrides.length) this.require('builtin_tool_overrides_v1');
    else {
      if (required.includes('builtin_tool_overrides_v1')) invalid('builtin tool override feature requires a native grant');
      this.features.delete('builtin_tool_overrides_v1');
    }
    this.maxConcurrent = Math.min(8, params.protocol.limits?.max_concurrent_requests || 1);
    const routed = SESSION_FEATURES.filter(f => this.features.has(f));
    if (routed.length && (routed.length !== 2 || this.maxConcurrent < 2 || !this.features.has('session_entries'))) invalid('paired session transport requirements');
    if (routed.length) {
      const frame = params.protocol.limits?.max_message_bytes;
      this.sessionTransport = new SessionTransport(this, params.protocol.session_snapshot_transport_v1, frame);
      this.transport.setFrameLimit(frame);
    } else if (params.protocol.session_snapshot_transport_v1 !== undefined) invalid('session transport without paired features');
    this.namespace = params.extension?.name;
    this.scratchDirectory = params.artifact_directory;
    this.initialHost = params.host || {}; this.workspace = params.workspace; this.initializingId = store.id;
    for (const flag of params.flag_values || []) this.flagValues.set(flag.name, flag.value);
    await this.load();
    validateProviderInitialization(this);
    const metadata = this.metadata(), declared = params.contributes || {};
    if (metadata.argument_completions?.length) this.require('autocomplete');
    if (metadata.hooks.includes('resources_discover')) this.require('resource_paths_v1');
    if (metadata.hooks.some(hook => hook === 'provider_context' || sessionOperationHooks.includes(hook) || modelTurnHooks.includes(hook))) this.require('session_entries');
    if (metadata.hooks.some(hook => pipelineHooks.includes(hook))) this.require('pipeline_hooks_v1');
    if (metadata.events.includes('before_agent_start')) this.require('before_prompt_state_v1');
    if (metadata.events.includes('input')) this.require('input_transform_v1');
    if (metadata.tools.some(tool => tool.prompt_snippet !== undefined || tool.prompt_guidelines !== undefined)) this.require('tool_prompt_metadata_v1');
    // Only uncaptured manual entrypoints use a wholly dynamic renderer catalog.
    // Reviewed snapshots and mirrors retain their captured static registrations.
    if (!this.config.registrations && metadata.tool_renderers.length) this.require('dynamic_tool_renderers');
    // A skipped extension leaves its declared registrations unused; nothing
    // beyond the reviewed declarations may appear.
    const skipped = this.loadFailures?.length > 0 || this.reviewedFactoriesOmitted === true;
    if (skipped && !isDeepStrictEqual([...metadata.commands.map(command => command.name)].sort(), [...(declared.commands || [])].sort())) this.require('runtime_commands');
    for (const [kind, names] of [['tools', metadata.tools.map(t => t.name)], ['commands', metadata.commands.map(c => c.name)], ['hooks', metadata.hooks], ['tool_renderers', metadata.tool_renderers]]) {
      if (kind === 'tools' && this.features.has('dynamic_tools')) continue;
      if (kind === 'commands' && this.features.has('runtime_commands')) continue;
      if (kind === 'tool_renderers' && this.features.has('dynamic_tool_renderers')) continue;
      const allowed = [...(declared[kind] || [])];
      const matches = skipped ? names.every(name => allowed.includes(name)) : JSON.stringify(allowed.sort()) === JSON.stringify([...names].sort());
      if (!matches) invalid(`manifest ${kind} differs from reviewed registrations: ${names.join(', ')}`);
    }
    const staticMetadata = this.config.registrations;
    if (staticMetadata) {
      const staticSurface = value => {
        if (this.mirrorPiSetup) return value;
        const { tools, ...rest } = value; return rest;
      };
      const matches = skipped
        ? Object.entries(staticSurface(metadata)).every(([kind, values]) => values.every(value => (staticMetadata[kind] || []).some(allowed => isDeepStrictEqual(value, allowed))))
        : isDeepStrictEqual(staticSurface(staticMetadata), staticSurface(metadata));
      if (!matches) invalid('reviewed registration metadata changed; configure again');
    }
    for (const [name, { definition: d }] of this.flags) {
      const value = this.flagValues.get(name) ?? d.default;
      if (d.type === 'integer' ? !Number.isSafeInteger(value) : typeof value !== d.type) invalid(`flag ${name} type`);
    }
    if (metadata.shortcuts.length) this.require('shortcuts');
    // Static reviewed shortcut declarations are admitted during native startup.
    // A reverse registration here would wait for a frontend that is not live yet.
    if (skipped ? !metadata.shortcuts.every(shortcut => (declared.shortcuts || []).some(allowed => isDeepStrictEqual(shortcut, allowed))) : !isDeepStrictEqual(declared.shortcuts || [], metadata.shortcuts)) invalid('manifest shortcuts differ from reviewed registrations');
    this.initialized = true;
    return {
      // Shortcuts are static on the host wire. Keep the reviewed declarations;
      // invoking one from a skipped factory refuses instead of running stale JS.
      api_version: '0.4', tools: metadata.tools, commands: metadata.commands, shortcuts: skipped ? (declared.shortcuts || []) : metadata.shortcuts,
      protocol: { version: '0.4', features: [...this.features], limits: { max_concurrent_requests: this.maxConcurrent, ...(this.sessionTransport ? { max_message_bytes: this.sessionTransport.frame } : {}) },
        ...(this.sessionTransport ? { session_snapshot_transport_v1: this.sessionTransport.profile } : {}),
        ...(this.features.has('lifecycle_events') ? { lifecycle_events: ['turn/started', 'turn/settled', 'tool/started', 'tool/settled'] } : {}) },
    };
  }
  // One Pi-style block after initialize, not a separate notice per issue kind.
  reportStartupIssues() { this.issues.startup(); }
  // Late renderer registrations are still inert without the native consumer.
  reportInertRenderers() { this.issues.reportInertRenderers(); }
  /// The completion chain lives in one place: `lib/completions.mjs`. These
  /// wrappers only expose it to the initialize path and to provider admission.
  ensureAutocompleteChain() {
    return ensureCompletionChain(this);
  }
  /// Called before the initialize reply is written, so no completion query can
  /// see a missing chain while that reply is in flight.
  armAutocomplete() {
    if (this.autocompleteStarted || !this.features.has('autocomplete') || !this.commands.size) return this.autocompleteRegistration;
    return ensureCompletionChain(this);
  }
  registerAutocomplete(contribution = false) {
    return startCompletionChain(this, contribution);
  }
  bind(params, store, { foreground = store.method === 'hook/run' && params.hook === 'session_start', requireLeaf, preparationHead } = {}) {
    const context = params.context || {};
    if (Object.hasOwn(context.host || {}, 'theme')) nativeTheme(context.host.theme);
    const owner = context.resource_owner || params.payload?.binding;
    if (!owner) {
      if (params.session_leaf !== undefined || requireLeaf) invalid('complete resource_owner required');
      return;
    }
    const key = ownerKey(owner);
    for (const field of ['session_id', 'extension_instance_id']) bounded(owner[field], `resource_owner ${field}`, 256);
    if (context.resource_owner && params.payload?.binding && ownerKey(params.payload.binding) !== key) invalid('session lifecycle binding owner');
    const lifecycle = store.method === 'hook/run' && ['session_start', 'session_end'].includes(params.hook);
    // Validate private authority before touching any retained state. A hook's
    // session binding is not a host foreground lifecycle transition.
    let grant;
    if (params.session_leaf !== undefined) {
      if (store.method !== 'hook/run') invalid('session_leaf requires hook/run');
      grant = leafGrant(params.session_leaf, owner);
      if (Object.hasOwn(context.host || {}, 'session_leaf_id') && context.host.session_leaf_id !== grant.expected_head) invalid('session_leaf snapshot head');
    }
    if (requireLeaf && (!grant || preparationHead !== undefined && grant.expected_head !== preparationHead)) unsupported(requireLeaf, requireLeaf === 'provider_context' ? 'matching native session_leaf preparation required' : 'actual native session_leaf consumer required');
    let state = this.states.get(key);
    // Revisiting a session creates a new incarnation. Captured old contexts
    // still point at their retired state and must never become live again.
    if (state && !state.alive && !(store.method === 'hook/run' && params.hook === 'session_start')) {
      throw rpcError(-32002, 'not_foreground_owner settled owner');
    }
    if (!lifecycle && this.foreground && (owner.extension_instance_id !== this.foreground.owner.extension_instance_id || owner.process_generation !== this.foreground.owner.process_generation)) throw rpcError(-32002, 'not_foreground_owner process binding');
    // Commands/shortcuts are foreground actions, not a way to switch a live
    // foreground owner. Their first binding supports hosts without start hooks.
    foreground ||= !this.foreground && ['command/execute', 'shortcut/execute'].includes(store.method);
    if (!foreground && this.foreground && state !== this.foreground && ['command/execute', 'shortcut/execute'].includes(store.method)) throw rpcError(-32002, 'not_foreground_owner command belongs to an inactive session');
    if (!state || !state.alive) {
      state = { key, owner: Object.freeze({ ...owner }), alive: true, workspace: context.workspace || this.workspace, host: { ...this.initialHost, ...(context.host || {}) }, statuses: new Map(), branchListeners: new Set(), uiQueues: new Map(), parent: store.id };
      this.states.set(key, state);
    } else {
      state.workspace = context.workspace || state.workspace;
      state.host = { ...state.host, ...(context.host || {}) }; state.parent = store.id;
    }
    store.state = state;
    if (grant) store.leaf = store.activation?.leaf ?? { grant };
    if (foreground) {
      if (this.foreground && this.foreground !== state) this.retire(this.foreground).catch(e => this.backgroundError(e));
      this.foreground = state;
      for (const waiter of this.foregroundWaiters) waiter(state);
    }
  }
  prepareSessionReplacement(store) {
    // Admission belongs before session/create: its native lifecycle retires the
    // old session before returning the setup receipt. Only this live command's
    // one-shot continuation survives; no retained context gains new authority.
    this.assertOwner(store);
    const previous = store.state;
    const liveCommand = () => {
      store.controller.signal.throwIfAborted();
      if (this.stopping || store.method !== 'command/execute' || !store.live || this.active.get(store.id)?.controller !== store.controller) throw rpcError(-32002, 'not_foreground_owner replacement requires its live command');
    };
    liveCommand();
    let consumed = false;
    return (context, sessionId) => {
      liveCommand();
      if (consumed || store.state !== previous || this.foreground !== previous) throw rpcError(-32002, 'not_foreground_owner replacement continuation retired');
      consumed = true;
      const next = context?.resource_owner;
      ownerKey(next);
      if (next.extension_instance_id !== previous.owner.extension_instance_id || next.process_generation !== previous.owner.process_generation || next.session_id === previous.owner.session_id) invalid('replacement receipt owner');
      if (context.host?.session_id !== sessionId) invalid('setup replacement session id');
      const fresh = { ...store };
      this.bind({ context }, fresh, { foreground: true });
      return fresh;
    };
  }
  // Session facts for one bound store. The negotiated history transport only
  // overlays the fields it delivers (entries, branch, leaf, file, header,
  // labels); it never changes the owner or who is foreground.
  sessionFacts(store) {
    return this.sessionTransport ? this.sessionTransport.host(store) : store.state.host;
  }
  // Resolves once the host binds the session that replaced the active one.
  foregroundFor(sessionId, signal) {
    signal.throwIfAborted();
    if (this.foreground?.alive && this.foreground.host.session_id === sessionId) return Promise.resolve(this.foreground);
    return new Promise((resolve, reject) => {
      const cleanup = () => { this.foregroundWaiters.delete(waiter); signal.removeEventListener('abort', abort); };
      const waiter = state => { if (state.alive && state.host.session_id === sessionId) { cleanup(); resolve(state); } };
      const abort = () => { cleanup(); reject(signal.reason); };
      this.foregroundWaiters.add(waiter);
      signal.addEventListener('abort', abort, { once: true });
    });
  }
  async retire(state) {
    cancelProviderStream(this, undefined, state);
    this.transcript.retire(state);
    this.sessionTransport?.retire(state);
    state.alive = false; this.terminalInput.retire(state); retireMcpSession(state); retireAutocomplete(state); retireCompactions(this, state); retireChildSessions(this, state); this.timers.owner(state); this.bus.ownerEnded(state); state.branchListeners.clear();
    // Session-scoped hooks are cancelled on their own owner retirement, never
    // another hook's private binding. Replacement commands retain their parent.
    for (const store of this.active.values()) if (store.state === state && (store.resourceDiscovery || store.providerContext || store.method === 'hook/run' && !['session_start', 'session_end'].includes(store.hook))) store.controller.abort(cancelled());
    await this.ui.ownerEnded(state);
  }
  updated(params) {
    const key = ownerKey(params.resource_owner);
    // A routed publication always invalidates its own delivery barrier first;
    // that is delivery bookkeeping for any owner, never a foreground change.
    const routedPublication = this.sessionTransport && params.host?.session_view_revision !== undefined;
    if (routedPublication) {
      const revision = params.host.session_view_revision;
      if (!Number.isSafeInteger(revision) || revision < 1) invalid('session view revision');
      this.sessionTransport.invalidate(params.resource_owner, revision);
    }
    const state = this.states.get(key);
    if (routedPublication) {
      // Retained host facts belong to the current foreground only. For any
      // other owner the invalidation above is the whole effect: that is a
      // delivery fact, not a refusal.
      if (!this.features.has('remote_ui') || !state?.alive || state !== this.foreground) return;
    } else {
      this.require('remote_ui');
      if (!state || !state.alive || state !== this.foreground) throw rpcError(-32002, 'not_foreground_owner context/updated');
    }
    const next = plainJSON(params.host, 'host snapshot', 524288);
    if (Object.hasOwn(next, 'theme')) nativeTheme(next.theme);
    const previous = state.host.git_branch; state.host = { ...state.host, ...next };
    if (previous !== state.host.git_branch) for (const listener of state.branchListeners) listener();
    for (const surface of this.ui.surfaces.values()) if (surface.store.state === state) this.scope.run(surface.store, () => { surface.tui.invalidate(); surface.requestRender(); });
  }

  async runEvent(event, value, store, { veto = false } = {}) {
    for (const entry of [...this.events.get(event) || []]) {
      store.controller.signal.throwIfAborted();
      const childStore = { ...store, factory: entry.factory };
      let result;
      try { result = await this.scope.run(childStore, () => entry.handler(value, createContext(this, childStore))); }
      catch (error) {
        store.controller.signal.throwIfAborted(); this.assertSessionOwner(store);
        // Pi's ExtensionRunner.emit reports a failing handler and runs the
        // next one. Vetoes, cancellation, owner refusals and tracked host
        // mutations still propagate; a skipped callback is never a host receipt.
        if (veto || error?.code === -32800 || error?.code === -32002 || store.errors.includes(error)) throw error;
        this.reportCallbackError(event, entry.factory, error);
        continue;
      }
      store.controller.signal.throwIfAborted();
      if (result !== undefined) {
        if (veto && result && typeof result === 'object' && Object.keys(result).every(k => ['block', 'reason', 'terminate'].includes(k))) {
          if (result.block) {
            if (result.terminate !== undefined && typeof result.terminate !== 'boolean') invalid('tool_call terminate');
            store.toolTermination = result.terminate;
            return { action: 'deny', reason: result.reason ?? 'Blocked by Pi extension' };
          }
        } else if (event.startsWith('session_before_')) unsupported(`${event} result`, 'wire cannot apply this event transformation or veto');
        // Pi ignores other handlers' return values.
        else if (veto) unsupported(`${event} result`, 'wire cannot apply this event transformation or veto');
      }
    }
    return { action: 'continue' };
  }
  // Pi's emitToolResult: handlers chain, a throwing handler is reported and
  // skipped, and content replaced without structuredContent drops the old one.
  async runToolResult(payload, store) {
    const event = { type: 'tool_result', toolName: payload.name, toolCallId: payload.tool_call_id,
      ...(payload.parent_tool_call_id == null ? {} : { parentToolCallId: payload.parent_tool_call_id }),
      input: payload.arguments, content: structuredClone(payload.pi_content ?? [{ type: 'text', text: payload.output }]),
      details: payload.metadata?.pi_details, ...(payload.structured_content === undefined ? {} : { structuredContent: payload.structured_content }), isError: payload.is_error,
      ...(payload.metadata?.pi_usage !== undefined ? { usage: structuredClone(payload.metadata.pi_usage) } : payload.usage ? { usage: { input: payload.usage.input_tokens, output: payload.usage.output_tokens, cacheRead: payload.usage.cache_read_tokens, cacheWrite: payload.usage.cache_write_tokens, totalTokens: payload.usage.total_tokens } } : {}) };
    const changed = new Set();
    for (const entry of [...this.events.get('tool_result') || []]) {
      store.controller.signal.throwIfAborted();
      const childStore = { ...store, factory: entry.factory };
      let result;
      try { result = await this.scope.run(childStore, () => entry.handler(event, createContext(this, childStore))); }
      catch (error) {
        store.controller.signal.throwIfAborted(); this.assertSessionOwner(store);
        if (error?.code === -32800 || error?.code === -32002 || store.errors.includes(error)) throw error;
        this.reportCallbackError('tool_result', entry.factory, error); continue;
      }
      store.controller.signal.throwIfAborted();
      if (!result) continue;
      fields(result, ['content', 'details', 'structuredContent', 'isError', 'usage'], 'tool_result result');
      if (result.usage !== undefined) {
        fields(result.usage, ['input', 'output', 'cacheRead', 'cacheWrite', 'totalTokens', 'cost'], 'tool usage');
        for (const key of ['input', 'output', 'cacheRead', 'cacheWrite', 'totalTokens']) if (!Number.isSafeInteger(result.usage[key]) || result.usage[key] < 0) invalid(`tool usage ${key}`);
        fields(result.usage.cost, ['input', 'output', 'cacheRead', 'cacheWrite', 'total'], 'tool usage cost');
        for (const key of ['input', 'output', 'cacheRead', 'cacheWrite', 'total']) if (!Number.isFinite(result.usage.cost[key]) || result.usage.cost[key] < 0) invalid(`tool usage cost ${key}`);
        event.usage = plainJSON(result.usage, 'tool usage'); changed.add('usage');
      }
      if (result.content !== undefined) { event.content = result.content; changed.add('content'); if (result.structuredContent === undefined) { delete event.structuredContent; changed.delete('structuredContent'); } }
      if (result.details !== undefined) { event.details = result.details; changed.add('details'); }
      if (result.structuredContent !== undefined) { event.structuredContent = result.structuredContent; changed.add('structuredContent'); }
      if (result.isError !== undefined) { event.isError = Boolean(result.isError); changed.add('isError'); }
    }
    if (!changed.size) return undefined;
    return {
      ...(changed.has('content') ? { content: event.content.every(part => part.type === 'text') ? textOnly(event.content).map(part => part.text) : await toolContent(this, event.content, store) } : {}),
      ...(changed.has('structuredContent') ? { structured_content: plainJSON(event.structuredContent, 'structured content', 262144) } : {}),
      ...((changed.has('details') || changed.has('usage')) ? { metadata: { ...(payload.metadata ?? {}), ...(changed.has('details') ? { pi_details: plainJSON(event.details, 'tool details', 65536, { omitUndefined: true }) } : {}), ...(changed.has('usage') ? { pi_usage: event.usage } : {}) } } : {}),
      ...(changed.has('usage') ? { usage: { input_tokens: event.usage.input, output_tokens: event.usage.output, cache_read_tokens: event.usage.cacheRead, cache_write_tokens: event.usage.cacheWrite, cache_write_1h_tokens: 0, reasoning_tokens: 0, total_tokens: event.usage.totalTokens } } : {}),
      ...(changed.has('isError') ? { is_error: event.isError } : {}),
    };
  }
  queued(store, work, { holdOnCancel = false } = {}) {
    if (++this.hookQueued > 128) { this.hookQueued--; throw rpcError(-32012, 'bounds_exceeded ordered hook queue'); }
    const running = this.hookTail.then(() => { store.controller.signal.throwIfAborted(); return Promise.resolve().then(work); });
    const raced = cancellable(running, store.controller.signal);
    const result = holdOnCancel ? raced.catch(async error => {
      if (!store.controller.signal.aborted) throw error;
      try { await running; } catch { /* Preserve cancellation after the callback settles. */ }
      throw error;
    }) : raced;
    this.hookTail = result.catch(() => {});
    return result.finally(() => { this.hookQueued--; });
  }
  async dispatch(message, store) {
    const p = message.params;
    if (message.method === 'initialize') return this.initialize(p, store);
    if (!this.initialized) invalid('not initialized');
    if (message.method === 'ui/terminal-input/intercept') return this.terminalInput.dispatch(p, store);
    if (message.method === 'session/snapshot/prepare') {
      this.require('session_snapshot_transport_v1');
      return this.sessionTransport.prepare(p, store);
    }
    await this.sessionTransport?.hydrate(p, store);
    if (message.method === 'provider/stream') return prepareProviderStream(this, p, store);
    if (message.method === 'ui/autocomplete/complete') return commandCompletions(this, p, store);
    if (message.method === 'hook/run' && p.hook === 'resources_discover') return discoverResources(this, p, store);
    if (message.method === 'hook/run' && p.hook === 'provider_context') return projectContext(this, p, store);
    if (message.method === 'hook/run' && pipelineHooks.includes(p.hook)) return providerPipeline(this, p, store);
    if (message.method === 'hook/run' && sessionReplacementHooks.includes(p.hook)) return sessionReplacement(this, p, store);
    if (message.method === 'hook/run' && sessionOperationHooks.includes(p.hook)) return sessionOperation(this, p, store);
    if (message.method === 'hook/run' && modelTurnHooks.includes(p.hook)) return modelTurn(this, p, store);
    this.bind(p, store);
    if (message.method === 'transcript/render') return this.transcript.render(p, store);
    if (message.method === 'hook/run' && p.hook === 'before_prompt' && p.payload?.phase === 'input') {
      this.require('input_transform_v1');
      return this.queued(store, async () => ({ disposition: { action: 'continue' }, context: [], notifications: [],
        input_event: await transformInput(this, p.payload, store) }));
    }
    if (message.method === 'command/execute') {
      const cmd = this.commands.get(p.name); if (!cmd) invalid(`unknown command ${p.name}`);
      const opened = deferred(); store.detach = () => { store.detached = true; opened.resolve({ text: '', notifications: [], context: [] }); };
      store.factory = cmd.factory;
      const run = this.scope.run(store, async () => {
        // Negotiated transport support is not a foreground UI lease. Ordinary
        // headless commands must not acquire a composer before their handler.
        if (this.features.has('composer') && store.state.host.has_ui !== false) {
          const { text } = await this.hostCall('composer/get', {}, store);
          if (!this.ui.activeEditor(store)) store.state.host.composer_text = text;
        }
        // Pi awaits command handlers but ignores their return value.
        await cmd.definition.handler((p.arguments || []).join(' '), createContext(this, store));
        await this.flush(store); return { text: '', notifications: [], context: [] };
      });
      run.catch(error => { if (store.detached && !store.controller.signal.aborted) this.backgroundError(error); });
      return Promise.race([run, opened.promise]);
    }
    if (message.method === 'shortcut/execute') {
      this.require('shortcuts');
      const meta = this.metadata().shortcuts.find(shortcut => shortcut.name === p.name);
      if (!meta) invalid(`unknown shortcut ${p.name}`);
      const shortcut = this.shortcuts.get(meta.key);
      store.factory = shortcut.factory;
      await this.scope.run(store, () => shortcut.definition.handler(createContext(this, store)));
      await this.flush(store);
      return { text: '', notifications: [], context: [] };
    }
    if (message.method === 'tool/prepare_arguments') return prepareRegisteredArguments(this, p, store);
    if (message.method === 'tool/call') return executeRegisteredTool(this, p, store, await createToolContext(this, p, store, createContext(this, store)));
    if (message.method === 'hook/run') return this.queued(store, async () => {
      const events = Object.keys(hookEvents).filter(e => e !== 'input' && hookEvents[e] === p.hook);
      if (!this.metadata().hooks.includes(p.hook)) invalid(`unknown hook ${p.hook}`);
      let disposition = { action: 'continue' };
      const payload = p.payload || {};
      let systemPrompt, toolInput, customMessages = [];
      if (p.hook === 'session_start') await startMcpSession(this, store);
      if (p.hook === 'after_tool_call') {
        const toolResult = await this.runToolResult(payload, store);
        await this.flush(store);
        return { disposition, context: [], notifications: [], ...(toolResult ? { tool_result: toolResult } : {}) };
      }
      for (const event of events) {
        if (event === 'before_agent_start' && this.events.has(event)) {
          const before = await beforeAgentStart(this, payload, store);
          systemPrompt = before.systemPrompt;
          customMessages.push(...before.messages);
          continue;
        }
        // Pi hands handlers one mutable input; later handlers see earlier mutations.
        const value = p.hook === 'before_tool_call' ? { type: event, toolName: payload.name, toolCallId: payload.tool_call_id, ...(payload.parent_tool_call_id == null ? {} : { parentToolCallId: payload.parent_tool_call_id }), input: toolInput ??= structuredClone(payload.arguments ?? {}) }
          : p.hook === 'after_tool_call' ? { type: event, toolName: payload.name, input: payload.arguments, content: [{ type: 'text', text: payload.output }], isError: payload.is_error }
          // Pi's session_start always has a reason; a host that gives none is
          // starting this process's session, which Pi calls "startup".
          : facade({ type: event, ...(event === 'session_start' && payload.reason === undefined ? { reason: 'startup' } : {}), ...payload }, `${event} event`);
        disposition = await this.runEvent(event, value, store, { veto: p.hook === 'before_tool_call' });
        if (disposition.action === 'deny') break;
      }
      await this.flush(store);
      if (p.hook === 'before_prompt') rememberPrompt(store, payload);
      if (p.hook === 'session_end' && store.state) await this.retire(store.state);
      const replaced = toolInput !== undefined && disposition.action === 'continue' && !isDeepStrictEqual(toolInput, payload.arguments ?? {});
      return { disposition, context: [], notifications: [], ...(customMessages.length ? { custom_messages: customMessages } : {}), ...(systemPrompt === undefined || systemPrompt === payload.system_prompt ? {} : { system_prompt: systemPrompt }),
        ...(replaced ? { arguments: plainJSON(toolInput, 'tool arguments', 1048576) } : {}), ...(store.toolTermination === undefined ? {} : { terminate: store.toolTermination }) };
    }, { holdOnCancel: true });
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
    if (message.method === 'provider/cancel') { cancelProviderStream(this, p.stream_id); return; }
    // Advisory notifications cannot deliver a veto or a post-commit append
    // consumer. Actual Pi compaction callbacks now arrive as awaited hook/run.
    if (['compaction/started', 'compaction/settled'].includes(message.method)) return;
    if (message.method === 'ui/editor-state') {
      this.require('editor_handoff'); const state = this.foreground;
      // The host can publish its first composer snapshot before any request
      // has issued an owner. There is no retained context to update yet; the
      // first command/editor mount reads the current composer from the host.
      if (!state?.alive) return;
      if (!Number.isSafeInteger(p.revision) || p.revision < 0) invalid('editor state revision');
      if (state.editorRevision !== undefined && p.revision <= state.editorRevision) return;
      state.editorRevision = p.revision;
      const text = bounded(p.text, 'editor text', 262144);
      const editors = [...this.ui.surfaces.values()].filter(surface => surface.placement === 'editor' && surface.store.state === state);
      // This snapshot has no mount/input identity and is never a checkpoint ACK.
      // Even a late echo after the queue drains must not replace the local draft.
      // Genuine concurrent native mutations are arbitrated by the host fence.
      if (editors.length) return;
      state.host.composer_text = text;
      return;
    }
    if (message.method.startsWith('ui/')) { this.ui.handle(message.method, p); return; }
    if (message.method === 'context/updated') { this.updated(p); return; }
    let event = notificationEvents[message.method];
    let handler;
    if (message.method === 'shortcut/trigger') {
      this.require('shortcuts'); const meta = this.metadata().shortcuts.find(s => s.name === p.id);
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
      } else {
        if (runMessageMethods.has(message.method)) await handleRunMessage(this, message.method, p, store);
        else await this.runEvent(event, facade({ type: event, ...p }, `${event} event`), store);
        const following = followingEvents[message.method];
        if (following) await this.runEvent(following, { type: following }, store);
      }
      await this.flush(store);
    });
  }
  cancel(id) {
    this.active.get(id)?.controller.abort(cancelled()); cancelCompactions(this, id); this.transport.cancel(id);
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
    const store = { id: message.id, method: message.method, hook: message.method === 'hook/run' ? message.params?.hook : undefined, controller: new AbortController(), pending: new Set(), errors: [], live: true };
    this.active.set(message.id, store);
    if (message.method === 'initialize') this.initializationBarrier = deferred();
    let settlementError = rpcError(-32002, 'originating request did not complete');
    try {
      // The host can send its first command before the initialize write's
      // receipt arrives. Keep startup publication ahead of that command without
      // waiting for reverse registrations (whose replies the host must serve).
      if (message.method !== 'initialize') await this.initializationBarrier?.promise;
      let result = await withContextLimits(this.sessionTransport?.profile, () => this.scope.run(store, () => this.dispatch(message, store)));
      if (this.sessionTransport && store.hook === 'provider_context') result = await this.sessionTransport.projection(result, store);
      store.controller.signal.throwIfAborted();
      // Arm the chain before this reply's write settles. The host may query
      // completions the moment it reads the line, and nothing here orders that
      // inbound request after the write callback.
      if (message.method === 'initialize') this.armAutocomplete();
      // No later reply may overtake the initialize epilogue: the host drops
      // extension-originated notices until it has read the initialize result
      // and subscribed, so the grouped startup notice is published only after
      // that write settles. Gating every later reply on the epilogue gives the
      // notice a total order without moving it ahead of the reply.
      if (message.method !== 'initialize') await this.initializeEpilogue;
      if (!this.stopping) {
        // A reply is also a barrier for anything already published: the host
        // may treat this response as evidence that earlier notices were
        // delivered, so flush pending extension-originated writes first.
        await this.issues.flush();
        const reply = this.transport.send({ jsonrpc: '2.0', id: message.id, result });
        if (message.method === 'initialize') {
          // Registration stays behind the reply so the query chain waits for
          // the host to have resolved the negotiated feature; the notice is
          // published last and the epilogue then resolves.
          this.initializeEpilogue = (async () => {
            try {
              await reply;
            } catch {
              return;
            }
            try {
              this.registerAutocomplete();
              startProviderRegistration(this);
              this.reportStartupIssues();
            } catch (error) {
              this.backgroundError(error);
            }
          })();
        }
        await reply;
        settlementError = undefined;
        if (message.method === 'provider/stream') startProviderStream(this, message.params.stream_id);
      }
    } catch (error) {
      settlementError = error;
      if (!this.stopping) {
        if (message.method !== 'initialize') await this.initializeEpilogue;
        await this.issues.flush();
        await this.transport.send({ jsonrpc: '2.0', id: message.id, error: {
          code: store.controller.signal.aborted ? -32800 : (Number.isInteger(error.code) ? error.code : -32603),
          message: store.controller.signal.aborted ? 'request cancelled' : String(error?.message || error).slice(0, 4096),
          ...(!store.controller.signal.aborted && fallbackData(error) ? { data: fallbackData(error) } : {}),
        } });
      }
    } finally {
      if (message.method === 'initialize') this.initializationBarrier.resolve();
      this.sessionTransport?.settle(store, settlementError);
      store.live = false; this.active.delete(message.id); this.transport.settleParent(message.id);
      settleCompactions(this, store, settlementError);
    }
  }
  async shutdown(id) {
    if (this.stopping) return;
    this.stopping = true;
    cancelProviderStream(this);
    retireCompactions(this, undefined, cancelled());
    retireChildSessions(this); this.uninstallChildren?.();
    for (const store of this.active.values()) store.controller.abort(cancelled());
    for (const state of this.states.values()) this.transcript.retire(state);
    this.timers.all(); await this.ui.shutdown();
    for (const state of this.states.values()) state.alive = false;
    try { await deadline(this.transport.send({ jsonrpc: '2.0', id, result: {} }), 750); await deadline(this.transport.idle(), 750); }
    finally {
      try { await deadline(this.transport.close(), 250); }
      finally { process.exit(0); }
    }
  }
  lost(error, eof) {
    cancelProviderStream(this);
    this.stopping = true; retireCompactions(this, undefined, error); this.timers.all(); retireChildSessions(this); this.uninstallChildren?.();
    for (const store of this.active.values()) store.controller.abort(cancelled());
    this.ui.shutdown().finally(() => { if (!eof) console.error(`[pi-compat transport] ${error.message}`); process.exit(eof ? 0 : 1); });
  }
}
