import { closeSync, constants, fstatSync, openSync, readSync } from 'node:fs';
import { bounded, fields, invalid, plainJSON, rpcError } from './errors.mjs';
import { colorToHex, parseColor } from './theme-colors.mjs';

// Pi 1.0 theme-json.ts/theme.ts, 581e7ba78141a4d8b61cc9d11b8b22ae7e59195e.
// Copyright (c) 2025 Mario Zechner. MIT, see ../LICENSE.pi.
export const foregroundTokens = Object.freeze([
  'accent', 'border', 'borderAccent', 'borderMuted', 'success', 'error', 'warning', 'muted', 'dim', 'text', 'thinkingText',
  'scrollbarTrack', 'scrollbarThumb', 'searchMatchText', 'userMessageText', 'customMessageText', 'customMessageLabel',
  'toolTitle', 'toolOutput', 'mdHeading', 'mdLink', 'mdLinkUrl', 'mdCode', 'mdCodeBlock', 'mdCodeBlockBorder',
  'mdQuote', 'mdQuoteBorder', 'mdHr', 'mdListBullet', 'toolDiffAdded', 'toolDiffRemoved', 'toolDiffContext',
  'syntaxComment', 'syntaxKeyword', 'syntaxFunction', 'syntaxVariable', 'syntaxString', 'syntaxNumber',
  'syntaxType', 'syntaxOperator', 'syntaxPunctuation', 'thinkingOff', 'thinkingMinimal', 'thinkingLow',
  'thinkingMedium', 'thinkingHigh', 'thinkingXhigh', 'thinkingMax', 'bashMode',
]);
export const backgroundTokens = Object.freeze(['selectedBg', 'searchMatchBg', 'userMessageBg', 'customMessageBg', 'toolPendingBg', 'toolSuccessBg', 'toolErrorBg']);
const tokens = [...foregroundTokens, ...backgroundTokens];
const fallbacks = Object.freeze({ scrollbarTrack: 'muted', scrollbarThumb: 'text', thinkingMax: 'thinkingXhigh', searchMatchBg: 'selectedBg', searchMatchText: 'text' });
const maxBytes = 262144;
function plain(text, label, max = 1024) {
  bounded(text, label, max, { controls: true });
  if (/[\x00-\x1f\x7f-\x9f]/u.test(text)) invalid(`${label} contains terminal controls`);
  return text;
}
function colorValue(value, label) {
  if (typeof value === 'number') {
    if (!Number.isInteger(value) || value < 0 || value > 255) invalid(`${label} must be an ANSI index from 0 to 255`);
  } else plain(value, label);
  return value;
}

/** Bounded data-only JSON parser. This never discovers files or activates a host theme. */
export function parsePiTheme(input) {
  if (typeof input === 'string') {
    bounded(input, 'Pi theme JSON', maxBytes, { controls: true });
    try { input = JSON.parse(input.replace(/^\uFEFF/u, '')); } catch { invalid('Pi theme must be JSON, not native TOML'); }
  }
  const document = plainJSON(input, 'Pi theme JSON', maxBytes);
  fields(document, ['$schema', 'name', 'appearance', 'vars', 'colors', 'export'], 'Pi theme');
  plain(document.name, 'Pi theme name', 128);
  if (document.name.includes('/')) invalid('Pi theme names cannot contain /');
  if (document.$schema !== undefined) plain(document.$schema, 'Pi theme schema', 4096);
  if (document.appearance !== undefined && !['dark', 'light'].includes(document.appearance)) invalid('Pi theme appearance');
  const vars = document.vars === undefined ? {} : document.vars;
  if (!vars || typeof vars !== 'object' || Array.isArray(vars)) invalid('Pi theme vars must be an object');
  if (Object.keys(vars).length > 256) throw rpcError(-32602, 'bounds_exceeded Pi theme variables');
  for (const [key, value] of Object.entries(vars)) { plain(key, 'Pi theme variable', 128); colorValue(value, `Pi theme vars.${key}`); }
  fields(document.colors, tokens, 'Pi theme colors');
  const colors = { ...document.colors };
  for (const token of tokens) {
    if (!Object.hasOwn(colors, token)) {
      if (!fallbacks[token]) invalid(`Pi theme missing required color ${token}`);
      colors[token] = colors[fallbacks[token]];
    }
    colorValue(colors[token], `Pi theme colors.${token}`);
  }
  const resolve = (initial, label) => {
    let value = initial;
    const visited = new Set();
    while (typeof value === 'string' && value !== '' && !value.startsWith('#') && !/^ok(lch|hsl)\(/i.test(value)) {
      if (visited.has(value)) invalid(`Pi theme circular variable reference: ${value}`);
      visited.add(value);
      if (!Object.hasOwn(vars, value)) invalid(`Pi theme variable reference not found: ${value}`);
      value = vars[value];
    }
    if (value !== '') {
      try { parseColor(value); } catch (error) { invalid(`${label}: ${error.message}`); }
    }
    return value;
  };
  for (const token of tokens) colors[token] = resolve(colors[token], `Pi theme colors.${token}`);
  const exported = {};
  if (document.export !== undefined) {
    fields(document.export, ['pageBg', 'cardBg', 'infoBg'], 'Pi theme export');
    for (const [key, value] of Object.entries(document.export)) exported[key] = resolve(colorValue(value, `Pi theme export.${key}`), `Pi theme export.${key}`);
  }
  return Object.freeze({ name: document.name, ...(document.appearance === undefined ? {} : { appearance: document.appearance }),
    colors: Object.freeze(colors), export: Object.freeze(exported) });
}

/** Explicit path only; no HOME search, watcher, activation or trust change. */
export function readPiTheme(path) {
  plain(path, 'Pi theme path', 4096);
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile()) invalid('Pi theme path must be a regular file');
    if (stat.size > maxBytes) throw rpcError(-32602, 'bounds_exceeded Pi theme JSON');
    const bytes = Buffer.alloc(maxBytes + 1);
    let length = 0, count;
    while (length < bytes.length && (count = readSync(fd, bytes, length, bytes.length - length, null))) length += count;
    if (length > maxBytes) throw rpcError(-32602, 'bounds_exceeded Pi theme JSON');
    let text;
    try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)); }
    catch { invalid('Pi theme file must be UTF-8'); }
    return parsePiTheme(text);
  } finally { closeSync(fd); }
}

// Only existing native tokens with an actual semantic consumer are projected.
// Sources: sexy-tui-rs/src/theme/tokens.rs and coding-agent/src/tui/view/tern_theme.rs.
export const nativeTokenProjection = Object.freeze({
  accent: 'accent', border: 'border', borderAccent: 'border_focused', borderMuted: 'border_idle',
  success: 'success', error: 'error', warning: 'warning', muted: 'muted', dim: 'dim', text: 'foreground',
  thinkingText: 'reasoning_text', selectedBg: 'selected_bg', userMessageText: 'user_msg_text', userMessageBg: 'user_msg_bg',
  toolTitle: 'tool_title', toolOutput: 'tool_output', toolPendingBg: 'tool_pending_bg', toolSuccessBg: 'tool_success_bg', toolErrorBg: 'tool_error_bg',
  mdHeading: 'md_heading', mdLink: 'md_link', mdCode: 'md_code', mdCodeBlock: 'md_code_block', mdCodeBlockBorder: 'md_code_border',
  mdQuote: 'md_quote', mdQuoteBorder: 'md_quote_border', mdHr: 'md_hr', mdListBullet: 'md_list_bullet',
  toolDiffAdded: 'diff_added', toolDiffRemoved: 'diff_removed', toolDiffContext: 'diff_context',
  syntaxComment: 'syntax_comment', syntaxKeyword: 'syntax_keyword', syntaxFunction: 'syntax_function',
  syntaxVariable: 'syntax_variable', syntaxString: 'syntax_string', syntaxNumber: 'syntax_number', syntaxType: 'syntax_type',
  syntaxOperator: 'syntax_operator', syntaxPunctuation: 'syntax_punctuation',
});

/** Return an explicitly labeled, partial native TOML projection, never masquerading as Pi JSON.
 * The caller must choose a reviewed destination and use native discovery/admission.
 * Layout, host model accents, terminal queries, HTML export and unmapped Pi roles are NOT reproduced.
 */
export function piThemeToNativeToml(input) {
  const palette = parsePiTheme(input);
  plain(palette.name, 'native theme name', 80);
  const native = value => value === '' ? 'default' : typeof value === 'number' ? `index:${value}` : colorToHex(parseColor(value));
  const lines = ['# Partial palette projection from Pi JSON; not Pi theme format.', '[metadata]',
    `name = ${JSON.stringify(palette.name)}`, 'description = "Pi palette projection; layout and unmapped roles are not reproduced"',
    `terminal = ${JSON.stringify(palette.appearance ?? 'any')}`, 'adaptive = false', '', '[colors]'];
  for (const [pi, octet] of Object.entries(nativeTokenProjection)) lines.push(`${octet} = ${JSON.stringify(native(palette.colors[pi]))}`);
  const toml = lines.join('\n') + '\n';
  bounded(toml, 'native theme TOML', maxBytes);
  return Object.freeze({ sourceFormat: 'pi-theme-json', format: 'octet-theme-toml', toml,
    unmappedColors: Object.freeze(tokens.filter(token => !Object.hasOwn(nativeTokenProjection, token))),
    unmappedExport: Object.freeze(Object.keys(palette.export)) });
}
