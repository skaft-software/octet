import { bounded, fields, invalid, unsupported } from './errors.mjs';
import { matchesKey } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';
import { Text } from '../node_modules/@earendil-works/pi-tui/dist/components/text.js';
import { Input } from '../node_modules/@earendil-works/pi-tui/dist/components/input.js';
import { addAutocompleteProvider } from './completions.mjs';
import { safeLines } from './remote-ui.mjs';
import { theme, createNativeTheme, nativeTheme } from './theme.mjs';
import { Container } from '../node_modules/@earendil-works/pi-tui/dist/tui.js';
import { Spacer } from '../node_modules/@earendil-works/pi-tui/dist/components/spacer.js';
import { Loader } from '../node_modules/@earendil-works/pi-tui/dist/components/loader.js';
import { CancellableLoader } from '../node_modules/@earendil-works/pi-tui/dist/components/cancellable-loader.js';
import { getKeybindings } from '../node_modules/@earendil-works/pi-tui/dist/keybindings.js';
import { keyHint } from './keybinding-hints.mjs';
import { rendering } from './ui-delivery.mjs';

// Public Pi authoring helpers, adapted from Pi 1.0.2's interactive components.
// Copyright (c) 2025 Mario Zechner; MIT, see ../LICENSE.pi.
export class DynamicBorder {
  constructor(color = text => theme.fg('border', text)) { this.color = color; }
  invalidate() {}
  render(width) { return [this.color('─'.repeat(Math.max(1, width)))]; }
}
export function getSettingsListTheme() {
  return {
    label: (text, selected) => selected ? theme.fg('accent', text) : text,
    value: (text, selected) => theme.fg(selected ? 'accent' : 'muted', text),
    description: text => theme.fg('dim', text), cursor: theme.fg('accent', '→ '),
    hint: text => theme.fg('dim', text),
  };
}
export class BorderedLoader extends Container {
  constructor(tui, theme, message, options = {}) {
    super(); fields(options, ['cancellable'], 'BorderedLoader options');
    this.cancellable = options.cancellable ?? true;
    this.signalController = new AbortController();
    const border = text => theme.fg('border', text);
    this.addChild(new DynamicBorder(border));
    const LoaderClass = this.cancellable ? CancellableLoader : Loader;
    this.loader = new LoaderClass(tui, text => theme.fg('accent', text), text => theme.fg('muted', text), message);
    this.addChild(this.loader);
    if (this.cancellable) { this.addChild(new Spacer(1)); this.addChild(new Text(keyHint('tui.select.cancel', 'cancel'), 1, 0)); }
    this.addChild(new Spacer(1)); this.addChild(new DynamicBorder(border));
  }
  get signal() { return this.cancellable ? this.loader.signal : this.signalController.signal; }
  set onAbort(fn) { if (this.cancellable) this.loader.onAbort = fn; }
  handleInput(data) { if (this.cancellable) this.loader.handleInput(data); }
  dispose() { if (this.loader.dispose) this.loader.dispose(); else this.loader.stop(); }
}

// Pi's chrome setters are synchronous local state updates that can never fail or
// delay a hook (`ExtensionUIContext.setStatus/setFooter/...`:
// core/extensions/types.ts:181-226; the interactive implementation only mutates
// local state and calls `requestRender()`: modes/interactive/interactive-mode.ts
// :2401-2404, 2739-2752, 4657). Delivery to the host is therefore best-effort:
// one coalesced request per kind per tick, never awaited on a hook/turn stack,
// and a failure is one bounded diagnostic instead of a hook failure or a retry.
const chromeChannels = new WeakMap();
const CHROME_DELIVERY_TIMEOUT_MS = 2000;
const CHROME_READ_TIMEOUT_MS = 250;
function chromeChannel(state) {
  let channel = chromeChannels.get(state);
  if (!channel) chromeChannels.set(state, channel = { pending: new Map(), scheduled: false, reported: new Set(), expandedVersion: 0, expandedPending: false });
  return channel;
}

export function chromeAPI(runtime, store) {
  // Read the shell for native Ctrl+O changes, but preserve Pi's immediate
  // set/get behavior while an asynchronous setter is still being delivered.
  // If a read times out, retain the last observed local value.
  //
  // A host without an interactive frontend is Pi's print/headless mode: its UI
  // context still exists and presentation setters do nothing, because there is
  // no chrome to update. Refusing them made a factory that unconditionally
  // calls `ctx.ui.setWidget`/`setTitle` at session_start look like it needed an
  // unsupported API (pi-plan-mode). Theme *catalog* reads and selection stay
  // explicit refusals: they would have to invent a native catalog.
  const headless = () => !runtime.features.has('remote_ui');
  const headlessReceipt = operation => {
    const state = store.state;
    applyLocal(operation);
    if (['theme_get', 'theme_list', 'theme_set'].includes(operation.kind)) {
      unsupported(`ctx.ui.${operation.kind === 'theme_set' ? 'setTheme' : 'theme'}`, 'the native theme catalog requires an interactive frontend');
    }
    return { tools_expanded: state.host.tools_expanded === true };
  };
  const report = (kind, error) => {
    const channel = chromeChannel(store.state);
    if (runtime.stopping || channel.reported.has(kind)) return;
    channel.reported.add(kind);
    runtime.backgroundError(error);
  };
  // Local facts Pi keeps in its own UI state are applied here immediately, so a
  // getter after a setter is correct without a round trip.
  const applyLocal = operation => {
    if (operation.kind === 'tools_expanded') store.state.host.tools_expanded = operation.expanded;
  };
  const deliver = operation => {
    if (runtime.stopping || !store.state.alive) return;
    const channel = chromeChannel(store.state);
    const version = channel.expandedVersion;
    const settleExpanded = () => {
      if (operation.kind === 'tools_expanded' && channel.expandedVersion === version) channel.expandedPending = false;
    };
    let request;
    try {
      // No store controller signal and no `parent`: a chrome update outlives the
      // hook that requested it, exactly as Pi's local state does.
      request = runtime.transport.request('ui/chrome', { parent_request_id: store.id, resource_owner: store.state.owner, chrome: operation },
        { timeout: CHROME_DELIVERY_TIMEOUT_MS });
    } catch (error) { settleExpanded(); report(operation.kind, error); return; }
    request.then(result => {
      settleExpanded();
      if (['theme_get', 'theme_list', 'theme_set'].includes(operation.kind)) return;
      try {
        fields(result, ['tools_expanded', 'theme'], 'UI chrome receipt');
        if (typeof result.tools_expanded !== 'boolean') invalid('UI chrome tools_expanded receipt');
        if (result.theme != null) { nativeTheme(result.theme); store.state.host.theme = result.theme; }
      } catch (error) { report(operation.kind, error); }
    }, error => { settleExpanded(); report(operation.kind, error); });
  };
  const flush = () => {
    const channel = chromeChannel(store.state);
    if (channel.scheduled) return;
    channel.scheduled = true;
    setImmediate(() => {
      channel.scheduled = false;
      const operations = [...channel.pending.values()];
      channel.pending.clear();
      for (const operation of operations) deliver(operation);
    });
  };
  const queue = operation => {
    if (headless()) return headlessReceipt(operation);
    runtime.require('remote_ui');
    applyLocal(operation);
    const channel = chromeChannel(store.state);
    if (operation.kind === 'tools_expanded') { channel.expandedVersion++; channel.expandedPending = true; }
    channel.pending.set(operation.kind, operation);
    flush();
    return undefined;
  };
  // Values Pi reads synchronously need a host receipt. Disclosure reads are
  // bounded and read-only; theme selection retains its committed receipt.
  const call = operation => {
    runtime.assertOwner(store);
    store.controller.signal.throwIfAborted();
    if (headless()) return headlessReceipt(operation);
    runtime.require('remote_ui');
    const result = runtime.transport.requestSync('ui/chrome', {
      parent_request_id: store.id, resource_owner: store.state.owner, chrome: operation,
    }, { parent: store.live ? store.id : undefined, signal: store.controller.signal,
      ...(operation.kind === 'get' ? { timeout: CHROME_READ_TIMEOUT_MS } : {}) });
    applyLocal(operation);
    if (!['theme_get', 'theme_list', 'theme_set'].includes(operation.kind)) {
      fields(result, ['tools_expanded', 'theme'], 'UI chrome receipt');
      if (typeof result.tools_expanded !== 'boolean') invalid('UI chrome tools_expanded receipt');
      if (result.theme != null) { nativeTheme(result.theme); store.state.host.theme = result.theme; }
    }
    return result;
  };
  return {
    getTheme(name) {
      bounded(name, 'theme name', 128);
      const result = call({ kind: 'theme_get', name });
      fields(result, ['theme'], 'theme lookup receipt');
      return result.theme === null ? undefined : createNativeTheme(result.theme);
    },
    getAllThemes() {
      const result = call({ kind: 'theme_list' });
      fields(result, ['themes'], 'theme catalog receipt');
      if (!Array.isArray(result.themes)) invalid('theme catalog receipt');
      return result.themes.map(metadata => {
        fields(metadata, ['name', 'path'], 'theme catalog item');
        bounded(metadata.name, 'theme catalog name', 128);
        if (metadata.path != null) bounded(metadata.path, 'theme catalog path', 4096);
        return { name: metadata.name, path: metadata.path ?? undefined };
      });
    },
    setTheme(value) {
      // The approved wire admits native named palettes, not arbitrary uploaded
      // Theme objects. Never pretend an object's name selects its custom colors.
      if (typeof value !== 'string') return { success: false, error: 'Native theme selection requires an available theme name' };
      bounded(value, 'theme name', 128);
      const result = call({ kind: 'theme_set', name: value });
      fields(result, ['success', 'error', 'theme'], 'theme selection receipt');
      if (typeof result.success !== 'boolean') invalid('theme selection success');
      if (result.error !== undefined) bounded(result.error, 'theme selection error', 4096, { controls: true });
      if (!result.success) return { success: false, ...(result.error === undefined ? {} : { error: result.error }) };
      nativeTheme(result.theme);
      store.state.host.theme = structuredClone(result.theme);
      for (const surface of runtime.ui.surfaces.values()) if (surface.store.state === store.state) {
        runtime.scope.run(surface.store, () => { surface.tui.invalidate(); surface.requestRender(); });
      }
      return { success: true };
    },
    setTitle(title) { bounded(title, 'terminal title', 1024); queue({ kind: 'title', title }); },
    setWorkingMessage(message) {
      if (message !== undefined) bounded(message, 'working message', 4096);
      queue({ kind: 'working_message', message: message ?? null });
    },
    setWorkingVisible(visible) {
      if (typeof visible !== 'boolean') invalid('working visibility');
      queue({ kind: 'working_visible', visible });
    },
    setWorkingIndicator(options) {
      if (options !== undefined) fields(options, ['frames', 'intervalMs'], 'working indicator');
      if (options?.frames !== undefined) safeLines(options.frames);
      if (options?.intervalMs !== undefined && (!Number.isSafeInteger(options.intervalMs) || options.intervalMs < 0)) invalid('working interval');
      queue({ kind: 'working_indicator', frames: options?.frames ?? null, interval_ms: options?.intervalMs ?? null });
    },
    setHiddenThinkingLabel(label) {
      if (label !== undefined) bounded(label, 'hidden thinking label', 4096);
      queue({ kind: 'hidden_thinking', label: label ?? null });
    },
    getToolsExpanded() {
      runtime.assertOwner(store);
      store.controller.signal.throwIfAborted();
      // A setter is immediate from Pi's perspective, even before its coalesced
      // delivery, and a paint path never waits on the host. Otherwise (command
      // and hook handlers) query the shell so native Ctrl+O is observable.
      if (headless() || rendering() || chromeChannel(store.state).expandedPending) return store.state.host.tools_expanded === true;
      try {
        const expanded = call({ kind: 'get' }).tools_expanded;
        store.state.host.tools_expanded = expanded;
        return expanded;
      } catch (error) {
        store.controller.signal.throwIfAborted();
        report('tools_expanded_get', error);
        return store.state.host.tools_expanded === true;
      }
    },
    setToolsExpanded(expanded) {
      if (typeof expanded !== 'boolean') invalid('tools expanded');
      queue({ kind: 'tools_expanded', expanded });
    },
  };
}

const editorFactories = new WeakMap();

// Pi's dialog timer counts down whole seconds (ceil(timeout / 1000)). It is
// owned by the mounted component, so host rescue/retirement disposes it too.
function dialog(custom, title, options, make) {
  options ??= {};
  fields(options, ['signal', 'timeout'], 'dialog options');
  if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) invalid('dialog signal');
  if (options.timeout !== undefined && (!Number.isFinite(options.timeout) || options.timeout < 0)) invalid('dialog timeout');
  bounded(title, 'dialog title', 16384, { controls: true });
  if (options.signal?.aborted) return Promise.resolve(undefined);
  return custom((tui, theme, _keys, done) => {
    let remaining = options.timeout > 0 ? Math.ceil(options.timeout / 1000) : undefined;
    const heading = new Text('', 0, 0);
    const update = () => heading.setText(theme.fg('accent', theme.bold(
      remaining === undefined ? title : `${title} (${remaining}s)`)));
    const cancel = () => done(undefined);
    const body = make(tui, theme, done);
    update();
    const timer = remaining === undefined ? undefined : setInterval(() => {
      remaining--; update(); tui.requestRender();
      if (remaining <= 0) cancel();
    }, 1000);
    options.signal?.addEventListener('abort', cancel, { once: true });
    if (options.signal?.aborted) cancel();
    return {
      get focused() { return body.focused; },
      set focused(value) { body.focused = value; },
      render(width) { return [...heading.render(width), ...body.render(width)]; },
      invalidate() { heading.invalidate(); body.invalidate?.(); },
      handleInput(data) { if (matchesKey(data, 'escape')) cancel(); else body.handleInput(data); },
      dispose() { clearInterval(timer); options.signal?.removeEventListener('abort', cancel); body.dispose?.(); },
    };
  });
}

export function dialogAPI(custom, native = {}) {
  const simple = options => {
    if (options === undefined) return true;
    fields(options, ['signal', 'timeout'], 'dialog options');
    return Object.keys(options).length === 0;
  };
  const select = (title, choices, options) => {
    if (!Array.isArray(choices) || choices.length > 256) invalid('select choices');
    choices.forEach(value => bounded(value, 'select choice', 4096));
    return dialog(custom, title, options, (tui, theme, done) => {
      let index = 0;
      return {
        render(width) {
          return choices.flatMap((value, i) => new Text(i === index
            ? theme.fg('accent', `→ ${value}`) : `  ${theme.fg('text', value)}`, 0, 0).render(width));
        },
        invalidate() {},
        handleInput(data) {
          if (matchesKey(data, 'enter') || data === '\n') { if (choices[index]) done(choices[index]); }
          else if (matchesKey(data, 'up') || data === 'k') { index = Math.max(0, index - 1); tui.requestRender(); }
          else if (matchesKey(data, 'down') || data === 'j') { index = Math.min(choices.length - 1, index + 1); tui.requestRender(); }
        },
      };
    });
  };
  return {
    select,
    async confirm(title, message, options) {
      if (native.confirm && simple(options)) return native.confirm(title, message);
      bounded(message, 'confirmation message', 16384, { controls: true });
      return await select(`${title}\n${message}`, ['Yes', 'No'], options) === 'Yes';
    },
    input(title, placeholder, options) {
      // Pi 1.0.2 accepts this argument, but ExtensionInputComponent does not
      // render it or use it as the initial value.
      if (placeholder !== undefined) bounded(placeholder, 'input placeholder', 16384);
      if (native.input && simple(options)) return native.input(title);
      return dialog(custom, title, options, (_tui, _theme, done) => {
        const input = new Input();
        const handle = input.handleInput.bind(input);
        input.handleInput = data => {
          if (matchesKey(data, 'enter') || data === '\n') done(input.getValue()); else handle(data);
        };
        return input;
      });
    },
  };
}

export function editorAPI(runtime, store, setSlot) {
  return {
    getEditorComponent() { runtime.assertOwner(store); return editorFactories.get(store.state)?.factory; },
    setEditorComponent(factory) {
      runtime.assertOwner(store);
      // No interactive editor exists headless; mounting is a no-op there.
      if (!runtime.features.has('remote_ui')) return;
      if (factory !== undefined && typeof factory !== 'function') invalid('editor factory');
      const previous = editorFactories.get(store.state), next = { factory };
      editorFactories.set(store.state, next);
      // Invoke the original factory once, on the admitted real TUI. A headless
      // probe can duplicate effects and silently discard custom editor behavior.
      const mounted = setSlot('editor', 'editor', factory);
      mounted.catch(() => {
        if (editorFactories.get(store.state) === next) {
          if (previous) editorFactories.set(store.state, previous); else editorFactories.delete(store.state);
        }
      });
    },
    addAutocompleteProvider(factory) { addAutocompleteProvider(runtime, store, factory); },
  };
}
