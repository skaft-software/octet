import { withContextLimits } from './context-limits.mjs';
import { readFileSync, statSync } from 'node:fs';
import { basename, dirname, join, parse, resolve } from 'node:path';
import { bounded, facade, fields, invalid, plainJSON, strict, unsupported } from './errors.mjs';
import { contextTheme } from './theme.mjs';
import { exec } from './exec.mjs';
import { mcpAPI } from './mcp.mjs';
import { chromeAPI, dialogAPI, editorAPI } from './ui-api.mjs';
import { currentModel, thinkingLevel, scopedModels, modelRegistry, registerProvider, unregisterProvider } from './providers.mjs';
import { setModel, setThinkingLevel } from './model-control.mjs';
import { registerTool, toolSnapshot, getAllTools, setActiveTools } from './tools.mjs';
import { Editor } from './editor.mjs';
import { translateSessionEntries } from './session-mirror.mjs';
import { compactionCallbackStore, requestCompaction } from './compaction.mjs';
import { matchesKey } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';
import { contextFacts, getSettings } from './context-api.mjs';
import { sessionMethods, sessionHeader } from './session-methods.mjs';
import { customMessageParams, userMessageParams } from './custom-messages.mjs';
import { buildContextEntries, buildSessionProjection } from './context.mjs';

export const hookEvents = {
  session_before_switch: 'session_before_switch', session_before_fork: 'session_before_fork',
  session_start: 'session_start', session_end: 'session_end', session_shutdown: 'session_end',
  tool_call: 'before_tool_call', tool_result: 'after_tool_call', input: 'before_prompt',
  before_agent_start: 'before_prompt', after_response: 'after_response', resources_discover: 'resources_discover',
  context: 'provider_context', context_with_system: 'provider_context', turn_start: 'model_turn_start', turn_end: 'model_turn_end',
  before_provider_request: 'before_provider_request', before_provider_headers: 'before_provider_headers', after_provider_response: 'after_provider_response',
  session_before_compact: 'session_before_compact', session_compact: 'session_compact', session_before_tree: 'session_before_tree', session_tree: 'session_tree',
};
export const notificationEvents = {
  'turn/started': 'agent_start', 'turn/settled': 'agent_end',
  'tool/started': 'tool_execution_start', 'tool/settled': 'tool_execution_end',
  'message/started': 'message_start', 'message/updated': 'message_update', 'message/settled': 'message_end',
  'compaction/failed': 'session_compact_failed',
  'session/info_changed': 'session_info_changed', 'dialog/started': 'ui_prompt_start', 'dialog/settled': 'ui_prompt_end',
  'model/selected': 'model_select', 'reasoning/selected': 'thinking_level_select', 'bash/user': 'user_bash',
};
// Pi events dispatched after another notification's event: the owning run settles once, after agent_end.
export const followingEvents = { 'turn/settled': 'agent_settled' };
function name(value, label) {
  bounded(value, label, 128);
  if (!/^[A-Za-z_][A-Za-z0-9_.:-]*$/.test(value)) invalid(label);
  return value;
}
function register(runtime, map, key, definition, factory) {
  runtime.assertFactory(factory);
  if (runtime.loaded) {
    const previous = map.get(key);
    if (!previous) unsupported('new runtime registration', 'this native catalog has no dynamic registration protocol');
    if (previous.factory !== factory) invalid(`registration ${key} belongs to another factory`);
    // Replacing local callbacks is live; changing native catalog metadata is not.
    // Refuse before touching the registry rather than claim an inert success.
    for (const field of ['description', 'usage', 'type', 'default']) {
      if (definition[field] !== previous.definition[field]) unsupported(`runtime registration ${key}.${field}`, 'native catalog metadata is immutable after initialization');
    }
  }
  if (!runtime.loaded && map.has(key)) invalid(`duplicate registration ${key}`);
  map.set(key, { definition, factory });
}
function gitBranch(cwd) {
  try {
    let git = join(cwd, '.git');
    if (!statSync(git).isDirectory()) {
      const value = readFileSync(git, 'utf8').trim();
      if (!value.startsWith('gitdir: ')) return undefined;
      git = resolve(cwd, value.slice(8));
    }
    const head = readFileSync(join(git, 'HEAD'), 'utf8').trim();
    return head.startsWith('ref: refs/heads/') ? head.slice(16) : undefined;
  } catch { return undefined; }
}
function snapshot(host, key, api) {
  if (!(key in host)) unsupported(api, `${key} snapshot not supplied by the host`);
  return host[key];
}
function humanizeExtensionName(value) {
  let name = value.trim();
  const parts = name.split('/');
  if (name.startsWith('@') && parts.length > 1) name = parts[1] === 'pi' ? parts[0].slice(1) : parts[1];
  else name = parts.at(-1);
  return name.replace(/([a-z0-9])([A-Z])/g, '$1 $2').replace(/[._-]+/g, ' ')
    .replace(/\b\w/g, character => character.toUpperCase());
}
export function extensionDisplayName(entry) {
  let directory = dirname(resolve(entry));
  for (;;) {
    try {
      const manifest = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
      const declared = manifest.pi?.displayName ?? manifest.displayName;
      if (typeof declared === 'string' && declared.trim()) return bounded(declared.trim(), 'extension display name', 128);
      if (typeof manifest.name === 'string' && manifest.name.trim()) return bounded(humanizeExtensionName(manifest.name), 'extension display name', 128);
    } catch {}
    const parent = dirname(directory);
    if (parent === directory || directory === parse(directory).root) break;
    directory = parent;
  }
  return bounded(humanizeExtensionName(basename(dirname(resolve(entry)))), 'extension display name', 128);
}
export function createAPI(runtime, factory) {
  const store = () => runtime.current(factory);
  const sessionStore = () => { const s = runtime.scope.getStore(); runtime.assertSessionOwner(s); return { ...s, factory }; };
  const op = (method, params, feature) => { runtime.require(feature); const s = store(); return runtime.track(runtime.hostCall(method, params, s), s); };
  // Pi's top-level API is an ordinary object: optional probes for non-Pi
  // members (e.g. unregisterTool) must see absence, not a compatibility error.
  // Known public members without a host implementation still refuse explicitly.
  const api = {
    ...mcpAPI(runtime, factory),
    registerTool(tool) {
      registerTool(runtime, factory, tool);
      if (runtime.initialized && runtime.features.has('transcript_render_v1')) runtime.transcript.invalidate(runtime.current(factory));
    },
    ...runtime.transcript.api(factory),
    registerProvider(name, config) { registerProvider(runtime, factory, name, config); },
    unregisterProvider(name) { unregisterProvider(runtime, factory, name); },
    registerVirtualModel() { unsupported('pi.registerVirtualModel'); },
    unregisterVirtualModel() { unsupported('pi.unregisterVirtualModel'); },
    registerCommand(key, definition) {
      name(key, 'command name'); fields(definition, ['description', 'handler', 'usage', 'getArgumentCompletions'], 'command');
      if (typeof definition.handler !== 'function') invalid('command handler');
      if (definition.getArgumentCompletions !== undefined && typeof definition.getArgumentCompletions !== 'function') invalid('command.getArgumentCompletions must be a function');
      if (definition.description !== undefined) bounded(definition.description, 'command description', 4096);
      if (runtime.loaded && definition.getArgumentCompletions) runtime.require('autocomplete');
      register(runtime, runtime.commands, key, definition, factory);
    },
    registerShortcut(key, definition) {
      fields(definition, ['description', 'handler'], 'shortcut'); bounded(key, 'shortcut key', 128);
      if (typeof definition.handler !== 'function') invalid('shortcut handler');
      if (key.split('+').includes('ctrl') && ['g', 'd'].includes(key.split('+').at(-1).toLowerCase())) unsupported(`shortcut ${key}`, 'reserved host rescue/shutdown control');
      register(runtime, runtime.shortcuts, key, definition, factory);
    },
    registerFlag(key, definition) {
      fields(definition, ['description', 'type', 'default'], 'flag');
      if (!/^[a-z][a-z0-9-]{0,63}$/.test(key) || !['boolean', 'string', 'integer'].includes(definition.type)) invalid('flag declaration');
      const defaultValue = definition.default === undefined ? ({ boolean: false, string: '', integer: 0 }[definition.type]) : definition.default;
      if ((definition.type === 'integer' ? !Number.isSafeInteger(defaultValue) : typeof defaultValue !== definition.type)) invalid('flag default');
      register(runtime, runtime.flags, key, { ...definition, default: defaultValue }, factory);
    },
    getFlag(key) {
      if (!runtime.flags.has(key)) invalid(`unknown flag ${key}`);
      return runtime.flagValues.has(key) ? runtime.flagValues.get(key) : runtime.flags.get(key).definition.default;
    },
    on(event, handler) {
      if (runtime.loaded && hookEvents[event] && !runtime.metadata().hooks.includes(hookEvents[event])) unsupported(`event ${event}`, 'native hook was not subscribed; configure again');
      if (typeof handler !== 'function') invalid('event handler');
      // A subscription to an event this host never emits is inert, as in Pi.
      if (event !== 'mcp_servers_change' && !hookEvents[event] && !Object.values(notificationEvents).includes(event) && !Object.values(followingEvents).includes(event)) {
        bounded(event, 'event name', 128);
        return () => {};
      }
      const list = runtime.events.get(event) || []; const entry = { handler, factory }; list.push(entry); runtime.events.set(event, list);
      return () => { const at = list.indexOf(entry); if (at >= 0) list.splice(at, 1); };
    },
    events: runtime.bus.facade(factory),
    exec: (command, args, options) => exec(runtime, store(), command, args, options),
    getSettings() { return getSettings(runtime, sessionStore()); },
    getSessionName() { return sessionStore().state.host.session_name ?? undefined; },
    setSessionName(value) {
      runtime.require('session_entries'); bounded(value, 'session name', 4096); const s = store(); s.controller.signal.throwIfAborted();
      const old = s.state.host.session_name, next = value || null; s.state.host.session_name = next;
      const result = op('session/set_name', { name: value }, 'session_entries');
      result.catch(() => { if (s.state.host.session_name === next) s.state.host.session_name = old; });
      return result;
    },
    appendEntry(type, data) { return runtime.appendEntry(type, data, sessionStore()); },
    setLabel(entryId, label) { bounded(entryId, 'entry id', 256); bounded(label, 'label', 4096); return op('session/set_label', { entry_id: entryId, label }, 'session_entries'); },
    sendUserMessage: (content, options) => op('session/send_user_message', userMessageParams(content, options), 'message_injection'),
    sendMessage: (message, options) => op('session/send_message', customMessageParams(message, options), 'message_injection'),
    getActiveTools() { return [...toolSnapshot(runtime, factory).active_tools]; },
    getAllTools() { return getAllTools(runtime, factory); },
    setActiveTools(names) { setActiveTools(runtime, factory, names); },
    getThinkingLevel() { return thinkingLevel(store().state.host); },
    setModel(model) { return setModel(runtime, store(), model); },
    setThinkingLevel(level) { setThinkingLevel(runtime, store(), level); },
    getCommands() { return [...runtime.commands].map(([name, d]) => ({ name, description: d.definition.description || name, source: 'extension' })); },
  };
  const methods = new Map();
  return new Proxy(api, {
    get(target, key, receiver) {
      const value = Reflect.get(target, key, receiver);
      if (typeof value !== 'function') return value;
      if (!methods.has(key)) methods.set(key, (...args) => { runtime.assertFactory(factory); return value(...args); });
      return methods.get(key);
    },
  });
}

export function textOnly(content) {
  if (!Array.isArray(content) || !content.length || content.length > 256) invalid('tool content must be a nonempty array');
  return content.map(part => {
    fields(part, ['type', 'text'], 'content part');
    if (part.type !== 'text') unsupported(`content ${part.type}`, 'publish media through a verified artifact contract, not raw Pi content');
    return { type: 'text', text: bounded(part.text, 'content text', 1048000, { controls: true }) };
  });
}

export function createContext(runtime, store, replaced = false) {
  const current = () => { runtime.assertSessionOwner(store); return store.state; };
  const foreground = () => { runtime.assertOwner(store); return store.state; };
  const operation = (method, params, feature) => { runtime.require(feature); return runtime.track(runtime.hostCall(method, params, store), store); };

  const footerData = facade({
    getGitBranch: () => foreground().host.git_branch ?? gitBranch(foreground().workspace),
    getExtensionStatuses: () => new Map(foreground().statuses),
    onBranchChange(handler) {
      if (typeof handler !== 'function') invalid('branch change listener');
      const s = foreground(), bound = () => runtime.scope.run(store, handler); s.branchListeners.add(bound); return () => s.branchListeners.delete(bound);
    },
  }, 'footerData');
  const setSlot = (slot, placement, factory, options) => {
    // Headless Pi has no chrome: a requested slot is simply not shown there.
    if (!runtime.features.has('remote_ui')) { foreground(); return Promise.resolve(); }
    runtime.require('remote_ui'); foreground();
    if (factory !== undefined && typeof factory !== 'function' && !Array.isArray(factory)) invalid(`${slot} factory`);
    const action = async () => {
      if (factory === undefined) return runtime.ui.clearSlot(store, slot);
      const make = Array.isArray(factory) ? () => ({ render: () => factory, invalidate() {} }) : factory;
      return runtime.ui.mount(store, placement, slot, slot === 'footer' ? (tui, t) => make(tui, t, footerData) : make, { slot, ...options });
    };
    const key = slot, prev = store.state.uiQueues.get(key) || Promise.resolve();
    const promise = prev.then(action); store.state.uiQueues.set(key, promise.catch(() => {}));
    return runtime.track(promise, store);
  };
  const custom = (factory, options = {}) => {
    runtime.require('remote_ui'); foreground(); fields(options, ['overlay', 'overlayOptions', 'onHandle'], 'ui.custom options');
    if (typeof factory !== 'function') invalid('ui.custom factory');
    if (options.overlay !== undefined && typeof options.overlay !== 'boolean') invalid('ui.custom overlay');
    if (options.onHandle !== undefined && typeof options.onHandle !== 'function') invalid('ui.custom onHandle');
    let finish, reject;
    const result = new Promise((resolve, fail) => { finish = resolve; reject = fail; });
    const mounting = runtime.ui.mount(store, 'fullscreen', 'Pi component', factory, { done: finish, reject, overlayOptions: options.overlay ? (options.overlayOptions ?? (() => undefined)) : undefined, onHandle: options.onHandle });
    runtime.track(mounting, store);
    return Promise.all([mounting, result]).then(([, value]) => value);
  };
  const ui = facade({
    theme: contextTheme(runtime, store),
    notify(message, type = 'info') {
      foreground(); store.controller.signal.throwIfAborted();
      if (!['info', 'success', 'warning', 'error'].includes(type)) invalid('notification type');
      const source = runtime.features.has('notification_source_v1') ? runtime.extensionNames?.get(store.factory) : undefined;
      return runtime.track(runtime.transport.notify('notification', { level: type, message: bounded(message, 'notification', 16384), ...(source ? { source } : {}) }), store);
    },
    ...dialogAPI(custom, {
      async confirm(title, message) {
        foreground();
        return (await runtime.hostCall('confirmation/request', { prompt: bounded(title, 'confirmation', 16384), detail: message === undefined ? null : bounded(message, 'confirmation detail', 16384), default: false, destructive: false }, store)).confirmed;
      },
      async input(title) {
        foreground();
        return (await runtime.hostCall('input/request', { prompt: bounded(title, 'input prompt', 16384), secret: false }, store)).value ?? undefined;
      },
    }),
    custom,
    onTerminalInput() { unsupported('ctx.ui.onTerminalInput', 'raw terminal key listening is not part of the host remote-UI contract'); },

    async editor(title, prefill = '') {
      bounded(title, 'editor title', 128); bounded(prefill, 'editor prefill', 262144);
      return custom((tui, t, _keys, done) => {
        const editor = new Editor(tui, { borderColor: t.borderColor, selectList: t.selectList });
        editor.setText(prefill); editor.onSubmit = done;
        const handle = editor.handleInput.bind(editor);
        editor.handleInput = data => { if (matchesKey(data, 'escape')) done(undefined); else handle(data); };
        return editor;
      });
    },
    setFooter: factory => setSlot('footer', 'footer', factory),
    setHeader: factory => setSlot('header', 'header', factory),
    setWidget(key, value, options = {}) {
      bounded(key, 'widget key', 64); fields(options, ['placement'], 'widget options');
      const placement = { aboveEditor: 'above_editor', belowEditor: 'below_editor' }[options.placement || 'aboveEditor'];
      if (!placement) invalid('widget placement');
      return setSlot(`widget:${key}`, placement, value);
    },
    ...editorAPI(runtime, store, setSlot),
    ...chromeAPI(runtime, store),
    onTerminalInput: handler => runtime.terminalInput.add(handler, store),
    setStatus(key, text) {
      bounded(key, 'status key', 64); const state = foreground();
      if (text === undefined) state.statuses.delete(`${store.factory}:${key}`); else state.statuses.set(`${store.factory}:${key}`, bounded(text, 'status', 4096, { controls: true }));
      for (const surface of runtime.ui.surfaces.values()) if (surface.store.state === state) surface.requestRender();
    },
    // Before the host's first editor-state snapshot the composer is empty, as
    // Pi's editor is at session_start; later snapshots keep this current.
    getEditorText: () => {
      const host = foreground().host;
      return 'composer_text' in host || !runtime.features.has('composer') ? snapshot(host, 'composer_text', 'ctx.ui.getEditorText') : '';
    },
    setEditorText(text) {
      runtime.require('composer'); store.controller.signal.throwIfAborted();
      bounded(text, 'composer text', 262144); const state = foreground();
      const checkpoint = runtime.ui.mutateEditor(store, text);
      if (checkpoint) return runtime.track(checkpoint, store);
      const old = state.host.composer_text; state.host.composer_text = text;
      const promise = operation('composer/set', { text }, 'composer');
      promise.catch(() => { if (state.host.composer_text === text) state.host.composer_text = old; }); return promise;
    },
    pasteToEditor(text) {
      runtime.require('composer'); store.controller.signal.throwIfAborted(); foreground();
      bounded(text, 'composer insert', 262144);
      const checkpoint = runtime.ui.mutateEditor(store, text, true);
      if (checkpoint) return runtime.track(checkpoint, store);
      // Without a custom component the cursor is host-owned; do not invent it.
      return operation('composer/insert', { text }, 'composer');
    },
  }, 'ctx.ui');
  // Lazy translation: a UI/resource-only factory must not fail merely because
  // an unrelated native history contains media this Pi message profile cannot map.
  const sessionState = () => {
    const state = current(), host = runtime.sessionFacts(store);
    // Native invalidation clears the entire mirror. A null history is not an
    // empty session and cannot leave old headers/labels visible after failure.
    if (host.session_entries === null || host.session_branch === null) unsupported('ctx.sessionManager', 'native session view unavailable');
    return { ...state, host };
  };
  const sessionEntries = key => withContextLimits(runtime.sessionTransport?.profile, () => translateSessionEntries(snapshot(sessionState().host, key, `ctx.sessionManager.${key}`), runtime.namespace));
  const sessionManager = strict({
    getCwd: () => sessionState().workspace,
    getSessionId: () => sessionState().host.session_id ?? undefined,
    getHeader: () => sessionHeader(sessionState().host) ?? null,
    getSessionName: () => sessionState().host.session_name ?? undefined,
    getEntries: () => sessionEntries('session_entries'),
    getBranch: fromId => {
      if (fromId === undefined) return sessionEntries('session_branch');
      const byId = new Map(sessionEntries('session_entries').map(entry => [entry.id, entry])), branch = [], seen = new Set();
      let entry = byId.get(fromId);
      while (entry) {
        if (seen.has(entry.id)) invalid('cyclic session ancestry');
        seen.add(entry.id); branch.push(entry); entry = byId.get(entry.parentId);
      }
      return branch.reverse();
    },
    getLeafEntry: () => sessionEntries('session_entries').find(entry => entry.id === sessionState().host.session_leaf_id),
    getLabel: id => snapshot(sessionState().host, 'session_labels', 'ctx.sessionManager.getLabel')[id],
    buildContextEntries: () => buildContextEntries(sessionEntries('session_entries'), sessionState().host.session_leaf_id),
    buildSessionProjection: () => buildSessionProjection(sessionEntries('session_entries'), sessionState().host.session_leaf_id),
    getTree: () => {
      const entries = sessionEntries('session_entries'), labels = snapshot(sessionState().host, 'session_labels', 'ctx.sessionManager.getTree');
      const nodes = new Map(entries.map(entry => [entry.id, { entry, children: [], label: labels[entry.id], labelTimestamp: undefined }])), roots = [];
      for (const entry of entries) {
        const node = nodes.get(entry.id), parent = nodes.get(entry.parentId);
        if (entry.parentId === null || entry.parentId === entry.id || !parent) roots.push(node); else parent.children.push(node);
      }
      for (const node of nodes.values()) node.children.sort((a, b) => new Date(a.entry.timestamp).getTime() - new Date(b.entry.timestamp).getTime());
      return roots;
    },
    getLeafId: () => snapshot(sessionState().host, 'session_leaf_id', 'ctx.sessionManager.getLeafId'),
    getSessionFile: () => snapshot(sessionState().host, 'session_file', 'ctx.sessionManager.getSessionFile'),
    getSessionDir: () => {
      const file = snapshot(sessionState().host, 'session_file', 'ctx.sessionManager.getSessionDir');
      if (file === null) unsupported('ctx.sessionManager.getSessionDir', 'no persistent session directory supplied by the host');
      return dirname(file);
    },
    getEntry: id => sessionEntries('session_entries').find(entry => entry.id === id),
  }, 'ctx.sessionManager');
  return facade({
    get cwd() { return current().workspace; },
    get sessionId() { return current().host.session_id ?? undefined; },
    get mode() { return contextFacts(runtime, store).mode; },
    isProjectTrusted() { return contextFacts(runtime, store).isProjectTrusted(); },
    getSystemPromptOptions() { return contextFacts(runtime, store).getSystemPromptOptions(); },
    get hasUI() { const state = current(); return runtime.features.has('remote_ui') && state === runtime.foreground && state.alive && state.host.has_ui !== false; },
    get model() { return currentModel(current().host); },
    get thinkingLevel() { return thinkingLevel(current().host, true); },
    get scopedModels() { return scopedModels(current().host); },
    ui, sessionManager,
    modelRegistry: modelRegistry(() => current().host, runtime),
    getContextUsage() {
      const host = current().host;
      if (Object.hasOwn(host, 'context_usage')) return host.context_usage === null ? undefined : structuredClone(host.context_usage);
      // Pi explicitly uses null for unavailable token/percentage estimates. The
      // host's model capacity is a real fact; do not invent token usage.
      if (host.model_view?.context_window) return { tokens: null, contextWindow: host.model_view.context_window, percent: null };
      return undefined;
    },
    isIdle: () => snapshot(current().host, 'is_idle', 'ctx.isIdle'),
    hasPendingMessages: () => snapshot(current().host, 'has_pending_messages', 'ctx.hasPendingMessages'),
    getSystemPrompt: () => snapshot(current().host, 'system_prompt', 'ctx.getSystemPrompt'),
    get signal() { return compactionCallbackStore(runtime, store).controller.signal; },
    waitForIdle() {
      if (store.method !== 'command/execute') unsupported('ctx.waitForIdle', 'requires a live command, never a hook waiting on its own run');
      return operation('session/wait_for_idle', { resource_owner: current().owner }, 'session_control_v1').then(result => {
        fields(result, ['session_id'], 'idle receipt'); bounded(result.session_id, 'idle session id', 256);
        if (result.session_id !== current().host.session_id) invalid('idle receipt session changed');
      });
    },
    compact: options => requestCompaction(runtime, store, options),
    abort() { unsupported('ctx.abort', 'host wire has no root-run abort contract'); },
    shutdown() { unsupported('ctx.shutdown', 'the extension process is host-owned; request shutdown through the host'); },
    ...sessionMethods(runtime, store, createContext),
    ...(replaced ? {
      sendMessage: (message, options) => operation('session/send_message', customMessageParams(message, options), 'message_injection'),
      sendUserMessage: (content, options) => operation('session/send_user_message', userMessageParams(content, options), 'message_injection'),
    } : {}),
  }, 'ctx');
}
