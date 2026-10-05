import { readFileSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { bounded, fields, invalid, plainJSON, strict, unsupported } from './errors.mjs';
import { theme } from './theme.mjs';
import { exec } from './exec.mjs';
import { mcpAPI } from './mcp.mjs';
import { chromeAPI, dialogAPI, editorAPI } from './ui-api.mjs';
import { currentModel, thinkingLevel, scopedModels, modelRegistry, registerProvider, unregisterProvider } from './providers.mjs';
import { setModel, setThinkingLevel } from './model-control.mjs';
import { registerTool, toolSnapshot, getAllTools, setActiveTools } from './tools.mjs';
import { Editor } from '../node_modules/@earendil-works/pi-tui/dist/components/editor.js';
import { translateSessionEntries } from './session-mirror.mjs';
import { compactionCallbackStore, requestCompaction } from './compaction.mjs';
import { matchesKey } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';
import { contextFacts, getSettings } from './context-api.mjs';
import { sessionMethods, sessionHeader } from './session-methods.mjs';
import { customMessageParams, userMessageParams } from './custom-messages.mjs';

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
  if (runtime.loaded && !map.has(key)) unsupported('new runtime registration', 'this native catalog has no dynamic registration protocol');
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
export function createAPI(runtime, factory) {
  const store = () => runtime.current(factory);
  const op = (method, params, feature) => { runtime.require(feature); const s = store(); return runtime.track(runtime.hostCall(method, params, s), s); };
  return strict({
    ...mcpAPI(runtime, factory),
    registerTool(tool) { registerTool(runtime, factory, tool); },
    ...runtime.transcript.api(factory),
    registerProvider(name, config) { registerProvider(runtime, factory, name, config); },
    unregisterProvider(name) { unregisterProvider(runtime, factory, name); },
    registerCommand(key, definition) {
      name(key, 'command name'); fields(definition, ['description', 'handler', 'usage', 'getArgumentCompletions'], 'command');
      if (typeof definition.handler !== 'function') invalid('command handler');
      if (definition.getArgumentCompletions !== undefined && typeof definition.getArgumentCompletions !== 'function') invalid('command.getArgumentCompletions must be a function');
      if (definition.description !== undefined) bounded(definition.description, 'command description', 4096);
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
      if (event !== 'mcp_servers_change' && !hookEvents[event] && !Object.values(notificationEvents).includes(event) && !Object.values(followingEvents).includes(event)) unsupported(`event ${event}`, 'no corresponding host event');
      if (typeof handler !== 'function') invalid('event handler');
      const list = runtime.events.get(event) || []; const entry = { handler, factory }; list.push(entry); runtime.events.set(event, list);
      return () => { const at = list.indexOf(entry); if (at >= 0) list.splice(at, 1); };
    },
    events: runtime.bus.facade(factory),
    exec: (command, args, options) => exec(runtime, store(), command, args, options),
    getSettings() { return getSettings(runtime, store()); },
    getSessionName() { return store().state.host.session_name ?? undefined; },
    setSessionName(value) {
      runtime.require('session_entries'); bounded(value, 'session name', 4096); const s = store(); s.controller.signal.throwIfAborted();
      const old = s.state.host.session_name, next = value || null; s.state.host.session_name = next;
      const result = op('session/set_name', { name: value }, 'session_entries');
      result.catch(() => { if (s.state.host.session_name === next) s.state.host.session_name = old; });
      return result;
    },
    appendEntry(type, data) { return runtime.appendEntry(type, data, store()); },
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
  }, 'pi');
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
  const current = () => { runtime.assertOwner(store); return store.state; };
  const operation = (method, params, feature) => { runtime.require(feature); return runtime.track(runtime.hostCall(method, params, store), store); };

  const footerData = strict({
    getGitBranch: () => current().host.git_branch ?? gitBranch(current().workspace),
    getExtensionStatuses: () => new Map(current().statuses),
    onBranchChange(handler) {
      if (typeof handler !== 'function') invalid('branch change listener');
      const s = current(), bound = () => runtime.scope.run(store, handler); s.branchListeners.add(bound); return () => s.branchListeners.delete(bound);
    },
  }, 'footerData');
  const setSlot = (slot, placement, factory, options) => {
    runtime.require('remote_ui'); current();
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
    runtime.require('remote_ui'); current(); fields(options, ['overlay', 'overlayOptions', 'onHandle'], 'ui.custom options');
    if (typeof factory !== 'function') invalid('ui.custom factory');
    if (options.overlay !== undefined && typeof options.overlay !== 'boolean') invalid('ui.custom overlay');
    if (options.onHandle !== undefined && typeof options.onHandle !== 'function') invalid('ui.custom onHandle');
    let finish, reject;
    const result = new Promise((resolve, fail) => { finish = resolve; reject = fail; });
    const mounting = runtime.ui.mount(store, 'fullscreen', 'Pi component', factory, { done: finish, reject, overlayOptions: options.overlay ? (options.overlayOptions ?? (() => undefined)) : undefined, onHandle: options.onHandle });
    runtime.track(mounting, store);
    return Promise.all([mounting, result]).then(([, value]) => value);
  };
  const ui = strict({
    theme,
    notify(message, type = 'info') {
      if (!['info', 'success', 'warning', 'error'].includes(type)) invalid('notification type');
      return runtime.track(runtime.transport.notify('notification', { level: type, message: bounded(message, 'notification', 16384) }), store);
    },
    ...dialogAPI(custom, {
      async confirm(title, message) {
        return (await runtime.hostCall('confirmation/request', { prompt: bounded(title, 'confirmation', 16384), detail: message === undefined ? null : bounded(message, 'confirmation detail', 16384), default: false, destructive: false }, store)).confirmed;
      },
      async input(title) {
        return (await runtime.hostCall('input/request', { prompt: bounded(title, 'input prompt', 16384), secret: false }, store)).value ?? undefined;
      },
    }),
    custom,

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
    setStatus(key, text) {
      bounded(key, 'status key', 64); const state = current();
      if (text === undefined) state.statuses.delete(`${store.factory}:${key}`); else state.statuses.set(`${store.factory}:${key}`, bounded(text, 'status', 4096, { controls: true }));
      for (const surface of runtime.ui.surfaces.values()) if (surface.store.state === state) surface.requestRender();
    },
    getEditorText: () => snapshot(current().host, 'composer_text', 'ctx.ui.getEditorText'),
    setEditorText(text) {
      runtime.require('composer'); store.controller.signal.throwIfAborted();
      bounded(text, 'composer text', 262144); const state = current();
      const checkpoint = runtime.ui.mutateEditor(store, text);
      if (checkpoint) return runtime.track(checkpoint, store);
      const old = state.host.composer_text; state.host.composer_text = text;
      const promise = operation('composer/set', { text }, 'composer');
      promise.catch(() => { if (state.host.composer_text === text) state.host.composer_text = old; }); return promise;
    },
    pasteToEditor(text) {
      runtime.require('composer'); store.controller.signal.throwIfAborted(); current();
      bounded(text, 'composer insert', 262144);
      const checkpoint = runtime.ui.mutateEditor(store, text, true);
      if (checkpoint) return runtime.track(checkpoint, store);
      // Without a custom component the cursor is host-owned; do not invent it.
      return operation('composer/insert', { text }, 'composer');
    },
  }, 'ctx.ui');
  // Lazy translation: a UI/resource-only factory must not fail merely because
  // an unrelated native history contains media this Pi message profile cannot map.
  const sessionEntries = key => translateSessionEntries(snapshot(current().host, key, `ctx.sessionManager.${key}`), runtime.namespace);
  const sessionManager = strict({
    getSessionId: () => current().host.session_id ?? undefined,
    getHeader: () => sessionHeader(current().host),
    getSessionName: () => current().host.session_name ?? undefined,
    getEntries: () => sessionEntries('session_entries'),
    getBranch: () => sessionEntries('session_branch'),
    getLeafId: () => snapshot(current().host, 'session_leaf_id', 'ctx.sessionManager.getLeafId'),
    getSessionFile: () => snapshot(current().host, 'session_file', 'ctx.sessionManager.getSessionFile'),
    getEntry: id => sessionEntries('session_entries').find(entry => entry.id === id),
  }, 'ctx.sessionManager');
  return strict({
    get cwd() { return current().workspace; },
    get mode() { return contextFacts(runtime, store).mode; },
    isProjectTrusted() { return contextFacts(runtime, store).isProjectTrusted(); },
    getSystemPromptOptions() { return contextFacts(runtime, store).getSystemPromptOptions(); },
    get hasUI() { return runtime.features.has('remote_ui') && current().alive; },
    get model() { return currentModel(current().host); },
    get thinkingLevel() { return thinkingLevel(current().host, true); },
    get scopedModels() { return scopedModels(current().host); },
    ui, sessionManager,
    modelRegistry: modelRegistry(() => current().host),
    getContextUsage() {
      const host = current().host;
      if (host.context_usage !== undefined) return host.context_usage;
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
    ...sessionMethods(runtime, store, createContext),
    ...(replaced ? {
      sendMessage: (message, options) => operation('session/send_message', customMessageParams(message, options), 'message_injection'),
      sendUserMessage: (content, options) => operation('session/send_user_message', userMessageParams(content, options), 'message_injection'),
    } : {}),
  }, 'ctx');
}
