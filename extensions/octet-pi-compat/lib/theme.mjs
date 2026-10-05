import { fields, invalid, plainJSON, strict, unsupported } from './errors.mjs';
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
// Theme methods/fallbacks follow Pi 1.0 581e7ba...; MIT, see ../LICENSE.pi.
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
    fields(options, ['name', 'sourcePath', 'sourceInfo', 'appearance', 'dim'], 'theme options');
    if (!['truecolor', '256color'].includes(mode)) invalid('theme color mode');
    const data = parsePiTheme({ name: options.name ?? '', ...(options.appearance === undefined ? {} : { appearance: options.appearance }), colors: { ...fgColors, ...bgColors } });
    if (options.dim !== undefined && (!Array.isArray(options.dim) || options.dim.length > foregroundTokens.length || options.dim.some(token => !foregroundTokens.includes(token)))) invalid('theme dim tokens');
    this.name = options.name; this.sourcePath = options.sourcePath;
    this.sourceInfo = options.sourceInfo === undefined ? undefined : plainJSON(options.sourceInfo, 'theme sourceInfo');
    this.mode = mode; this.dimTokens = new Set(options.dim);
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
        map.set(token, value === '' ? `\x1b[${background ? 49 : 39}m` : (background ? backgroundAnsi : foregroundAnsi)(colors[token], mode));
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
  fg(role, text) { return `${this.getFgAnsi(role)}${text}${this.dimTokens.has(role) ? '\x1b[22;39m' : '\x1b[39m'}`; }
  bg(role, text) { return `${this.getBgAnsi(role)}${text}\x1b[49m`; }
  getFgAnsi(role) { return this.token(this.fgAnsi, role) + (this.dimTokens.has(role) ? '\x1b[2m' : ''); }
  getBgAnsi(role) { return this.token(this.bgAnsi, role); }
  getColorMode() { return this.mode; }
  style(text, options) {
    fields(options, ['fg', 'bg', 'bold', 'dim', 'italic', 'underline', 'inverse', 'strikethrough'], 'theme style');
    for (const key of ['bold', 'dim', 'italic', 'underline', 'inverse', 'strikethrough']) if (options[key] !== undefined && typeof options[key] !== 'boolean') invalid(`theme style ${key}`);
    const ansi = (value, map, convert) => value === undefined ? undefined : typeof value === 'string' ? this.token(map, value) : convert(concreteColor(value), this.mode);
    return styleTextWithAnsi(text, ansi(options.fg, this.fgAnsi, foregroundAnsi), ansi(options.bg, this.bgAnsi, backgroundAnsi),
      this.dimTokens.has(options.fg) ? { ...options, dim: true } : options);
  }
  bold(text) { return styled(1, 22, text); }
  dim(text) { return styled(2, 22, text); }
  italic(text) { return styled(3, 23, text); }
  underline(text) { return styled(4, 24, text); }
  inverse(text) { return styled(7, 27, text); }
  strikethrough(text) { return styled(9, 29, text); }
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
export const theme = strict(new Theme(fgColors, bgColors, 'truecolor', { name: 'octet-pi-compat' }), 'theme');
const shortcuts = {
  'tui.select.up': ['up'], 'tui.select.down': ['down'], 'tui.select.confirm': ['enter'],
  'tui.select.cancel': ['escape'], 'tui.input.submit': ['enter'],
  'selectUp': ['up'], 'selectDown': ['down'], 'selectConfirm': ['enter'], 'selectCancel': ['escape'],
};
export const keybindings = strict({
  matches(data, action) {
    const keys = shortcuts[action];
    if (!keys) unsupported(`keybindings.${action}`, 'host binding not supplied');
    return keys.some(key => matchesKey(data, key));
  },
  getKeys(action) {
    if (!shortcuts[action]) unsupported(`keybindings.${action}`, 'host binding not supplied');
    return [...shortcuts[action]];
  },
}, 'keybindings');
