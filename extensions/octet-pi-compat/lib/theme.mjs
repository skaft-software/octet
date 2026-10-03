import { strict, unsupported } from './errors.mjs';
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
const style = (code, text) => `\x1b[${code}m${text}\x1b[0m`;
const fg = (role, text) => {
  if (!(role in palette)) unsupported(`theme.fg(${role})`, 'unknown compatibility palette role');
  return style(palette[role], text);
};
export const theme = strict({
  name: 'octet-pi-compat', fg,
  bg(role, text) {
    const colors = { selectedBg: 44, userMessageBg: 49, customMessageBg: 49, toolPendingBg: 49, toolSuccessBg: 49, toolErrorBg: 49 };
    if (!(role in colors)) unsupported(`theme.bg(${role})`, 'unknown compatibility palette role');
    return style(colors[role], text);
  },
  bold: text => style(1, text), dim: text => style(2, text), italic: text => style(3, text),
  underline: text => style(4, text), inverse: text => style(7, text), strikethrough: text => style(9, text),
  borderColor: text => fg('border', text),
  selectList: {
    selectedPrefix: text => fg('accent', text), selectedText: text => fg('accent', text),
    description: text => fg('muted', text), scrollInfo: text => fg('dim', text), noMatch: text => fg('warning', text),
  },
}, 'theme');
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
