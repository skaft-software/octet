import { bounded, fields, invalid, rpcError, strict, unsupported } from './errors.mjs';
import { keyData, mouseData, reservedKey } from './keys.mjs';
import { mouseModeIntent } from './transport.mjs';
import { theme, hostKeybindings } from './theme.mjs';
import { commandArgumentRequest, getAutocompleteProvider } from './completions.mjs';
import { piPosition } from './autocomplete-edits.mjs';
import { sliceByColumn, truncateToWidth, visibleWidth } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';

const CURSOR_MARKER = '\x1b_pi:c\x07';
const MAX_CAPTURE_READMISSIONS = 8;
const MAX_FRAME_ROWS = 256;
const MAX_FRAME_LINE_BYTES = 16384;
const MAX_FRAME_TEXT_BYTES = 524288;
const TRUNCATION_MARK = '…';
const SGR_RESET = '\x1b[0m';
const OSC8_START = '\x1b]8;';
const OSC8_MAX_URI_BYTES = 4096;
const OSC8_MAX_PARAMS_BYTES = 1024;
const OSC8_CONTROL = /[\x00-\x1f\x7f-\x9f]/u;

// Pi's TUI uses OSC 8 to make diagnostic locations clickable. The native
// renderer accepts only SGR, so admit a strictly bounded balanced link and
// drop its control wrappers while retaining the visible label.
function stripOsc8Links(line) {
  let output = '', cursor = 0, open = false;
  while (true) {
    const start = line.indexOf(OSC8_START, cursor);
    if (start < 0) break;
    output += line.slice(cursor, start);
    const payloadStart = start + OSC8_START.length;
    const bel = line.indexOf('\x07', payloadStart), st = line.indexOf('\x1b\\', payloadStart);
    const end = bel < 0 ? st : st < 0 ? bel : Math.min(bel, st);
    if (end < 0) invalid('ui/frame malformed OSC 8 hyperlink');
    const endingLength = end === bel ? 1 : 2;
    const payload = line.slice(start + OSC8_START.length, end);
    const separator = payload.indexOf(';');
    if (separator < 0) invalid('ui/frame malformed OSC 8 hyperlink');
    const params = payload.slice(0, separator), uri = payload.slice(separator + 1);
    if (Buffer.byteLength(params) > OSC8_MAX_PARAMS_BYTES || OSC8_CONTROL.test(params)) invalid('ui/frame OSC 8 parameters');
    if (uri === '') {
      if (params !== '' || !open) invalid('ui/frame unbalanced OSC 8 hyperlink');
      open = false;
    } else {
      if (open || Buffer.byteLength(uri) > OSC8_MAX_URI_BYTES || OSC8_CONTROL.test(uri)) invalid('ui/frame OSC 8 URI');
      let parsed;
      try { parsed = new URL(uri); } catch { invalid('ui/frame OSC 8 URI'); }
      if (!['file:', 'http:', 'https:'].includes(parsed.protocol) || (parsed.protocol !== 'file:' && !parsed.hostname)) invalid('ui/frame OSC 8 scheme');
      open = true;
    }
    cursor = end + endingLength;
  }
  if (open) invalid('ui/frame unbalanced OSC 8 hyperlink');
  return output + line.slice(cursor);
}
const basic = new Set([0, 1, 2, 3, 4, 5, 7, 8, 9, 21, 22, 23, 24, 25, 27, 28, 29, 39, 49, ...Array.from({ length: 8 }, (_, i) => 30 + i), ...Array.from({ length: 8 }, (_, i) => 40 + i), ...Array.from({ length: 8 }, (_, i) => 90 + i), ...Array.from({ length: 8 }, (_, i) => 100 + i)]);

function validatedLine(value) {
  if (typeof value !== 'string') invalid('component render must return string[]');
  // Pi's cursor marker is component metadata. Balanced, validated OSC 8 is
  // stripped above; all other controls (including other OSC) are refused.
  const line = stripOsc8Links(value.replaceAll(CURSOR_MARKER, ''));
  const text = line.replace(/\x1b\[([0-9;]*)m/g, (_match, body) => {
    if (body.length > 128 || !/^\d+(;\d+)*$/.test(body)) invalid('ui/frame SGR parameters');
    const raw = body.split(';'), codes = raw.map(Number);
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (basic.has(c)) continue;
      if ([38, 48].includes(c)) {
        const mode = raw[++i], n = mode === '2' ? 3 : mode === '5' ? 1 : 0;
        if (!n || i + n >= codes.length) invalid('ui/frame color SGR');
        for (let j = 0; j < n; j++) if (!Number.isInteger(codes[++i]) || codes[i] < 0 || codes[i] > 255) invalid('ui/frame RGB/index SGR');
      } else invalid(`ui/frame unsupported SGR ${c}`);
    }
    return '';
  });
  if (/[\x00-\x1f\x7f-\x9f\uD800-\uDFFF]/u.test(text)) invalid('ui/frame contains nonprintable data or unsafe escape');
  return line;
}

// Strict validation remains available for APIs that require a complete frame.
export function safeLines(lines) {
  if (!Array.isArray(lines) || lines.length > MAX_FRAME_ROWS) throw rpcError(-32602, 'bounds_exceeded ui/frame rows');
  let total = 0;
  return lines.map(value => {
    const line = validatedLine(value), bytes = Buffer.byteLength(line);
    total += bytes;
    if (bytes > MAX_FRAME_LINE_BYTES || total > MAX_FRAME_TEXT_BYTES) throw rpcError(-32602, 'bounds_exceeded ui/frame text');
    return line;
  });
}

function fitLine(line, maxBytes, forceMarker = false) {
  if (!forceMarker && Buffer.byteLength(line) <= maxBytes) return { line, truncated: false };
  if (maxBytes < Buffer.byteLength(TRUNCATION_MARK)) return { line: '', truncated: true };

  let output = '', bytes = 0, styled = false;
  const tokens = /\x1b\[[0-9;]*m|./gsu;
  let match;
  while ((match = tokens.exec(line)) !== null) {
    const token = match[0], tokenBytes = Buffer.byteLength(token);
    const nextStyled = styled || token.startsWith('\x1b[');
    const suffixBytes = Buffer.byteLength(TRUNCATION_MARK) + (nextStyled ? Buffer.byteLength(SGR_RESET) : 0);
    if (bytes + tokenBytes + suffixBytes > maxBytes) break;
    output += token; bytes += tokenBytes; styled = nextStyled;
  }
  return { line: `${output}${styled ? SGR_RESET : ''}${TRUNCATION_MARK}`, truncated: true };
}

// Pi components can render much more text than Octet's bounded snapshot wire,
// especially when they emit truecolor SGR for every cell. Clip presentation at
// safe SGR/Unicode boundaries instead of retiring an otherwise valid UI mount.
export function fitLines(lines) {
  if (!Array.isArray(lines)) invalid('component render must return string[]');
  const result = [];
  let total = 0, needsMarker = lines.length > MAX_FRAME_ROWS, lastIsMarked = false;
  const count = Math.min(lines.length, MAX_FRAME_ROWS);
  for (let index = 0; index < count; index++) {
    const line = validatedLine(lines[index]);
    const remaining = MAX_FRAME_TEXT_BYTES - total;
    if (remaining <= 0) { needsMarker = true; break; }
    const limit = Math.min(MAX_FRAME_LINE_BYTES, remaining);
    const fitted = fitLine(line, limit);
    result.push(fitted.line);
    total += Buffer.byteLength(fitted.line);
    lastIsMarked = fitted.truncated && fitted.line.endsWith(TRUNCATION_MARK);
    if (fitted.truncated && limit < MAX_FRAME_LINE_BYTES) { needsMarker = true; break; }
  }
  if (needsMarker && !lastIsMarked) {
    let retainedBytes = total;
    while (result.length) {
      const lastIndex = result.length - 1, lastBytes = Buffer.byteLength(result[lastIndex]);
      const previousBytes = retainedBytes - lastBytes;
      const limit = Math.min(MAX_FRAME_LINE_BYTES, MAX_FRAME_TEXT_BYTES - previousBytes);
      const marked = fitLine(result[lastIndex], limit, true).line;
      if (marked.endsWith(TRUNCATION_MARK)) { result[lastIndex] = marked; break; }
      result.pop(); retainedBytes = previousBytes;
    }
  }
  return result;
}
function dimension(value) {
  if (!Number.isInteger(value) || value <= 0 || value > 65535) invalid('ui geometry');
  return value;
}
function component(value) {
  if (!value || typeof value.render !== 'function') invalid('factory must return a Component');
  return value;
}
function size(value, reference, fallback) {
  if (value === undefined) return fallback;
  if (Number.isFinite(value)) return Math.floor(value);
  if (typeof value === 'string' && /^\d+(\.\d+)?%$/.test(value)) return Math.floor(reference * parseFloat(value) / 100);
  invalid('overlay size');
}

const editorSurfaces = new WeakMap();
const nativeEditors = new WeakMap();

// Internal component binding only; native handles still pass the admitted
// surface's owner/mount checks on every synchronous call.
export function registerNativeEditor(component, call, id) {
  const binding = {call, id, checkpoint: undefined};
  nativeEditors.set(component, binding);
  return binding;
}


// The native editor constructor receives only this admitted surface's fence.
// No terminal, ambient runtime, or extension-created owner can authorize it.
export function nativeEditorCall(tui) {
  const surface = editorSurfaces.get(tui);
  if (!surface) unsupported('native Editor', 'requires the host-supplied TUI');
  const call = (operation, editorId) => {
    if (operation.op === 'dispose' && surface.closed) return {};
    if (surface.closed || !surface.opened || surface.runtime.stopping) throw rpcError(-32002, 'native editor surface retired');
    const runtime = surface.runtime, store = surface.store;
    runtime.require('remote_ui'); runtime.assertOwner(store);
    store.controller.signal.throwIfAborted();
    return runtime.transport.requestSync('ui/chrome', {
      parent_request_id: store.id, resource_owner: store.state.owner,
      chrome: { kind: 'editor', surface_id: surface.id,
        ...(surface.placement === 'editor' ? {mount_id: surface.mountId} : {}),
        ...(editorId === undefined ? {} : {editor_id: editorId}), operation },
    }, {parent: store.live ? store.id : undefined, signal: store.controller.signal});
  };
  call.isAlive = () => !surface.closed && surface.opened && !surface.runtime.stopping;
  return call;
}

export class RemoteTUI {
  constructor(surface) {
    this.surface = surface; this.children = []; this.focus = null; this.overlays = []; this.listeners = new Set();
    this.terminal = strict({
      get columns() { return surface.columns; }, get rows() { return surface.rows; },
      get geometry() { return { columns: surface.columns, rows: surface.rows }; },
      write(data) {
        const intent = mouseModeIntent(data);
        if (intent === undefined) unsupported('tui.terminal.write', 'only reviewed mouse-capture intent is admitted; Rust owns all output');
        surface.runtime.scope.run(surface.store, () => surface.runtime.mouseIntent(intent));
      },
      start() { unsupported('tui.terminal.start', 'Rust owns terminal input'); },
      stop() { unsupported('tui.terminal.stop', 'Rust owns terminal lifetime'); },
    }, 'tui.terminal');
    this.facade = strict(this, 'tui');
    editorSurfaces.set(this.facade, surface);
  }
  requestRender() { this.surface.requestRender(); }
  addChild(value) { this.children.push(component(value)); this.requestRender(); }
  removeChild(value) { const at = this.children.indexOf(value); if (at >= 0) this.children.splice(at, 1); if (this.focus === value) this.setFocus(null); this.requestRender(); }
  clear() { this.children.length = 0; this.setFocus(null); this.requestRender(); }
  invalidate() { for (const c of this.children) c.invalidate?.(); for (const entry of this.overlays) entry.component.invalidate?.(); }
  setFocus(value) { if (this.focus && 'focused' in this.focus) this.focus.focused = false; this.focus = value; if (value && 'focused' in value) value.focused = true; this.requestRender(); }
  addInputListener(listener) { if (typeof listener !== 'function') invalid('input listener'); this.listeners.add(listener); return () => this.listeners.delete(listener); }
  showOverlay(value, options = {}) {
    fields(options, ['width', 'minWidth', 'maxHeight', 'anchor', 'offsetX', 'offsetY', 'row', 'col', 'margin', 'visible', 'nonCapturing'], 'overlay');
    const entry = { component: component(value), options, hidden: false, before: this.focus };
    this.overlays.push(entry);
    if (!options.nonCapturing) this.setFocus(value);
    const remove = () => {
      const at = this.overlays.indexOf(entry); if (at < 0) return;
      this.overlays.splice(at, 1);
      if (this.focus === value) this.setFocus(this.topOverlay()?.component ?? entry.before);
      this.requestRender();
    };
    this.requestRender();
    return {
      hide: remove, setHidden: hidden => { if (!this.overlays.includes(entry)) return; entry.hidden = Boolean(hidden); if (hidden && this.focus === value) this.setFocus(entry.before); else if (!hidden && !options.nonCapturing) this.setFocus(value); this.requestRender(); },
      isHidden: () => entry.hidden || !this.overlays.includes(entry),
      focus: () => { if (this.overlays.includes(entry) && !entry.hidden) { this.overlays.splice(this.overlays.indexOf(entry), 1); this.overlays.push(entry); this.setFocus(value); } },
      unfocus: opts => { if (opts) fields(opts, ['target'], 'overlay.unfocus'); this.setFocus(opts ? opts.target : entry.before); },
      isFocused: () => this.focus === value,
    };
  }
  topOverlay() { return this.overlays.findLast(e => !e.hidden && !e.options.nonCapturing && (!e.options.visible || e.options.visible(this.surface.columns, this.surface.rows))); }
  hideOverlay() { const entry = this.overlays.pop(); if (entry) { this.setFocus(entry.before); this.requestRender(); } }
  hasOverlay() { return this.overlays.some(e => !e.hidden); }
  input(data, release) {
    for (const listener of this.listeners) {
      const result = listener(data);
      if (result?.consume) return;
      if (result?.data !== undefined) data = result.data;
    }
    const overlay = this.overlays.find(entry => entry.component === this.focus);
    const focus = overlay && (overlay.hidden || overlay.options.visible && !overlay.options.visible(this.surface.columns, this.surface.rows)) ? null : this.focus;
    if (release && !focus?.wantsKeyRelease) return;
    focus?.handleInput?.(data);
    this.requestRender();
  }
  render(width) {
    let lines = this.children.flatMap(c => fitLines(c.render(width)));
    for (const e of this.overlays) {
      if (e.hidden || e.options.visible && !e.options.visible(width, this.surface.rows)) continue;
      const o = e.options, margin = typeof o.margin === 'number' ? { top: o.margin, right: o.margin, bottom: o.margin, left: o.margin } : (o.margin || {});
      const left = margin.left || 0, top = margin.top || 0;
      const availW = Math.max(1, width - left - (margin.right || 0)), availH = Math.max(1, this.surface.rows - top - (margin.bottom || 0));
      const w = Math.min(availW, Math.max(1, o.minWidth || 1, size(o.width, width, Math.min(80, availW))));
      const overlay = fitLines(e.component.render(w)).slice(0, Math.min(availH, size(o.maxHeight, this.surface.rows, availH)));
      const anchor = o.anchor || 'center';
      if (!['center', 'top-left', 'top-right', 'bottom-left', 'bottom-right', 'top-center', 'bottom-center', 'left-center', 'right-center'].includes(anchor)) invalid('overlay anchor');
      const xDefault = left + (anchor.includes('left') ? 0 : anchor.includes('right') ? availW - w : Math.floor((availW - w) / 2));
      const yDefault = top + (anchor.startsWith('top') ? 0 : anchor.startsWith('bottom') ? availH - overlay.length : Math.floor((availH - overlay.length) / 2));
      const x = Math.max(left, Math.min(width - w, size(o.col, width - w, xDefault) + (o.offsetX || 0)));
      const y = Math.max(top, Math.min(this.surface.rows - overlay.length, size(o.row, this.surface.rows - overlay.length, yDefault) + (o.offsetY || 0)));
      while (lines.length < y + overlay.length) lines.push('');
      for (let row = 0; row < overlay.length; row++) {
        const base = lines[y + row] || '';
        const prefix = sliceByColumn(base, 0, x), suffix = sliceByColumn(base, x + w, width - x - w);
        const content = truncateToWidth(overlay[row], w, '');
        lines[y + row] = prefix + ' '.repeat(Math.max(0, x - visibleWidth(prefix))) + '\x1b[0m' + content + ' '.repeat(Math.max(0, w - visibleWidth(content))) + '\x1b[0m' + suffix;
      }
    }
    return fitLines(lines);
  }
  start() { unsupported('tui.start', 'Rust owns the terminal'); }
  stop() { unsupported('tui.stop', 'close the host surface with done instead'); }
}

export class RemoteUI {
  constructor(runtime) { this.runtime = runtime; this.surfaces = new Map(); this.slots = new Map(); this.counter = 0; }
  async mount(store, placement, title, factory, { done, reject, slot, overlayOptions, onHandle } = {}) {
    this.runtime.require('remote_ui'); this.runtime.assertOwner(store);
    if (this.surfaces.size >= 16) throw rpcError(-32012, 'bounds_exceeded remote surfaces');
    if (slot) await this.clearSlot(store, slot);
    const surface = {
      id: `pi-${++this.counter}`, runtime: this.runtime, placement, store, closed: false, opened: false, revision: 0, scheduled: false,
      columns: 80, rows: 24, resolve: done, reject, ready: false, dirty: false, initialInputs: [],
      phase: 'admitting', desiredMouseCapture: Boolean(store.mouseCapture), requestedMouseCapture: false, admittedMouseCapture: false,
    };
    surface.requestRender = () => {
      if (surface.closed || !surface.ready) return;
      surface.dirty = true;
      if (surface.scheduled) return;
      surface.scheduled = true;
      setImmediate(async () => {
        surface.dirty = false;
        try {
          if (!surface.closed && surface.store.state.alive) await this.runtime.scope.run(surface.store, () => this.push(surface));
        } catch (error) {
          this.runtime.backgroundError(error); this.close(surface).catch(e => this.runtime.backgroundError(e));
        } finally {
          surface.scheduled = false;
          if (surface.dirty) surface.requestRender();
        }
      });
    };
    surface.store = { ...store, surface };
    surface.tui = new RemoteTUI(surface);
    this.surfaces.set(surface.id, surface);
    if (slot) { surface.slotKey = `${store.state.key}:${slot}`; this.slots.set(surface.slotKey, surface); }
    try {
      const geometry = await this.admit(surface, title, Boolean(store.mouseCapture));
      if (!geometry || surface.closed) return surface;
      if (placement === 'editor') {
        if (!geometry.editor_mount_id) unsupported('editor checkpoint', 'host ui/open must supply editor_mount_id');
        if (typeof geometry.editor_mount_id !== 'string' || !/^[A-Za-z0-9_.-]{1,64}$/.test(geometry.editor_mount_id)) invalid('editor_mount_id');
        surface.mountId = geometry.editor_mount_id;
        // A notification can arrive before the open reply's JS continuation.
        // Input never establishes its own mount identity.
        if (surface.initialInputs.some(input => input.editor_input.mount_id !== surface.mountId)) invalid('editor input mount/revision mismatch');
      }
      surface.phase = 'constructing';
      const finish = value => this.close(surface, value).catch(error => this.runtime.backgroundError(error));
      const editorTheme = placement === 'editor' ? {
        borderColor: text => theme.getThinkingBorderColor(surface.store.state.host.reasoning ?? 'off')(text),
        selectList: theme.selectList,
      } : theme;
      surface.component = await this.runtime.scope.run(surface.store, () => factory(surface.tui.facade, editorTheme, hostKeybindings(() => surface.store.state.host.keybindings), finish));
      component(surface.component);
      if (surface.closed) { this.runtime.scope.run(surface.store, () => surface.component.dispose?.()); return surface; }
      // Geometry is host-issued before construction. Each open freezes only its
      // request; intent may still change while either close or open awaits ACK.
      // Reconcile before activation, with bounded churn and no intermediate frame.
      surface.phase = 'admitting';
      let readmissions = 0;
      while (surface.desiredMouseCapture !== surface.admittedMouseCapture) {
        if (++readmissions > MAX_CAPTURE_READMISSIONS) throw rpcError(-32012, 'bounds_exceeded mouse capture admission changes');
        await this.releaseMount(surface);
        if (surface.closed) return surface;
        if (!await this.admit(surface, title, surface.desiredMouseCapture) || surface.closed) return surface;
      }
      if (overlayOptions !== undefined) {
        const resolved = typeof overlayOptions === 'function' ? this.runtime.scope.run(surface.store, overlayOptions) : overlayOptions;
        const handle = surface.tui.showOverlay(surface.component, resolved ?? (surface.component.width ? { width: surface.component.width } : {}));
        if (onHandle) this.runtime.scope.run(surface.store, () => onHandle(handle));
      } else { surface.tui.children.push(surface.component); surface.tui.setFocus(surface.component); }
      if (placement === 'editor') await this.bindEditor(surface);
      if (surface.closed) return surface;
      surface.phase = 'active'; surface.ready = true;
      // First delivery, not recovery replay: seed/bind first, then dispatch each
      // fenced host event exactly once. push captures the resulting ACK tail.
      for (const input of surface.initialInputs.splice(0)) {
        if (surface.closed) break;
        this.editorInput(surface, input);
      }
      await this.push(surface);
      if (placement === 'fullscreen' && store.method === 'command/execute' && store.detach) store.detach();
      return surface;
    } catch (error) { await this.close(surface, undefined, !surface.opened).catch(e => this.runtime.backgroundError(e)); throw error; }
  }
  async admit(surface, title, capture) {
    surface.requestedMouseCapture = capture;
    const geometry = await this.runtime.hostCall('ui/open', {
      surface_id: surface.id, title, placement: surface.placement, mouse_capture: capture,
    }, surface.store);
    // Observed retirement already restored the host lease; never revive it or
    // send a duplicate close because an earlier successful ACK arrived late.
    if (surface.observedClose) return null;
    if (!surface.store.state.alive || this.runtime.stopping) { await this.close(surface, undefined, true); return null; }
    // Record admission before validating geometry, so malformed success also
    // gets cleaned up. Local done/cancel still needs a close after a late ACK.
    surface.opened = true; surface.admittedMouseCapture = capture;
    if (surface.closed) { await this.releaseMount(surface); return null; }
    surface.columns = dimension(geometry.columns); surface.rows = dimension(geometry.rows);
    return geometry;
  }
  releaseMount(surface) {
    if (surface.releasing) return surface.releasing;
    if (!surface.opened || surface.observedClose || !surface.store.state.alive || this.runtime.stopping) return;
    // Claim the close synchronously. done/cancel during this ACK cannot send a
    // second close or open a replacement before this release has settled.
    surface.opened = false; surface.admittedMouseCapture = false;
    surface.releasing = Promise.resolve().then(() => this.runtime.hostCall('ui/close',
      { surface_id: surface.id }, { ...surface.store, controller: new AbortController() }))
      .finally(() => { surface.releasing = null; });
    return surface.releasing;
  }
  async bindEditor(surface) {
    const c = surface.component, store = surface.store;
    this.runtime.require('composer');
    if (typeof c.setText !== 'function' || typeof c.getText !== 'function') unsupported('editor component', 'requires getText/setText');
    const native = nativeEditors.get(c);
    if (native) native.call({op: 'bind'}, native.id);
    const { text } = await this.runtime.hostCall('composer/get', {}, store);
    if (surface.closed) return;
    store.state.host.composer_text = text; c.setText(text);
    if (this.runtime.features.has('autocomplete')) {
      // Octet's slash popup is the one command registry for the composer slot:
      // native, Octet-extension and Pi commands. Pi's own top-level list would
      // be a second popup beside it. Argument, attachment and extension
      // completions still come from the Pi provider.
      const provider = getAutocompleteProvider(this.runtime, store);
      c.setAutocompleteProvider?.({
        ...provider,
        getSuggestions(lines, cursorLine, cursorCol, options) {
          const line = String(lines?.[cursorLine] ?? '').slice(0, cursorCol).trimStart();
          if (line.startsWith('/') && !line.includes(' ')) return null;
          if (native) {
            const {text, index} = piPosition(lines, cursorLine, cursorCol);
            if (commandArgumentRequest({text, cursor: Buffer.byteLength(text.slice(0, index)), revision: 0})) return null;
          }
          return provider.getSuggestions(lines, cursorLine, cursorCol, options);
        },
      });
    }
    const change = c.onChange;
    const editor = surface.editor = { pending: 0, mutating: 0, inputRevision: 0, checkpointRevision: 0,
      retired: false, tail: Promise.resolve(), changeRevision: 0 };
    const ordered = action => {
      if (editor.pending >= 128) throw rpcError(-32012, 'bounds_exceeded editor update queue');
      editor.pending++;
      // Draft updates and submission are lossless, ordered effects, not frames.
      // A failed checkpoint prevents later updates from claiming a successful
      // handoff; the runtime surfaces the refusal rather than replaying it.
      const promise = editor.tail.then(() => {
        if (editor.retired) throw rpcError(-32002, 'editor mount retired');
        return action();
      }).finally(() => { editor.pending--; });
      editor.tail = promise;
      return this.runtime.track(promise, store);
    };
    editor.checkpoint = () => {
      if (surface.closed) throw rpcError(-32002, 'editor mount retired');
      const text = bounded(c.getExpandedText?.() ?? c.getText(), 'editor draft', 262144);
      const checkpoint = { surface_id: surface.id, mount_id: surface.mountId,
        input_revision: editor.inputRevision, checkpoint_revision: ++editor.checkpointRevision };
      if (!Number.isSafeInteger(checkpoint.checkpoint_revision)) invalid('editor checkpoint revision exhausted');
      store.state.host.composer_text = text;
      return ordered(async () => {
        const ack = await this.runtime.hostCall('composer/set', { text, editor_checkpoint: checkpoint }, store);
        fields(ack, ['input_revision', 'checkpoint_revision'], 'editor checkpoint acknowledgement');
        if (ack.input_revision !== checkpoint.input_revision || ack.checkpoint_revision !== checkpoint.checkpoint_revision) {
          invalid('editor checkpoint acknowledgement mismatch');
        }
      });
    };
    if (native) native.checkpoint = () => {if (!surface.closed && !editor.mutating) editor.checkpoint();};
    // The host owns submit, follow-up, clear and close for the composer slot;
    // the component answers host text decisions with checkpoints only.
    c.onChange = value => {
      if (surface.closed) return;
      store.state.host.composer_text = value;
      editor.changeRevision++;
      if (!editor.mutating) editor.checkpoint();
      change?.(value);
    };
    // Custom setText may normalize the native seed before its first frame.
    if (c.getText() !== text) await editor.checkpoint();
  }
  activeEditor(store) {
    this.runtime.assertOwner(store);
    return [...this.surfaces.values()].find(s => s.placement === 'editor' && s.store.state === store.state && !s.closed);
  }
  mutateEditor(store, text, insert = false) {
    const surface = this.activeEditor(store);
    if (!surface) return null;
    if (!surface.editor) unsupported('editor mutation', 'custom editor is still mounting');
    const method = insert ? 'insertTextAtCursor' : 'setText';
    if (typeof surface.component[method] !== 'function') unsupported(`editor.${method}`);
    const editor = surface.editor;
    editor.mutating++;
    try { this.runtime.scope.run(surface.store, () => surface.component[method](text)); }
    catch (error) { this.close(surface).catch(e => this.runtime.backgroundError(e)); throw error; }
    finally { editor.mutating--; }
    const checkpoint = editor.checkpoint();
    surface.requestRender();
    return checkpoint;
  }
  editorInput(surface, params) {
    const editor = surface.editor;
    try {
      const issued = params.editor_input;
      if (!issued) unsupported('editor input checkpoint', 'host must supply editor_input');
      fields(issued, ['mount_id', 'input_revision'], 'editor_input');
      const previous = surface.initialInputs.at(-1)?.editor_input.input_revision ?? editor?.inputRevision ?? 0;
      if (typeof issued.mount_id !== 'string' || !/^[A-Za-z0-9_.-]{1,64}$/.test(issued.mount_id)
          || surface.mountId !== undefined && issued.mount_id !== surface.mountId
          || !Number.isSafeInteger(issued.input_revision) || issued.input_revision !== previous + 1) invalid('editor input mount/revision mismatch');
      bounded(params.key, 'editor key', 32, { controls: true });
      const data = keyData(params);
      if (!surface.ready) {
        if (surface.initialInputs.length >= 128) throw rpcError(-32012, 'bounds_exceeded editor initial input queue');
        // Keep only bounded normalized fields, never a raw terminal byte stream,
        // arbitrary notification payload, or an acknowledged/replayed input.
        surface.initialInputs.push({ key: params.key, kind: params.kind, modifiers: [...(params.modifiers ?? [])], editor_input: { ...issued } });
        return;
      }
      editor.mutating++;
      try { surface.tui.input(data, params.kind === 'release'); } finally { editor.mutating--; }
      if (!surface.closed) {
        // Intermediate onChange calls cannot acknowledge an unfinished input.
        editor.inputRevision = issued.input_revision;
        editor.checkpoint(); // Includes cursor, consumed, no-op and release events.
        surface.requestRender();
      }
    } catch (error) {
      this.close(surface).catch(e => this.runtime.backgroundError(e));
      throw error;
    }
  }
  async push(surface) {
    if (surface.closed) return;
    // Painting a frame is presentation only: it never waits on session-view
    // readiness or a publication barrier, which would delay frames under load.
    const columns = surface.columns, rows = surface.rows;
    let lines;
    try { lines = this.runtime.scope.run(surface.store, () => surface.tui.render(columns)); }
    catch (error) {
      // History-reading components cannot render a missing publication as an
      // empty session. Retain their last frame without blocking native paint or
      // retiring the mount; successful preparation requests another render.
      if (error?.code === -32002 && error.message === 'session view unavailable') return;
      throw error;
    }
    // Capture lines and this exact barrier in the same synchronous turn. Never
    // render newer text after waiting on an older checkpoint acknowledgement.
    const checkpoint = surface.editor?.tail;
    await checkpoint;
    if (surface.closed || !surface.store.state.alive || columns !== surface.columns || rows !== surface.rows) return;
    await this.runtime.transport.notify('ui/frame', {
      resource_owner: surface.store.state.owner, surface_id: surface.id,
      revision: surface.revision++, columns, rows, lines,
    }, `frame:${surface.id}`);
  }
  async close(surface, value, observed = false) {
    if (surface.closed) return;
    surface.closed = true; surface.observedClose = observed; surface.phase = 'closed';
    if (observed) { surface.opened = false; surface.admittedMouseCapture = false; }
    surface.initialInputs.length = 0;
    if (surface.editor && observed) surface.editor.retired = true;
    this.surfaces.delete(surface.id);
    if (surface.slotKey && this.slots.get(surface.slotKey) === surface) this.slots.delete(surface.slotKey);
    this.runtime.timers.surface(surface);
    const disposed = new Set();
    for (const c of [surface.component, ...surface.tui.children, ...surface.tui.overlays.map(e => e.component)]) if (c && !disposed.has(c)) {
      disposed.add(c);
      try { this.runtime.scope.run(surface.store, () => c.dispose?.()); } catch (error) { this.runtime.backgroundError(error); }
    }
    surface.tui.listeners.clear();
    try {
      if ((surface.opened || surface.releasing) && !observed && surface.store.state.alive && !this.runtime.stopping) {
        // Stop accepting input before draining an adapter-initiated restoration.
        // Otherwise an accepted draft write can arrive after the native editor is
        // editable (or a replacement editor has read its seed) and overwrite it.
        // Host rescue/shutdown remain immediate, not blocked on extension effects.
        try { await surface.editor?.tail; }
        finally {
          await this.releaseMount(surface);
        }
      }
    } catch (error) { surface.reject?.(error); throw error; }
    // ui.custom continuations may paste into the native composer. Resolve only
    // after the host has acknowledged restoration, never before sending close.
    surface.resolve?.(value);
  }
  clearSlot(store, slot) {
    const surface = this.slots.get(`${store.state.key}:${slot}`);
    return surface ? this.close(surface) : Promise.resolve();
  }
  async ownerEnded(state) { await Promise.all([...this.surfaces.values()].filter(s => s.store.state === state).map(s => this.close(s, undefined, true))); }
  async cancelParent(id) {
    await Promise.all([...this.surfaces.values()].filter(s => s.store.id === id).map(s => {
      // A real cancellation can arrive after the origin's normal reply. Its
      // retained surface still carries that controller even when active has
      // dropped the request. Abort before draining, never convert cancel to ACK.
      s.store.controller.abort(rpcError(-32800, 'request cancelled'));
      return this.close(s);
    }));
  }
  async shutdown() { await Promise.all([...this.surfaces.values()].map(s => this.close(s, undefined, true))); }
  handle(method, params) {
    this.runtime.require('remote_ui');
    const surface = this.surfaces.get(params.surface_id);
    if (!surface || surface.closed) return;
    if (method === 'ui/closed') { this.close(surface, undefined, true).catch(e => this.runtime.backgroundError(e)); return; }
    this.runtime.scope.run(surface.store, () => {
      if (method === 'ui/resize') {
        surface.columns = dimension(params.columns); surface.rows = dimension(params.rows);
        surface.tui.invalidate(); surface.requestRender();
      } else if (method === 'ui/key') {
        if (!reservedKey(params, surface.placement)) {
          if (surface.placement === 'editor') this.editorInput(surface, params);
          else surface.tui.input(keyData(params), params.kind === 'release');
        }
      } else if (method === 'ui/editor-text') {
        if (surface.placement !== 'editor') invalid('ui/editor-text without an editor mount');
        const editor = surface.editor;
        if (!editor || surface.closed) return;
        const text = bounded(params.text, 'editor text', 262144);
        // The genuine component keeps pastes and undo: the host decision is a
        // replacement, not a rebuild of local editor state.
        editor.mutating++;
        try { this.runtime.scope.run(surface.store, () => {
          // A refused native admission re-offers the unchanged draft. Do not
          // reset the component's paste ledger, cursor or undo for that echo.
          if (params.paste) {
            const issued = params.editor_input;
            fields(issued, ['mount_id', 'input_revision'], 'editor_input');
            if (issued.mount_id !== surface.mountId || issued.input_revision !== editor.inputRevision + 1) invalid('editor paste mount/revision mismatch');
            surface.component.handleInput('\x1b[200~' + text + '\x1b[201~');
            editor.inputRevision = issued.input_revision;
          } else if (nativeEditors.has(surface.component) && params.native_changed !== undefined) {
            if (params.native_changed && (surface.component.getExpandedText?.() ?? surface.component.getText()) === text) surface.component.onChange?.(surface.component.getText());
          } else if ((surface.component.getExpandedText?.() ?? surface.component.getText()) !== text) surface.component.setText(text);
        }); }
        finally { editor.mutating--; }
        editor.checkpoint();
        surface.requestRender();
      } else if (method === 'ui/mouse') {
        if (!surface.ready) return; // No component input before capture admission.
        if (!surface.admittedMouseCapture) invalid('ui/mouse without capture');
        if (surface.placement === 'editor') unsupported('custom editor mouse input', 'no host-issued editor input revision');
        surface.tui.input(mouseData(params), false);
      } else unsupported(method);

    });
  }
}
