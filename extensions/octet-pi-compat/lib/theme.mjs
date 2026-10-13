import { isAbsolute } from 'node:path';
import { bounded, fields, invalid, plainJSON, strict, unsupported } from './errors.mjs';
import { backgroundAnsi, colorToOklch, foregroundAnsi, indexedColor, mixColors, oklchColor, parseColor, rgbColor, styleTextWithAnsi } from './theme-colors.mjs';
import { backgroundTokens, foregroundTokens, parsePiTheme, readPiTheme } from './theme-palette.mjs';
export { parsePiTheme, piThemeToNativeToml } from './theme-palette.mjs';
import { matchesKey } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';
const palette = {
  accent: 36, border: 90, borderAccent: 36, borderMuted: 90, success: 32,
  error: 31, warning: 33, muted: 90, dim: 90, text: 39, thinkingText: 90,
  userMessageText: 39, customMessageText: 39, customMessageLabel: 36,
  toolTitle: 36, toolOutput: 39, toolDiffAdded: 32, toolDiffRemoved: 31,
  toolDiffContext: 90, mdHeading: 36, mdLink: 34, mdLinkUrl: 90,
  mdCode: 33, mdCodeBlock: 39, mdCodeBlockBorder: 90, mdQuote: 90,
  mdQuoteBorder: 90, mdHr: 90, mdListBullet: 36,
  syntaxComment: 90, syntaxKeyword: 35, syntaxFunction: 34,
  syntaxVariable: 39, syntaxString: 32, syntaxNumber: 33,
  syntaxType: 36, syntaxOperator: 39, syntaxPunctuation: 39,
};
// Pure Pi palette rendering, not a claim about the host's current theme.
// Theme methods/fallbacks follow Pi 1.0.2 581e7ba...; MIT, see ../LICENSE.pi.
const styled = (open, close, text) => {
  text = String(text);
  if (!text) return '';
  return `\x1b[${open}m${text.replaceAll(`\x1b[${close}m`, `\x1b[${open}m`).replace(/\r?\n/g, line => `\x1b[${close}m${line}\x1b[${open}m`)}\x1b[${close}m`;
};
function concreteColor(color) {
  if (!color || typeof color !== 'object') invalid('theme style color');
  switch (color.kind) {
    case 'rgb': fields(color, ['kind', 'r', 'g', 'b'], 'theme RGB'); return rgbColor(color.r, color.g, color.b);
    case 'indexed': fields(color, ['kind', 'index'], 'theme indexed color'); return indexedColor(color.index);
    case 'oklch': fields(color, ['kind', 'l', 'c', 'h'], 'theme OKLCH'); return oklchColor(color.l, color.c, color.h);
    default: invalid('theme style color kind');
  }
}
function detectedAppearance(colors) {
  const average = names => {
    const fixed = names.map(name => colors[name]).filter(value => value !== '')
      .map(parseColor).filter(color => color.kind !== 'indexed' || color.index >= 16);
    return fixed.length ? fixed.reduce((sum, color) => sum + colorToOklch(color).l, 0) / fixed.length : undefined;
  };
  const fg = average(foregroundTokens), bg = average(backgroundTokens);
  if (fg !== undefined && bg !== undefined) return bg < fg ? 'dark' : 'light';
  if (bg !== undefined) return bg < 0.5 ? 'dark' : 'light';
  if (fg !== undefined) return fg > 0.5 ? 'dark' : 'light';
  const value = process.env.COLORFGBG?.split(';').at(-1)?.trim();
  return value && /^\d{1,2}$/.test(value) && +value <= 15 && !(+value <= 6 || +value === 8) ? 'light' : 'dark';
}
export class Theme {
  constructor(fgColors, bgColors, mode = 'truecolor', options = {}) {
    fields(fgColors, foregroundTokens, 'theme foregrounds'); fields(bgColors, backgroundTokens, 'theme backgrounds');
    fields(options, ['name', 'sourcePath', 'sourceInfo', 'appearance', 'dim', 'capabilities'], 'theme options');
    if (!['truecolor', '256color'].includes(mode)) invalid('theme color mode');
    const data = parsePiTheme({ name: options.name ?? '', ...(options.appearance === undefined ? {} : { appearance: options.appearance }), colors: { ...fgColors, ...bgColors } });
    if (options.dim !== undefined && (!Array.isArray(options.dim) || options.dim.length > foregroundTokens.length || options.dim.some(token => !foregroundTokens.includes(token)))) invalid('theme dim tokens');
    this.name = options.name; this.sourcePath = options.sourcePath;
    this.sourceInfo = options.sourceInfo === undefined ? undefined : plainJSON(options.sourceInfo, 'theme sourceInfo');
    const modifiers = ['bold', 'dim', 'italic', 'underline', 'inverse', 'strikethrough'];
    const capabilities = options.capabilities ?? Object.fromEntries(modifiers.map(key => [key, true]));
    fields(capabilities, ['color', ...modifiers], 'theme capabilities');
    if (capabilities.color !== undefined && !['truecolor', '256color', '16color', 'none'].includes(capabilities.color)) invalid('theme color capability');
    for (const key of modifiers) if (typeof capabilities[key] !== 'boolean') invalid(`theme capability ${key}`);
    this.capabilities = Object.freeze({ color: mode, ...capabilities });
    this.mode = mode; this.dimTokens = new Set(this.capabilities.dim ? options.dim : []);
    this.appearance = data.appearance ?? detectedAppearance(data.colors);
    this.fgAnsi = new Map(); this.bgAnsi = new Map();
    const colors = {};
    for (const [names, map, background] of [[foregroundTokens, this.fgAnsi, false], [backgroundTokens, this.bgAnsi, true]]) {
      for (const token of names) {
        const value = data.colors[token];
        // Like Pi without terminal reports: default colors remain SGR defaults;
        // concrete color math uses the documented appearance-based guess.
        const fallback = background ? this.appearance === 'light' ? '#ffffff' : '#000000' : this.appearance === 'light' ? '#000000' : '#e5e5e7';
        colors[token] = parseColor(value === '' ? fallback : value);
        map.set(token, this.capabilities.color === 'none' ? '' : value === '' ? `\x1b[${background ? 49 : 39}m` : (background ? backgroundAnsi : foregroundAnsi)(colors[token], mode));
      }
    }
    const background = parseColor(this.appearance === 'light' ? '#ffffff' : '#000000');
    for (const token of this.dimTokens) colors[token] = mixColors(colors[token], background, 0.4);
    this.colors = Object.freeze(colors);
    this.borderColor = text => this.fg('borderMuted', text);
    this.selectList = {
      selectedPrefix: text => this.fg('accent', text), selectedText: text => this.fg('accent', text),
      description: text => this.fg('muted', text), scrollInfo: text => this.fg('muted', text), noMatch: text => this.fg('muted', text),
    };
  }
  token(map, role) {
    if (!map.has(role)) unsupported(`theme color ${role}`, 'unknown Pi palette role or incorrect foreground/background slot');
    return map.get(role);
  }
  fg(role, text) {
    const close = [this.dimTokens.has(role) ? 22 : undefined, this.capabilities.color !== 'none' ? 39 : undefined].filter(code => code !== undefined);
    return `${this.getFgAnsi(role)}${text}${close.length ? `\x1b[${close.join(';')}m` : ''}`;
  }
  bg(role, text) { return `${this.getBgAnsi(role)}${text}${this.capabilities.color === 'none' ? '' : '\x1b[49m'}`; }
  getFgAnsi(role) { return this.token(this.fgAnsi, role) + (this.dimTokens.has(role) ? '\x1b[2m' : ''); }
  getBgAnsi(role) { return this.token(this.bgAnsi, role); }
  getColorMode() { return this.mode; }
  style(text, options) {
    fields(options, ['fg', 'bg', 'bold', 'dim', 'italic', 'underline', 'inverse', 'strikethrough'], 'theme style');
    for (const key of ['bold', 'dim', 'italic', 'underline', 'inverse', 'strikethrough']) if (options[key] !== undefined && typeof options[key] !== 'boolean') invalid(`theme style ${key}`);
    const ansi = (value, map, convert) => value === undefined ? undefined : typeof value === 'string' ? this.token(map, value) : convert(concreteColor(value), this.mode);
    const resolved = { ...options, ...(this.dimTokens.has(options.fg) ? { dim: true } : {}) };
    for (const key of ['bold', 'dim', 'italic', 'underline', 'inverse', 'strikethrough']) if (!this.capabilities[key]) resolved[key] = false;
    // Validate even unsupported color values, but emit no color SGR on a
    // terminal whose authoritative native profile disables color.
    const fg = ansi(options.fg, this.fgAnsi, foregroundAnsi), bg = ansi(options.bg, this.bgAnsi, backgroundAnsi);
    return styleTextWithAnsi(text, this.capabilities.color === 'none' ? undefined : fg,
      this.capabilities.color === 'none' ? undefined : bg, resolved);
  }
  bold(text) { return this.capabilities.bold ? styled(1, 22, text) : String(text); }
  dim(text) { return this.capabilities.dim ? styled(2, 22, text) : String(text); }
  italic(text) { return this.capabilities.italic ? styled(3, 23, text) : String(text); }
  underline(text) { return this.capabilities.underline ? styled(4, 24, text) : String(text); }
  inverse(text) { return this.capabilities.inverse ? styled(7, 27, text) : String(text); }
  strikethrough(text) { return this.capabilities.strikethrough ? styled(9, 29, text) : String(text); }
  getThinkingBorderColor(level) {
    const role = { off: 'thinkingOff', minimal: 'thinkingMinimal', low: 'thinkingLow', medium: 'thinkingMedium', high: 'thinkingHigh', xhigh: 'thinkingXhigh', max: 'thinkingMax' }[level] ?? 'thinkingOff';
    return text => this.fg(role, text);
  }
  getBashModeBorderColor() { return text => this.fg('bashMode', text); }
}
export function createThemeFromJson(input, mode = 'truecolor', sourcePath) {
  const data = parsePiTheme(input);
  const select = names => Object.fromEntries(names.map(name => [name, data.colors[name]]));
  return new Theme(select(foregroundTokens), select(backgroundTokens), mode, { name: data.name, sourcePath, appearance: data.appearance });
}
export function loadThemeFromPath(path, mode = 'truecolor') { return createThemeFromJson(readPiTheme(path), mode, path); }
const fgColors = Object.fromEntries(Object.entries(palette).map(([name, sgr]) => [name, sgr === 39 ? '' : sgr >= 90 ? sgr - 90 + 8 : sgr - 30]));
Object.assign(fgColors, { thinkingOff: 8, thinkingMinimal: 8, thinkingLow: 6, thinkingMedium: 6, thinkingHigh: 5, thinkingXhigh: 5, bashMode: 3 });
const bgColors = { selectedBg: 4, userMessageBg: '', customMessageBg: '', toolPendingBg: '', toolSuccessBg: '', toolErrorBg: '' };
const compatibilityTheme = () => new Theme(fgColors, bgColors, 'truecolor', { name: 'octet-pi-compat' });
// One bridge process, including native ESM and jiti module graphs. This is Pi
// presentation data, not a host palette/selection capability. Retained contexts
// and helper callbacks keep the same facade when its palette is replaced.
const state = globalThis[Symbol.for('octet.pi-compat.theme')] ??= { current: compatibilityTheme() };
const nativeThemes = new WeakMap();
export function createNativeTheme(palette) {
  fields(palette, ['name', 'path', 'appearance', 'colors', 'foregrounds', 'backgrounds', 'capabilities'], 'native theme palette');
  if (!['dark', 'light'].includes(palette.appearance)) invalid('native theme appearance');
  const data = parsePiTheme({ name: palette.name, appearance: palette.appearance, colors: palette.colors });
  for (const token of [...foregroundTokens, ...backgroundTokens]) if (!Object.hasOwn(palette.colors, token)) invalid('native theme missing resolved color');
  fields(palette.capabilities, ['color', 'bold', 'dim', 'italic', 'underline', 'inverse', 'strikethrough'], 'native theme capabilities');
  if (!['truecolor', '256color', '16color', 'none'].includes(palette.capabilities.color)) invalid('native theme color capability');
  fields(palette.foregrounds, foregroundTokens, 'native theme foregrounds');
  fields(palette.backgrounds, backgroundTokens, 'native theme backgrounds');
  const dim = [];
  for (const token of foregroundTokens) {
    const role = palette.foregrounds[token];
    fields(role, ['color', 'dim'], 'native theme foreground');
    if (typeof role.dim !== 'boolean' || role.color !== data.colors[token]) invalid('native theme foreground identity');
    if (role.dim) dim.push(token);
  }
  for (const token of backgroundTokens) if (palette.backgrounds[token] !== data.colors[token]) invalid('native theme background identity');
  if (palette.path != null) bounded(palette.path, 'native theme path', 4096);
  const mode = ['256color', '16color'].includes(palette.capabilities.color) ? '256color' : 'truecolor';
  return new Theme(Object.fromEntries(foregroundTokens.map(token => [token, data.colors[token]])),
    Object.fromEntries(backgroundTokens.map(token => [token, data.colors[token]])), mode,
    { name: data.name, sourcePath: palette.path ?? undefined, appearance: data.appearance, dim, capabilities: palette.capabilities });
}
export function nativeTheme(palette) {
  if (!palette || typeof palette !== 'object') unsupported('ctx.ui.theme', 'authoritative palette unavailable');
  let value = nativeThemes.get(palette);
  if (!value) { value = createNativeTheme(palette); nativeThemes.set(palette, value); }
  return value;
}
export function bindHostTheme(runtime) {
  state.resolve = () => {
    const store = runtime.scope.getStore();
    if (!store?.state || !Object.hasOwn(store.state.host, 'theme')) return state.current;
    runtime.assertSessionOwner(store);
    return nativeTheme(store.state.host.theme);
  };
}
function themeFacade(resolve = () => state.resolve?.() ?? state.current) {
  const methods = new Map();
  const selectList = Object.fromEntries(Object.keys(state.current.selectList).map(key => [key, (...args) => resolve().selectList[key](...args)]));
  const facade = strict(new Proxy({}, {
    has: (_target, key) => Reflect.has(resolve(), key),
    get(_target, key) {
      if (key === 'selectList') return selectList;
      const value = Reflect.get(resolve(), key);
      if (typeof value !== 'function') return value;
      if (!methods.has(key)) methods.set(key, (...args) => Reflect.apply(resolve()[key], facade, args));
      return methods.get(key);
    },
  }), 'theme');
  return facade;
}
export const theme = state.facade ??= themeFacade();
export function contextTheme(runtime, store) {
  return themeFacade(() => {
    runtime.assertSessionOwner(store);
    return Object.hasOwn(store.state.host, 'theme') ? nativeTheme(store.state.host.theme) : state.current;
  });
}
export const onBridgeThemeChange = callback => { state.onChange = callback; };
// Internal startup binding, deliberately not exposed as ctx.ui.setTheme or a
// Pi initTheme replacement. Only an explicitly configured snapshot is read.
export function configureBridgeTheme(selected) {
  let next;
  if (selected === undefined) next = compatibilityTheme();
  else {
    fields(selected, ['name', 'path', 'native_name', 'native_path'], 'imported Pi theme');
    bounded(selected.name, 'imported Pi theme name', 128);
    bounded(selected.path, 'imported Pi theme path', 4096);
    if (!isAbsolute(selected.path)) invalid('imported Pi theme path must be absolute');
    next = loadThemeFromPath(selected.path);
    if (next.name !== selected.name) invalid('imported Pi theme name differs from the reviewed snapshot');
  }
  state.current = next;
  state.onChange?.();
}
// Pi 1.0.2's admitted native helpers read these two process-local theme slots.
// Binding them does not run initTheme, read Pi settings, or start a watcher.
export function bindInstalledPiTheme() {
  for (const scope of ['@earendil-works', '@mariozechner']) globalThis[Symbol.for(`${scope}/pi-coding-agent:theme`)] = theme;
}
// Read the owning context on every call: retained components observe native
// reloads without sharing bindings with a different session owner. No Pi/local
// defaults are substituted for a missing host snapshot or an unbound action.
export function hostKeybindings(snapshot) {
  const keysFor = action => {
    const bindings = snapshot();
    if (!bindings || !Object.hasOwn(bindings, action)) unsupported(`keybindings.${action}`, 'host binding not supplied');
    const keys = bindings[action];
    if (!Array.isArray(keys) || keys.some(key => typeof key !== 'string')) invalid('host keybinding snapshot');
    return keys;
  };
  return strict({
    matches(data, action) { return keysFor(action).some(key => matchesKey(data, key)); },
    getKeys(action) { return [...keysFor(action)]; },
    getResolvedBindings() { return plainJSON(snapshot(), 'host keybindings'); },
    getEffectiveConfig() { return plainJSON(snapshot(), 'host keybindings'); },
  }, 'keybindings');
}
