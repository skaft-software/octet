const textEncoder = new TextEncoder();

// Private legacy projection, not a canonical API 0.3 capability descriptor.
export const API_VERSION = "0.2";
export const SEMANTIC_UI_FEATURE = "semantic_ui";

export const MAX_EXTENSION_UI_KEY_BYTES = 128;
export const MAX_EXTENSION_UI_TEXT_BYTES = 8 * 1024;
export const MAX_EXTENSION_UI_LINES = 32;
export const MAX_EXTENSION_UI_INDICATOR_FRAMES = 16;
export const MAX_EXTENSION_UI_ENTRIES = 64;
export const MIN_EXTENSION_UI_INTERVAL_MS = 16;
export const MAX_EXTENSION_UI_INTERVAL_MS = 10_000;
export const MAX_COMPONENT_INPUT_BYTES = 1024;
export const MAX_COMPONENT_WIDTH = 10_000;
export const DEFAULT_COMPONENT_WIDTH = 80;

export const SEMANTIC_UI_LIMITS = Object.freeze({
  maxKeyBytes: MAX_EXTENSION_UI_KEY_BYTES,
  maxTextBytes: MAX_EXTENSION_UI_TEXT_BYTES,
  maxLines: MAX_EXTENSION_UI_LINES,
  maxIndicatorFrames: MAX_EXTENSION_UI_INDICATOR_FRAMES,
  maxEntries: MAX_EXTENSION_UI_ENTRIES,
  minIntervalMs: MIN_EXTENSION_UI_INTERVAL_MS,
  maxIntervalMs: MAX_EXTENSION_UI_INTERVAL_MS,
  maxComponentInputBytes: MAX_COMPONENT_INPUT_BYTES,
});

function deepFreeze(value, seen = new Set()) {
  if (value === null || typeof value !== "object" || seen.has(value)) return value;
  seen.add(value);
  for (const child of Object.values(value)) deepFreeze(child, seen);
  return Object.freeze(value);
}

export const SEMANTIC_UI_ADMISSION = deepFreeze({
  apiVersion: API_VERSION,
  api_version: API_VERSION,
  feature: SEMANTIC_UI_FEATURE,
  methods: ["status/contribution", "ui/contribution"],
  surfaces: ["status", "header", "footer"],
  limits: SEMANTIC_UI_LIMITS,
});

export class SemanticUiError extends Error {
  constructor(message, cause) {
    super(message);
    this.name = new.target.name;
    if (cause !== undefined) this.cause = cause;
  }
}
export class SemanticUiAdmissionError extends SemanticUiError {}
export class SemanticUiDisposedError extends SemanticUiError {}
export class SemanticUiStaleError extends SemanticUiError {}
export class SemanticUiValidationError extends SemanticUiError {}
export class SemanticUiLimitError extends SemanticUiError {}
export class SemanticUiComponentError extends SemanticUiError {}
export class SemanticUiUnsupportedError extends SemanticUiError {}
export class SemanticUiTransportError extends SemanticUiError {}

function byteLength(value) { return textEncoder.encode(value).length; }

function truncateUtf8(value, maxBytes) {
  if (maxBytes <= 0) return "";
  if (byteLength(value) <= maxBytes) return value;
  let result = "";
  let used = 0;
  for (const character of value) {
    const size = byteLength(character);
    if (used + size > maxBytes) break;
    result += character;
    used += size;
  }
  return result;
}

function skipCsi(value, index) {
  while (index < value.length) {
    const code = value.charCodeAt(index++);
    if (code >= 0x40 && code <= 0x7e) break;
  }
  return index;
}

function skipStringControl(value, index) {
  while (index < value.length) {
    const code = value.charCodeAt(index);
    if (code === 0x07 || code === 0x9c) return index + 1;
    if (code === 0x1b && value.charCodeAt(index + 1) === 0x5c) return index + 2;
    index += 1;
  }
  return index;
}

/** Remove terminal controls and produce frontend-neutral bounded text. */
export function stripTerminalControls(value, { singleLine = true, ascii = false } = {}) {
  if (typeof value !== "string") throw new SemanticUiValidationError("semantic UI text must be a string");
  let result = "";
  for (let index = 0; index < value.length;) {
    const code = value.codePointAt(index);
    if (code === 0x1b) {
      const next = value.charCodeAt(index + 1);
      if (next === 0x5b) index = skipCsi(value, index + 2);
      else if ([0x5d, 0x50, 0x5e, 0x5f, 0x58].includes(next)) index = skipStringControl(value, index + 2);
      else index = Math.min(value.length, index + 2);
      continue;
    }
    if (code === 0x9b) {
      index = skipCsi(value, index + 1);
      continue;
    }
    if ([0x9d, 0x90, 0x98, 0x9e, 0x9f].includes(code)) {
      index = skipStringControl(value, index + 1);
      continue;
    }
    const character = String.fromCodePoint(code);
    index += character.length;
    if (code === 0x0a || code === 0x0d || character === "\u2028" || character === "\u2029") {
      result += singleLine ? " " : "\n";
    } else if (code === 0x09) {
      result += "\t";
    } else if (code < 0x20 || code === 0x7f || (code >= 0x80 && code <= 0x9f)) {
      continue;
    } else if (ascii && code > 0x7e) {
      result += "?";
    } else {
      result += character;
    }
  }
  return result;
}

export function sanitizeText(value, options = {}) {
  const maxBytes = options.maxBytes ?? MAX_EXTENSION_UI_TEXT_BYTES;
  if (!Number.isInteger(maxBytes) || maxBytes < 0) {
    throw new SemanticUiValidationError("semantic UI text bound must be a non-negative integer");
  }
  return truncateUtf8(stripTerminalControls(value, {
    singleLine: options.singleLine !== false,
    ascii: options.ascii === true,
  }), maxBytes);
}

export function sanitizeLines(lines, options = {}) {
  if (!Array.isArray(lines)) throw new SemanticUiValidationError("Pi components must render a string[]");
  return lines.slice(0, MAX_EXTENSION_UI_LINES).map((line) => sanitizeText(line, {
    maxBytes: options.maxBytes ?? MAX_EXTENSION_UI_TEXT_BYTES,
    singleLine: true,
    ascii: options.ascii === true,
  }));
}

export function validateUiKey(key) {
  if (typeof key !== "string" || key.length === 0 || byteLength(key) > MAX_EXTENSION_UI_KEY_BYTES) {
    throw new SemanticUiValidationError(`UI key must contain 1..=${MAX_EXTENSION_UI_KEY_BYTES} UTF-8 bytes`);
  }
  for (const byte of textEncoder.encode(key)) {
    if (!((byte >= 0x30 && byte <= 0x39) || (byte >= 0x41 && byte <= 0x5a) ||
      (byte >= 0x61 && byte <= 0x7a) || byte === 0x2e || byte === 0x5f || byte === 0x2d)) {
      throw new SemanticUiValidationError("UI key must use ASCII letters, digits, '.', '_', or '-'");
    }
  }
  return key;
}

function validateBoundedIdentifier(value, label) {
  if (typeof value !== "string" || value.length === 0 || byteLength(value) > MAX_EXTENSION_UI_KEY_BYTES) {
    throw new SemanticUiValidationError(`${label} must be a non-empty bounded string`);
  }
  if ([...value].some((character) => character < " " || character === "\u007f" || character === "\u001b")) {
    throw new SemanticUiValidationError(`${label} must not contain controls`);
  }
  return value;
}

function normalizeOwner(owner) {
  if (!owner || typeof owner !== "object") {
    throw new SemanticUiValidationError("semantic UI adapter requires an explicit owner with extensionInstanceId and generation");
  }
  const extensionInstanceId = owner.extensionInstanceId ?? owner.extension_instance_id;
  const generation = owner.generation ?? owner.processGeneration ?? owner.process_generation;
  const sessionId = owner.sessionId ?? owner.session_id;
  validateBoundedIdentifier(extensionInstanceId, "extensionInstanceId");
  if (!Number.isSafeInteger(generation) || generation < 0) {
    throw new SemanticUiValidationError("owner generation must be a non-negative safe integer");
  }
  if (sessionId !== undefined) validateBoundedIdentifier(sessionId, "sessionId");
  const publicOwner = { extensionInstanceId, generation };
  const wireOwner = { extension_instance_id: extensionInstanceId, process_generation: generation };
  if (sessionId !== undefined) {
    publicOwner.sessionId = sessionId;
    wireOwner.session_id = sessionId;
  }
  return { publicOwner: Object.freeze(publicOwner), wireOwner: Object.freeze(wireOwner) };
}

function normalizeWidth(width) {
  if (!Number.isSafeInteger(width) || width < 1) throw new SemanticUiValidationError("component width must be a positive integer");
  return Math.min(width, MAX_COMPONENT_WIDTH);
}

function normalizePlacement(options) {
  const placement = typeof options === "string" ? options : options?.placement;
  if (placement === undefined || placement === "aboveEditor" || placement === "above_editor") return "above_editor";
  if (placement === "belowEditor" || placement === "below_editor") return "below_editor";
  throw new SemanticUiValidationError("widget placement must be aboveEditor or belowEditor");
}

function cloneBounded(value, depth = 0, seen = new Set()) {
  if (value === null || typeof value === "boolean") return value;
  if (typeof value === "string") return sanitizeText(value, { singleLine: false, maxBytes: MAX_EXTENSION_UI_TEXT_BYTES });
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value !== "object") return undefined;
  if (seen.has(value)) return "[Circular]";
  if (depth >= 4) return "[Truncated]";
  seen.add(value);
  let result;
  if (Array.isArray(value)) {
    result = [];
    for (let index = 0; index < Math.min(value.length, 64); index += 1) {
      const child = cloneBounded(value[index], depth + 1, seen);
      if (child !== undefined) result.push(child);
    }
  } else {
    result = Object.create(null);
    for (const key of Object.keys(value).slice(0, 64)) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !("value" in descriptor)) continue;
      const child = cloneBounded(descriptor.value, depth + 1, seen);
      if (child !== undefined) result[sanitizeText(key, { maxBytes: MAX_EXTENSION_UI_KEY_BYTES })] = child;
    }
  }
  seen.delete(value);
  return deepFreeze(result);
}

function normalizeOptionalText(value, label, ascii) {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") throw new SemanticUiValidationError(`${label} must be a string or null`);
  return sanitizeText(value, { ascii });
}

function normalizeFrames(value, ascii, reducedMotion) {
  if (value === undefined || value === null) return null;
  if (!Array.isArray(value)) throw new SemanticUiValidationError("working indicator frames must be a string[] or null");
  const frames = value.slice(0, MAX_EXTENSION_UI_INDICATOR_FRAMES).map((frame) => {
    if (typeof frame !== "string") throw new SemanticUiValidationError("working indicator frames must contain strings");
    return sanitizeText(frame, { ascii });
  });
  return reducedMotion && frames.length > 1 ? [frames[0]] : frames;
}

function normalizeInterval(value) {
  if (value === undefined || value === null) return null;
  if (!Number.isSafeInteger(value) || value < MIN_EXTENSION_UI_INTERVAL_MS || value > MAX_EXTENSION_UI_INTERVAL_MS) {
    throw new SemanticUiValidationError(`working indicator interval must be ${MIN_EXTENSION_UI_INTERVAL_MS}..=${MAX_EXTENSION_UI_INTERVAL_MS}ms`);
  }
  return value;
}

function isThenable(value) {
  return value !== null && (typeof value === "object" || typeof value === "function") && typeof value.then === "function";
}

function makeSafeTheme(ascii) {
  const styleText = (_role, text) => text === undefined ? "" : sanitizeText(String(text), { ascii });
  const identity = (text) => text === undefined ? "" : sanitizeText(String(text), { ascii });
  return Object.freeze({
    name: "octet-semantic", mode: "semantic", ascii, noColor: true, supportsColor: false, colors: false,
    getFgAnsi: () => "", getBgAnsi: () => "", fg: styleText, bg: styleText, color: styleText,
    bold: identity, dim: identity, italic: identity, underline: identity, strikethrough: identity,
    inverse: identity, link: (_url, text) => text === undefined ? "" : sanitizeText(String(text), { ascii }),
  });
}

function makeSafeFooterDataProvider() {
  return Object.freeze({
    getGitBranch: () => null,
    getExtensionStatuses: () => Object.freeze([]),
    getAvailableProviderCount: () => 0,
    onBranchChange: () => () => {},
  });
}

function makeComponent(value, args, label, ascii) {
  let component = value;
  if (typeof component === "function") {
    try { component = component(...args); }
    catch (error) { throw new SemanticUiComponentError(`${label} factory failed`, error); }
  }
  if (isThenable(component)) throw new SemanticUiUnsupportedError(`${label} factories must return synchronously`);
  try {
    if (!component || typeof component !== "object" || typeof component.render !== "function") {
      throw new SemanticUiComponentError(`${label} must provide render(width)`);
    }
    if (typeof component.invalidate !== "function") throw new SemanticUiComponentError(`${label} must provide invalidate()`);
    if (component.handleInput !== undefined && typeof component.handleInput !== "function") {
      throw new SemanticUiComponentError(`${label}.handleInput must be a function when present`);
    }
    if (component.wantsKeyRelease !== undefined && typeof component.wantsKeyRelease !== "boolean") {
      throw new SemanticUiComponentError(`${label}.wantsKeyRelease must be boolean when present`);
    }
  } catch (error) {
    try { component?.dispose?.(); } catch { /* retain the construction error */ }
    throw error;
  }
  return {
    wantsKeyRelease: component.wantsKeyRelease === true,
    render(width) {
      let lines;
      try { lines = component.render.call(component, width); }
      catch (error) { throw new SemanticUiComponentError(`${label}.render failed`, error); }
      try { return sanitizeLines(lines, { ascii }); }
      catch (error) { throw new SemanticUiComponentError(`${label}.render returned invalid lines`, error); }
    },
    invalidate() {
      try { return component.invalidate.call(component); }
      catch (error) { throw new SemanticUiComponentError(`${label}.invalidate failed`, error); }
    },
    handleInput(data) {
      if (typeof component.handleInput !== "function") return false;
      try { return component.handleInput.call(component, data); }
      catch (error) { throw new SemanticUiComponentError(`${label}.handleInput failed`, error); }
    },
    dispose() {
      if (typeof component.dispose !== "function") return undefined;
      try { component.dispose.call(component); }
      catch (error) { return error; }
      return undefined;
    },
  };
}

function makeContribution(value) { return deepFreeze(value); }

export function createSemanticUiAdapter(options = {}) {
  if (!options || typeof options !== "object") throw new SemanticUiValidationError("semantic UI adapter options must be an object");
  const apiVersion = options.apiVersion ?? API_VERSION;
  if (apiVersion !== API_VERSION) throw new SemanticUiAdmissionError("semantic UI projection requires legacy API 0.2 admission");
  const admission = SEMANTIC_UI_ADMISSION;
  const admit = options.admit ?? options.admission;
  if (typeof admit !== "function") {
    throw new SemanticUiAdmissionError("semantic UI is disabled without an explicit admission callback");
  }
  let admissionResult;
  try { admissionResult = admit(admission); }
  catch (error) { throw new SemanticUiAdmissionError("semantic UI admission callback failed", error); }
  if (!(admissionResult === true || admissionResult?.accepted === true)) {
    throw new SemanticUiAdmissionError(`semantic UI requires admitted API ${apiVersion} semantic_ui support`);
  }

  const emit = options.emitContribution ?? options.emit;
  if (typeof emit !== "function") throw new SemanticUiValidationError("semantic UI adapter requires a synchronous emitContribution callback");
  const emitStatus = options.emitStatus;
  if (emitStatus !== undefined && typeof emitStatus !== "function") throw new SemanticUiValidationError("emitStatus must be a function when provided");
  const diagnostic = options.diagnostic;
  if (diagnostic !== undefined && typeof diagnostic !== "function") throw new SemanticUiValidationError("diagnostic must be a function when provided");
  const isCurrent = options.isCurrent;
  if (isCurrent !== undefined && typeof isCurrent !== "function") throw new SemanticUiValidationError("isCurrent must be a function when provided");

  const ownerInput = options.owner ?? {
    sessionId: options.sessionId,
    extensionInstanceId: options.extensionInstanceId,
    generation: options.generation ?? options.processGeneration,
  };
  const { publicOwner, wireOwner } = normalizeOwner(ownerInput);
  const initialWidth = normalizeWidth(options.width ?? DEFAULT_COMPONENT_WIDTH);
  const ascii = options.ascii === true;
  const reducedMotion = options.reducedMotion === true;
  const signal = options.signal;
  if (signal !== undefined && (!signal || typeof signal.addEventListener !== "function")) throw new SemanticUiValidationError("signal must be an AbortSignal-like object");

  const state = {
    width: initialWidth, disposed: false, renderRequested: false,
    statuses: new Map(), widgets: new Map(), header: null, footer: null,
    working: { message: null, visible: null, frames: null, interval_ms: null },
    hiddenThinkingLabel: null, messageRenderers: new Map(), entryRenderers: new Map(),
  };
  const safeTheme = makeSafeTheme(ascii);
  const footerDataProvider = makeSafeFooterDataProvider();
  function makeSafeTui(scheduleRender) {
    return Object.freeze({
      get width() { return state.width; },
      get terminalWidth() { return state.width; },
      height: 0,
      getWidth: () => state.width,
      requestRender: scheduleRender,
      invalidate: scheduleRender,
    });
  }

  function requestRender() {
    assertLive();
    state.renderRequested = true;
    options.requestRender?.();
  }
  function report(kind, error) {
    if (typeof diagnostic !== "function") return;
    const message = sanitizeText(error instanceof Error ? error.message : String(error), { maxBytes: 512, ascii });
    try { diagnostic(deepFreeze({ kind, message, owner: wireOwner })); } catch { /* best effort */ }
  }
  function disposeComponent(slot, label) {
    if (!slot?.component) return;
    const error = slot.component.dispose();
    if (error) report("component_dispose", new SemanticUiComponentError(`${label} dispose failed`, error));
  }
  function disposeAllComponents() {
    for (const [key, slot] of state.widgets) disposeComponent(slot, `widget ${key}`);
    disposeComponent(state.header, "header");
    disposeComponent(state.footer, "footer");
  }
  let abortListener;
  function dispose(reason = "disposed") {
    if (state.disposed) return;
    state.disposed = true;
    if (signal && abortListener) signal.removeEventListener?.("abort", abortListener);
    disposeAllComponents();
    state.statuses.clear(); state.widgets.clear(); state.header = null; state.footer = null;
    state.messageRenderers.clear(); state.entryRenderers.clear(); state.renderRequested = false;
    void reason;
  }
  function assertLive() {
    if (state.disposed) throw new SemanticUiDisposedError("semantic UI adapter has been disposed");
    if (signal?.aborted) { dispose("cancelled"); throw new SemanticUiDisposedError("semantic UI adapter was cancelled"); }
    if (isCurrent) {
      let current;
      try { current = isCurrent(publicOwner); }
      catch (error) { dispose("stale"); throw new SemanticUiStaleError("owner/generation admission check failed", error); }
      if (current !== true) { dispose("stale"); throw new SemanticUiStaleError("semantic UI owner or generation is stale"); }
    }
  }
  function send(payload, method) {
    assertLive();
    const callback = method === "status/contribution" && emitStatus ? emitStatus : emit;
    const context = deepFreeze({ method, apiVersion, feature: SEMANTIC_UI_FEATURE,
      owner: wireOwner, resource_owner: wireOwner });
    let result;
    try { result = callback(payload, context); }
    catch (error) { throw new SemanticUiTransportError(`${method} callback failed`, error); }
    if (result === false) throw new SemanticUiTransportError(`${method} callback rejected the contribution`);
    if (isThenable(result)) throw new SemanticUiTransportError(`${method} callback must complete synchronously`);
  }
  function ensureEntryCapacity(map, key) {
    if (!map.has(key) && state.statuses.size + state.widgets.size >= MAX_EXTENSION_UI_ENTRIES) throw new SemanticUiLimitError(`semantic UI entry limit ${MAX_EXTENSION_UI_ENTRIES} reached`);
  }
  function renderComponent(slot, label, invalidate = false) {
    if (invalidate) slot.component.invalidate();
    return slot.component.render(state.width);
  }
  function makeUiComponent(value, label, includeFooterData = false) {
    let active = true;
    const tui = makeSafeTui(() => {
      if (!active) throw new SemanticUiDisposedError(`${label} component has been disposed`);
      requestRender();
    });
    const args = includeFooterData ? [tui, safeTheme, footerDataProvider] : [tui, safeTheme];
    let component;
    try { component = makeComponent(value, args, label, ascii); }
    catch (error) { active = false; throw error; }
    return { ...component, dispose() {
      if (!active) return;
      active = false;
      return component.dispose();
    } };
  }
  function prepareWidget(key, content, placement) {
    if (typeof content === "string") return { key, placement, component: null, lines: sanitizeLines([content], { ascii }) };
    if (Array.isArray(content)) return { key, placement, component: null, lines: sanitizeLines(content, { ascii }) };
    const component = makeUiComponent(content, `widget ${key}`);
    const slot = { key, placement, component, lines: null };
    try { slot.lines = renderComponent(slot, `widget ${key}`); }
    catch (error) { disposeComponent(slot, `widget ${key}`); throw error; }
    return slot;
  }
  function setStatus(key, text) {
    assertLive(); validateUiKey(key);
    const normalized = normalizeOptionalText(text, "status text", ascii);
    if (normalized !== null) ensureEntryCapacity(state.statuses, key);
    send(makeContribution({ kind: "status", key, text: normalized, style_role: "extension.pi.status", priority: 0 }), "ui/contribution");
    if (normalized === null) state.statuses.delete(key); else state.statuses.set(key, normalized);
  }
  function setWidget(key, content, optionsForWidget) {
    assertLive(); validateUiKey(key);
    const placement = normalizePlacement(optionsForWidget);
    const oldSlot = state.widgets.get(key);
    if (content === undefined || content === null) {
      send(makeContribution({ kind: "widget", key, lines: null, placement: oldSlot?.placement ?? placement, style_role: "extension.pi.status", priority: 0 }), "ui/contribution");
      if (oldSlot) { disposeComponent(oldSlot, `widget ${key}`); state.widgets.delete(key); }
      return;
    }
    ensureEntryCapacity(state.widgets, key);
    const nextSlot = prepareWidget(key, content, placement);
    try {
      send(makeContribution({ kind: "widget", key, lines: nextSlot.lines, placement, style_role: "extension.pi.status", priority: 0 }), "ui/contribution");
    } catch (error) { disposeComponent(nextSlot, `widget ${key}`); throw error; }
    if (oldSlot) disposeComponent(oldSlot, `widget ${key}`);
    state.widgets.set(key, nextSlot);
  }
  function emitWorking(next) {
    send(makeContribution({ kind: "working", message: next.message, visible: next.visible, frames: next.frames, interval_ms: next.interval_ms }), "ui/contribution");
    state.working = next;
  }
  function setWorkingMessage(message) {
    assertLive(); emitWorking({ ...state.working, message: normalizeOptionalText(message, "working message", ascii) });
  }
  function setWorkingVisible(visible) {
    assertLive();
    if (visible !== undefined && visible !== null && typeof visible !== "boolean") throw new SemanticUiValidationError("working visibility must be boolean or null");
    emitWorking({ ...state.working, visible: visible ?? null });
  }
  function setWorkingIndicator(framesOrOptions, intervalMs) {
    assertLive();
    let framesInput; let intervalInput;
    if (Array.isArray(framesOrOptions)) { framesInput = framesOrOptions; intervalInput = intervalMs; }
    else if (framesOrOptions === undefined || framesOrOptions === null) { framesInput = null; intervalInput = null; }
    else if (typeof framesOrOptions === "object") { framesInput = framesOrOptions.frames; intervalInput = framesOrOptions.intervalMs ?? framesOrOptions.interval_ms; }
    else throw new SemanticUiValidationError("working indicator must be frames or an options object");
    emitWorking({ ...state.working, frames: normalizeFrames(framesInput, ascii, reducedMotion), interval_ms: reducedMotion ? null : normalizeInterval(intervalInput) });
  }
  function setHiddenThinkingLabel(label) {
    assertLive();
    const normalized = normalizeOptionalText(label, "hidden thinking label", ascii);
    send(makeContribution({ kind: "hidden_thinking", label: normalized }), "ui/contribution");
    state.hiddenThinkingLabel = normalized;
  }
  function surfaceText(lines) { return sanitizeText(lines.join(" "), { ascii }); }
  function setSurface(surface, factory) {
    assertLive();
    const oldSlot = state[surface];
    if (factory === undefined || factory === null) {
      send(makeContribution({ surface, text: "", style_role: "extension.pi.status", priority: 0 }), "status/contribution");
      if (oldSlot) { disposeComponent(oldSlot, surface); state[surface] = null; }
      return;
    }
    const component = makeUiComponent(factory, surface, surface === "footer");
    const nextSlot = { component, lines: null };
    try {
      nextSlot.lines = renderComponent(nextSlot, surface);
      send(makeContribution({ surface, text: surfaceText(nextSlot.lines), style_role: "extension.pi.status", priority: 0 }), "status/contribution");
    } catch (error) { disposeComponent(nextSlot, surface); throw error; }
    if (oldSlot) disposeComponent(oldSlot, surface);
    state[surface] = nextSlot;
  }
  function repaintWidget(slot, invalidate) {
    const lines = renderComponent(slot, `widget ${slot.key}`, invalidate);
    send(makeContribution({ kind: "widget", key: slot.key, lines, placement: slot.placement, style_role: "extension.pi.status", priority: 0 }), "ui/contribution");
    slot.lines = lines;
  }
  function repaintSurface(surface, slot, invalidate) {
    const lines = renderComponent(slot, surface, invalidate);
    send(makeContribution({ surface, text: surfaceText(lines), style_role: "extension.pi.status", priority: 0 }), "status/contribution");
    slot.lines = lines;
  }
  function resize(width) {
    assertLive(); state.width = normalizeWidth(width);
    for (const slot of state.widgets.values()) {
      if (!slot.component) continue;
      try { repaintWidget(slot, true); } catch (error) { if (error instanceof SemanticUiComponentError) report("component_resize", error); else throw error; }
    }
    for (const surface of ["header", "footer"]) {
      const slot = state[surface]; if (!slot) continue;
      try { repaintSurface(surface, slot, true); } catch (error) { if (error instanceof SemanticUiComponentError) report("component_resize", error); else throw error; }
    }
  }
  function invalidate() {
    assertLive(); state.renderRequested = false;
    for (const slot of state.widgets.values()) {
      if (!slot.component) continue;
      try { repaintWidget(slot, true); } catch (error) { if (error instanceof SemanticUiComponentError) report("component_invalidate", error); else throw error; }
    }
    for (const surface of ["header", "footer"]) {
      const slot = state[surface]; if (!slot) continue;
      try { repaintSurface(surface, slot, true); } catch (error) { if (error instanceof SemanticUiComponentError) report("component_invalidate", error); else throw error; }
    }
  }
  function findInputSlot(surfaceOrKey) {
    if (surfaceOrKey === "header" || surfaceOrKey === "footer") return { surface: surfaceOrKey, slot: state[surfaceOrKey] };
    validateUiKey(surfaceOrKey); return { surface: null, slot: state.widgets.get(surfaceOrKey) };
  }
  function wantsKeyRelease(surfaceOrKey) {
    assertLive();
    return findInputSlot(surfaceOrKey).slot?.component?.wantsKeyRelease === true;
  }
  function handleInput(surfaceOrKey, input) {
    assertLive();
    if (typeof input !== "string") throw new SemanticUiValidationError("component input must be a string");
    const { surface, slot } = findInputSlot(surfaceOrKey);
    if (!slot?.component) return false;
    const result = slot.component.handleInput(sanitizeText(input, { maxBytes: MAX_COMPONENT_INPUT_BYTES, singleLine: false, ascii }));
    if (surface) repaintSurface(surface, slot, true); else repaintWidget(slot, true);
    return result === undefined ? true : result;
  }
  function registerRenderer(map, kind, customType, renderer) {
    assertLive(); validateBoundedIdentifier(customType, `${kind} renderer type`);
    if (typeof renderer !== "function") throw new SemanticUiValidationError(`${kind} renderer must be a function`);
    const registration = { customType, renderer }; map.set(customType, registration);
    return () => { if (map.get(customType) === registration) map.delete(customType); };
  }
  function registerMessageRenderer(customType, renderer) { return registerRenderer(state.messageRenderers, "message", customType, renderer); }
  function registerEntryRenderer(customType, renderer) { return registerRenderer(state.entryRenderers, "entry", customType, renderer); }
  function renderRegisteredRenderer(kind, customType, payload, rendererOptions = {}, width = state.width) {
    assertLive(); validateBoundedIdentifier(customType, `${kind} renderer type`);
    const map = kind === "message" ? state.messageRenderers : kind === "entry" ? state.entryRenderers : null;
    if (!map) throw new SemanticUiValidationError("renderer kind must be message or entry");
    const registration = map.get(customType);
    if (!registration) throw new SemanticUiUnsupportedError(`no ${kind} renderer is registered for ${customType}`);
    let component;
    try { component = registration.renderer(cloneBounded(payload), cloneBounded(rendererOptions) ?? Object.create(null), makeSafeTheme(ascii)); }
    catch (error) { throw new SemanticUiComponentError(`${kind} renderer failed`, error); }
    return makeComponent(component, [], `${kind} renderer ${customType}`, ascii).render(normalizeWidth(width));
  }
  function renderMessageRenderer(customType, payload, rendererOptions, width) { return renderRegisteredRenderer("message", customType, payload, rendererOptions, width); }
  function renderEntryRenderer(customType, payload, rendererOptions, width) { return renderRegisteredRenderer("entry", customType, payload, rendererOptions, width); }
  function rendererTypes(kind) {
    assertLive(); const map = kind === "message" ? state.messageRenderers : kind === "entry" ? state.entryRenderers : null;
    if (!map) throw new SemanticUiValidationError("renderer kind must be message or entry");
    return Object.freeze([...map.keys()]);
  }
  function snapshot() {
    const widgets = Object.create(null);
    for (const [key, slot] of state.widgets) widgets[key] = { lines: slot.lines ? [...slot.lines] : null, placement: slot.placement, component: Boolean(slot.component) };
    return deepFreeze({ owner: publicOwner, width: state.width, statuses: Object.fromEntries(state.statuses), widgets,
      working: { ...state.working }, hiddenThinkingLabel: state.hiddenThinkingLabel,
      header: state.header?.lines ? [...state.header.lines] : null, footer: state.footer?.lines ? [...state.footer.lines] : null,
      renderRequested: state.renderRequested, disposed: state.disposed });
  }

  if (signal) {
    abortListener = () => dispose("cancelled");
    if (signal.aborted) dispose("cancelled"); else signal.addEventListener("abort", abortListener, { once: true });
  }
  const ui = Object.freeze({ setStatus, setWorkingMessage, setWorkingVisible, setWorkingIndicator, setHiddenThinkingLabel, setWidget,
    setHeader: (factory) => setSurface("header", factory), setFooter: (factory) => setSurface("footer", factory) });
  const renderers = Object.freeze({ registerMessageRenderer, registerEntryRenderer, renderMessageRenderer, renderEntryRenderer, rendererTypes });
  const adapter = { admitted: true, owner: publicOwner, admission, limits: SEMANTIC_UI_LIMITS, ui, renderers,
    api: Object.freeze({ ui, registerMessageRenderer, registerEntryRenderer }), setStatus, setWorkingMessage, setWorkingVisible,
    setWorkingIndicator, setHiddenThinkingLabel, setWidget, setHeader: (factory) => setSurface("header", factory),
    setFooter: (factory) => setSurface("footer", factory), registerMessageRenderer, registerEntryRenderer, renderMessageRenderer,
    renderEntryRenderer, rendererTypes, resize, invalidate, wantsKeyRelease, handleInput, snapshot, dispose, cancel: () => dispose("cancelled"),
    get disposed() { return state.disposed; }, get width() { return state.width; } };
  return Object.freeze(adapter);
}

export const createPiSemanticUiAdapter = createSemanticUiAdapter;
export default createSemanticUiAdapter;
