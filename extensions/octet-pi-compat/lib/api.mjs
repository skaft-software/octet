import { readFileSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { bounded, fields, invalid, plainJSON, strict, unsupported } from './errors.mjs';
import { theme } from './theme.mjs';
import { Editor } from '../node_modules/@earendil-works/pi-tui/dist/components/editor.js';
import { translateSessionEntries } from './session-mirror.mjs';
import { compactionCallbackStore, requestCompaction } from './compaction.mjs';
import { matchesKey } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';

export const hookEvents = {
  session_start: 'session_start', session_end: 'session_end', session_shutdown: 'session_end',
  tool_call: 'before_tool_call', tool_result: 'after_tool_call', input: 'before_prompt',
  before_agent_start: 'before_prompt', after_response: 'after_response', resources_discover: 'resources_discover',
  context: 'provider_context', turn_start: 'model_turn_start', turn_end: 'model_turn_end',
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
function name(value, label) {
  bounded(value, label, 128);
  if (!/^[A-Za-z_][A-Za-z0-9_.:-]*$/.test(value)) invalid(label);
  return value;
}
function register(runtime, map, key, definition, factory) {
  if (runtime.loaded) unsupported('runtime registration', 'regenerate the reviewed static manifest and reload');
  if (map.has(key)) invalid(`duplicate registration ${key}`);
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
    registerTool(tool) {
      fields(tool, ['name', 'label', 'description', 'promptSnippet', 'promptGuidelines', 'parameters', 'execute', 'renderCall', 'renderResult', 'output_schema'], 'tool');
      name(tool.name, 'tool name'); bounded(tool.description, 'tool description', 4096);
      if (!tool.description || !tool.parameters || typeof tool.parameters !== 'object' || typeof tool.execute !== 'function') invalid('tool definition');
      if (tool.promptSnippet !== undefined) bounded(tool.promptSnippet, 'tool promptSnippet', 1024);
      if (tool.promptGuidelines !== undefined) {
        if (!Array.isArray(tool.promptGuidelines) || tool.promptGuidelines.length > 16) invalid('tool promptGuidelines must contain at most 16 strings');
        for (const value of tool.promptGuidelines) bounded(value, 'tool promptGuidelines entry', 1024);
      }
      if (tool.label !== undefined) bounded(tool.label, 'tool label', 128);
      for (const key of ['renderCall', 'renderResult']) if (tool[key] !== undefined && typeof tool[key] !== 'function') invalid(`tool.${key}`);
      register(runtime, runtime.tools, tool.name, tool, factory);
    },
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
      if (runtime.loaded) unsupported('runtime hook registration', 'regenerate the static manifest');
      if (!hookEvents[event] && !Object.values(notificationEvents).includes(event)) unsupported(`event ${event}`, 'no corresponding host event');
      if (typeof handler !== 'function') invalid('event handler');
      const list = runtime.events.get(event) || []; const entry = { handler, factory }; list.push(entry); runtime.events.set(event, list);
      return () => { const at = list.indexOf(entry); if (at >= 0) list.splice(at, 1); };
    },
    events: runtime.bus.facade(factory),
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
    sendUserMessage(text, options) {
      if (options !== undefined) unsupported('sendUserMessage options', 'delivery mode is owned by the host');
      return op('session/send_user_message', { text: bounded(text, 'user message', 262144) }, 'message_injection');
    },
    sendMessage(message, options = {}) {
      fields(options, ['triggerTurn'], 'sendMessage options');
      fields(message, ['role', 'content', 'customType', 'display', 'details'], 'message');
      const text = typeof message.content === 'string' ? message.content : textOnly(message.content).map(x => x.text).join('\n');
      if (message.role === 'user') {
        if (message.customType !== undefined || message.details !== undefined || message.display !== undefined || options.triggerTurn === false) unsupported('sendMessage user options');
        return op('session/send_user_message', { text: bounded(text, 'message', 262144) }, 'message_injection');
      }
      if (message.role || !message.customType) unsupported('sendMessage', 'no extension-authored assistant/system provider turns');
      if (message.display) unsupported('sendMessage display', 'host does not project custom entries into the transcript');
      const s = store(); runtime.require('session_entries');
      const promise = runtime.hostCall('session/append_entry', { entry_type: bounded(message.customType, 'customType', 128), data: plainJSON(message, 'custom message') }, s).then(() => {
        if (options.triggerTurn) { runtime.require('message_injection'); return runtime.hostCall('session/send_user_message', { text }, s); }
      });
      return runtime.track(promise, s);
    },
    getActiveTools() { return [...snapshot(store().state.host, 'active_tools', 'pi.getActiveTools')]; },
    getAllTools() { return [...snapshot(store().state.host, 'all_tools', 'pi.getAllTools')]; },
    setActiveTools(names) {
      if (!Array.isArray(names) || names.length > 256) invalid('active tools'); names.forEach(n => name(n, 'tool name'));
      runtime.require('active_tools'); const s = store(); s.controller.signal.throwIfAborted(); const old = s.state.host.active_tools, next = [...names]; s.state.host.active_tools = next;
      const promise = op('tools/set_active', { names }, 'active_tools'); promise.catch(() => { if (s.state.host.active_tools === next) s.state.host.active_tools = old; }); return promise;
    },
    getThinkingLevel() { const value = store().state.host.reasoning; return typeof value === 'string' ? value : value?.effort ?? (value?.type === 'off' ? 'off' : undefined); },
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

export function createContext(runtime, store) {
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
    const key = `${store.factory}:${slot}`, prev = store.state.uiQueues.get(key) || Promise.resolve();
    const promise = prev.then(action); store.state.uiQueues.set(key, promise.catch(() => {}));
    return runtime.track(promise, store);
  };
  const custom = (factory, options = {}) => {
    runtime.require('remote_ui'); current(); fields(options, ['overlay', 'overlayOptions', 'onHandle'], 'ui.custom options');
    if (typeof factory !== 'function') invalid('ui.custom factory');
    if (!options.overlay && (options.overlayOptions || options.onHandle)) invalid('overlay options require overlay=true');
    if (options.onHandle) unsupported('ui.custom.onHandle', 'use the TUI overlay handle supplied by showOverlay');
    let finish, reject;
    const result = new Promise((resolve, fail) => { finish = resolve; reject = fail; });
    const mounting = runtime.ui.mount(store, 'fullscreen', 'Pi component', factory, { done: finish, reject, overlayOptions: options.overlay ? (options.overlayOptions || {}) : undefined });
    runtime.track(mounting, store);
    return Promise.all([mounting, result]).then(([, value]) => value);
  };
  const ui = strict({
    theme,
    notify(message, type = 'info') {
      if (!['info', 'success', 'warning', 'error'].includes(type)) invalid('notification type');
      return runtime.track(runtime.transport.notify('notification', { level: type, message: bounded(message, 'notification', 16384) }), store);
    },
    async confirm(title, message, options) {
      if (options !== undefined) unsupported('ui.confirm options');
      return (await runtime.hostCall('confirmation/request', { prompt: bounded(title, 'confirmation', 16384), detail: message === undefined ? null : bounded(message, 'confirmation detail', 16384), default: false, destructive: false }, store)).confirmed;
    },
    async input(title, placeholder, options) {
      if (placeholder !== undefined || options !== undefined) unsupported('ui.input placeholder/options', 'host input wire has no placeholder/timeout');
      return (await runtime.hostCall('input/request', { prompt: bounded(title, 'input prompt', 16384), secret: false }, store)).value ?? undefined;
    },
    custom,
    async select(title, choices, options) {
      if (options !== undefined) unsupported('ui.select options');
      bounded(title, 'select title', 128);
      if (!Array.isArray(choices) || !choices.length || choices.length > 256) invalid('select choices');
      choices.forEach(value => bounded(value, 'select choice', 4096));
      return custom((tui, t, _keys, done) => {
        let index = 0;
        return { render: () => [t.bold(title), ...choices.map((value, i) => i === index ? t.fg('accent', `> ${value}`) : `  ${value}`)], invalidate() {},
          handleInput(data) {
            if (matchesKey(data, 'escape')) done(undefined);
            else if (matchesKey(data, 'enter')) done(choices[index]);
            else if (matchesKey(data, 'up')) { index = (index + choices.length - 1) % choices.length; tui.requestRender(); }
            else if (matchesKey(data, 'down')) { index = (index + 1) % choices.length; tui.requestRender(); }
          },
        };
      });
    },
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
    setEditorComponent: factory => setSlot('editor', 'editor', factory),
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
    getSessionName: () => current().host.session_name ?? undefined,
    getEntries: () => sessionEntries('session_entries'),
    getBranch: () => sessionEntries('session_branch'),
    getLeafId: () => snapshot(current().host, 'session_leaf_id', 'ctx.sessionManager.getLeafId'),
    getSessionFile: () => snapshot(current().host, 'session_file', 'ctx.sessionManager.getSessionFile'),
    getEntry: id => sessionEntries('session_entries').find(entry => entry.id === id),
  }, 'ctx.sessionManager');
  return strict({
    get cwd() { return current().workspace; },
    get hasUI() { return runtime.features.has('remote_ui') && current().alive; },
    get model() {
      const host = current().host;
      if (host.model_view) {
        const v = host.model_view;
        return { id: v.id, ...(v.name ? { name: v.name } : {}), api: v.api, provider: v.provider, reasoning: v.reasoning, input: v.input,
          contextWindow: v.context_window, maxTokens: v.max_tokens,
          ...(v.cost ? { cost: { input: v.cost.input / 1e6, output: v.cost.output / 1e6, cacheRead: v.cost.cache_read / 1e6, cacheWrite: v.cost.cache_write / 1e6 } } : {}),
        };
      }
      if (host.model_info) return host.model_info;
      return typeof host.model === 'object' ? host.model : host.model ? { id: host.model, name: host.model } : undefined;
    },
    ui, sessionManager,
    modelRegistry: strict({
      isUsingOAuth() { return snapshot(current().host, 'using_oauth', 'ctx.modelRegistry.isUsingOAuth'); },
      getAvailable: () => [...snapshot(current().host, 'available_models', 'ctx.modelRegistry.getAvailable')],
      getAll: () => [...snapshot(current().host, 'all_models', 'ctx.modelRegistry.getAll')],
      find: (provider, id) => snapshot(current().host, 'all_models', 'ctx.modelRegistry.find').find(m => m.provider === provider && m.id === id),
      getApiKey() { unsupported('ctx.modelRegistry.getApiKey', 'provider credentials never cross the extension boundary'); },
    }, 'ctx.modelRegistry'),
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
  }, 'ctx');
}
