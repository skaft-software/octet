// Host facades and pure utilities only; never import the Pi coding-agent runtime.
export { calculateContextTokens, estimateTokens, buildSessionContext } from '../lib/context.mjs';
import { unsupported } from '../lib/errors.mjs';
import { Editor } from '../node_modules/@earendil-works/pi-tui/dist/components/editor.js';
import { theme } from '../lib/theme.mjs';
export const defineTool = definition => definition;
export const getEditorTheme = () => ({ borderColor: theme.borderColor, selectList: theme.selectList });
export const getSelectListTheme = () => theme.selectList;
export const getMarkdownTheme = () => ({
  heading: text => theme.fg('mdHeading', text), link: text => theme.fg('mdLink', text), linkUrl: text => theme.fg('mdLinkUrl', text),
  code: text => theme.fg('mdCode', text), codeBlock: text => theme.fg('mdCodeBlock', text), codeBlockBorder: text => theme.fg('mdCodeBlockBorder', text),
  quote: text => theme.fg('mdQuote', text), quoteBorder: text => theme.fg('mdQuoteBorder', text), hr: text => theme.fg('mdHr', text), listBullet: text => theme.fg('mdListBullet', text),
  bold: theme.bold, italic: theme.italic, strikethrough: theme.strikethrough, underline: theme.underline,
});
export class CustomEditor extends Editor {
  constructor(tui, editorTheme, keybindings, options) {
    super(tui, editorTheme, options); this.keybindings = keybindings;
  }
}
// Named imports resolve to explicit refusals instead of any SDK fallback.
export { createAgentSession, AgentSession, SessionManager, createCodingTools, codingTools, readTool, bashTool, editTool, writeTool, searchTool,
  createReadTool, createBashTool, createEditTool, createWriteTool, createSearchTool,
  DefaultResourceLoader, SettingsManager, createGrepTool, createFindTool, createLsTool } from './child-sdk.mjs';
export class ModelRegistry { constructor() { unsupported('ModelRegistry', 'use the host-owned ctx.modelRegistry'); } }
export class AuthStorage { constructor() { unsupported('AuthStorage', 'provider credentials remain host-owned'); } }
