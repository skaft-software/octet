import { fields, invalid, rpcError, strict, unsupported } from './errors.mjs';
import { keyData, mouseData, reservedKey } from './keys.mjs';
import { mouseModeIntent } from './transport.mjs';
import { theme, keybindings } from './theme.mjs';
import { sliceByColumn, truncateToWidth, visibleWidth } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';

const CURSOR_MARKER = '\x1b_pi:c\x07';
const basic = new Set([0, 1, 2, 3, 4, 5, 7, 8, 9, 21, 22, 23, 24, 25, 27, 28, 29, 39, 49, ...Array.from({ length: 8 }, (_, i) => 30 + i), ...Array.from({ length: 8 }, (_, i) => 40 + i), ...Array.from({ length: 8 }, (_, i) => 90 + i), ...Array.from({ length: 8 }, (_, i) => 100 + i)]);
export function safeLines(lines) {
  if (!Array.isArray(lines) || lines.length > 256) throw rpcError(-32602, 'bounds_exceeded ui/frame rows');
  let total = 0;
  return lines.map(line => {
    if (typeof line !== 'string') invalid('component render must return string[]');
    // This Pi marker is component metadata, not terminal data. All other control
    // sequences (including images, hyperlinks and cursor movement) are refused.
    line = line.replaceAll(CURSOR_MARKER, '');
    const bytes = Buffer.byteLength(line); total += bytes;
    if (bytes > 16384 || total > 524288) throw rpcError(-32602, 'bounds_exceeded ui/frame text');
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
  });
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
      hide: remove, setHidden: hidden => { entry.hidden = Boolean(hidden); if (hidden && this.focus === value) this.setFocus(entry.before); else if (!hidden && !options.nonCapturing) this.setFocus(value); this.requestRender(); },
      isHidden: () => entry.hidden,
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
    const focus = this.topOverlay()?.component ?? this.focus;
    if (release && !focus?.wantsKeyRelease) return;
    focus?.handleInput?.(data);
    this.requestRender();
  }
  render(width) {
    let lines = this.children.flatMap(c => safeLines(c.render(width)));
    for (const e of this.overlays) {
      if (e.hidden || e.options.visible && !e.options.visible(width, this.surface.rows)) continue;
      const o = e.options, margin = typeof o.margin === 'number' ? { top: o.margin, right: o.margin, bottom: o.margin, left: o.margin } : (o.margin || {});
      const left = margin.left || 0, top = margin.top || 0;
      const availW = Math.max(1, width - left - (margin.right || 0)), availH = Math.max(1, this.surface.rows - top - (margin.bottom || 0));
      const w = Math.min(availW, Math.max(1, o.minWidth || 1, size(o.width, width, Math.min(80, availW))));
      const overlay = safeLines(e.component.render(w)).slice(0, Math.min(availH, size(o.maxHeight, this.surface.rows, availH)));
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
    return safeLines(lines);
  }
  start() { unsupported('tui.start', 'Rust owns the terminal'); }
  stop() { unsupported('tui.stop', 'close the host surface with done instead'); }
}

export class RemoteUI {
  constructor(runtime) { this.runtime = runtime; this.surfaces = new Map(); this.slots = new Map(); this.counter = 0; }
  async mount(store, placement, title, factory, { done, slot, overlayOptions } = {}) {
    this.runtime.require('remote_ui'); this.runtime.assertOwner(store);
    if (this.surfaces.size >= 16) throw rpcError(-32012, 'bounds_exceeded remote surfaces');
    if (slot) await this.clearSlot(store, slot);
    const surface = {
      id: `pi-${++this.counter}`, runtime: this.runtime, placement, store, closed: false, opened: false, revision: 0, scheduled: false,
      columns: 80, rows: 24, resolve: done,
    };
    surface.requestRender = () => {
      if (surface.closed || !surface.opened || surface.scheduled) return;
      surface.scheduled = true;
      setImmediate(() => {
        surface.scheduled = false;
        if (surface.closed || !surface.store.state.alive) return;
        this.runtime.scope.run(surface.store, () => this.push(surface)).catch(error => {
          this.runtime.backgroundError(error); this.close(surface, undefined, false).catch(e => this.runtime.backgroundError(e));
        });
      });
    };
    surface.store = { ...store, surface };
    surface.tui = new RemoteTUI(surface);
    this.surfaces.set(surface.id, surface);
    if (slot) { surface.slotKey = `${store.state.key}:${store.factory}:${slot}`; this.slots.set(surface.slotKey, surface); }
    try {
      const geometry = await this.runtime.hostCall('ui/open', { surface_id: surface.id, title, placement, ...(store.mouseCapture ? { mouse_capture: true } : {}) }, store);
      surface.columns = dimension(geometry.columns); surface.rows = dimension(geometry.rows); surface.opened = true;
      if (surface.closed || !store.state.alive) { await this.close(surface); return surface; }
      const finish = value => this.close(surface, value).catch(error => this.runtime.backgroundError(error));
      surface.component = await this.runtime.scope.run(surface.store, () => factory(surface.tui.facade, theme, keybindings, finish));
      component(surface.component);
      if (overlayOptions) surface.tui.showOverlay(surface.component, overlayOptions);
      else { surface.tui.children.push(surface.component); surface.tui.setFocus(surface.component); }
      if (placement === 'editor') await this.bindEditor(surface);
      await this.push(surface);
      if (placement === 'fullscreen' && store.method === 'command/execute' && store.detach) store.detach();
      return surface;
    } catch (error) { await this.close(surface, undefined, !surface.opened).catch(e => this.runtime.backgroundError(e)); throw error; }
  }
  async bindEditor(surface) {
    const c = surface.component, store = surface.store;
    this.runtime.require('composer'); this.runtime.require('message_injection');
    if (typeof c.setText !== 'function' || typeof c.getText !== 'function') unsupported('editor component', 'requires getText/setText');
    const { text } = await this.runtime.hostCall('composer/get', {}, store);
    store.state.host.composer_text = text; c.setText(text);
    const change = c.onChange, submit = c.onSubmit;
    c.onChange = value => {
      store.state.host.composer_text = value;
      this.runtime.track(this.runtime.hostCall('composer/set', { text: value }, store), store);
      change?.(value);
    };
    c.onSubmit = value => {
      this.runtime.track(this.runtime.hostCall('session/send_user_message', { text: value }, store), store);
      submit?.(value);
    };
  }
  async push(surface) {
    if (surface.closed) return;
    const lines = this.runtime.scope.run(surface.store, () => surface.tui.render(surface.columns));
    await this.runtime.transport.notify('ui/frame', {
      resource_owner: surface.store.state.owner, surface_id: surface.id,
      revision: surface.revision++, columns: surface.columns, rows: surface.rows, lines,
    }, `frame:${surface.id}`);
  }
  async close(surface, value, observed = false) {
    if (surface.closed) return;
    surface.closed = true;
    this.surfaces.delete(surface.id);
    if (surface.slotKey && this.slots.get(surface.slotKey) === surface) this.slots.delete(surface.slotKey);
    this.runtime.timers.surface(surface);
    const disposed = new Set();
    for (const c of [surface.component, ...surface.tui.children, ...surface.tui.overlays.map(e => e.component)]) if (c && !disposed.has(c)) {
      disposed.add(c);
      try { this.runtime.scope.run(surface.store, () => c.dispose?.()); } catch (error) { this.runtime.backgroundError(error); }
    }
    surface.tui.listeners.clear(); surface.resolve?.(value);
    if (surface.opened && !observed && surface.store.state.alive && !this.runtime.stopping) await this.runtime.hostCall('ui/close', { surface_id: surface.id }, { ...surface.store, controller: new AbortController() });
  }
  clearSlot(store, slot) {
    const surface = this.slots.get(`${store.state.key}:${store.factory}:${slot}`);
    return surface ? this.close(surface) : Promise.resolve();
  }
  async ownerEnded(state) { await Promise.all([...this.surfaces.values()].filter(s => s.store.state === state).map(s => this.close(s, undefined, true))); }
  async cancelParent(id) { await Promise.all([...this.surfaces.values()].filter(s => s.store.id === id).map(s => this.close(s))); }
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
        if (!reservedKey(params)) surface.tui.input(keyData(params), params.kind === 'release');
      } else if (method === 'ui/mouse') {
        if (!surface.store.mouseCapture) invalid('ui/mouse without capture');
        surface.tui.input(mouseData(params), false);
      } else unsupported(method);

    });
  }
}
