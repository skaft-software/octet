// Host facades and pure utilities only; never import the Pi coding-agent runtime.
export { calculateContextTokens, estimateTokens, buildSessionContext } from '../lib/context.mjs';
export { CURRENT_SESSION_VERSION, getPackageDir, getPackageJsonPath, getReadmePath } from '../lib/pi-config.mjs';
export { keyHint, keyText, rawKeyHint } from '../lib/keybinding-hints.mjs';
export { VERSION, CONFIG_DIR_NAME, getAgentDir, isToolCallEventType, isBashToolResult, isPowerShellToolResult, isReadToolResult, isEditToolResult,
  isWriteToolResult, isGrepToolResult, isFindToolResult, isLsToolResult, parseFrontmatter, stripFrontmatter,
  withFileMutationQueue, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES, formatSize, truncateHead, truncateTail,
  truncateLine, convertToLlm, serializeConversation } from '../lib/public-helpers.mjs';
import { unsupported } from '../lib/errors.mjs';
export { CustomEditor } from '../lib/custom-editor.mjs';
import { theme } from '../lib/theme.mjs';
export const defineTool = definition => definition;
export const getEditorTheme = () => ({ borderColor: theme.borderColor, selectList: theme.selectList });
export const getSelectListTheme = () => theme.selectList;
export { BorderedLoader, DynamicBorder, getSettingsListTheme } from '../lib/ui-api.mjs';
export const getMarkdownTheme = () => ({
  heading: text => theme.fg('mdHeading', text), link: text => theme.fg('mdLink', text), linkUrl: text => theme.fg('mdLinkUrl', text),
  code: text => theme.fg('mdCode', text), codeBlock: text => theme.fg('mdCodeBlock', text), codeBlockBorder: text => theme.fg('mdCodeBlockBorder', text),
  quote: text => theme.fg('mdQuote', text), quoteBorder: text => theme.fg('mdQuoteBorder', text), hr: text => theme.fg('mdHr', text), listBullet: text => theme.fg('mdListBullet', text),
  bold: theme.bold, italic: theme.italic, strikethrough: theme.strikethrough, underline: theme.underline,
});
// Named imports resolve to explicit refusals instead of any SDK fallback.
export { createAgentSession, AgentSession, SessionManager, createCodingTools, codingTools, readTool, bashTool, editTool, writeTool,
  createReadTool, createBashTool, createEditTool, createWriteTool,
  DefaultResourceLoader, SettingsManager, createGrepTool, createFindTool, createLsTool } from './child-sdk.mjs';
export class ModelRegistry { constructor() { unsupported('ModelRegistry', 'use the host-owned ctx.modelRegistry'); } }
export class AuthStorage { constructor() { unsupported('AuthStorage', 'provider credentials remain host-owned'); } }
