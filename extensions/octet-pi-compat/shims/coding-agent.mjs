import { join } from 'node:path';
import { getPackageDir } from '../lib/pi-config.mjs';
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
  DefaultResourceLoader, SettingsManager, createGrepTool, createFindTool, createLsTool,
  createPowerShellTool, createReadOnlyTools } from './child-sdk.mjs';
export { createReadToolDefinition, createEditToolDefinition, createWriteToolDefinition, createBashToolDefinition,
  createPowerShellToolDefinition, createGrepToolDefinition, createFindToolDefinition, createLsToolDefinition } from '../lib/builtin-tools.mjs';
export { ExtensionRunner } from '../lib/extension-runner.mjs';
export class ModelRegistry { constructor() { unsupported('ModelRegistry', 'use the host-owned ctx.modelRegistry'); } }
export class AuthStorage { constructor() { unsupported('AuthStorage', 'provider credentials remain host-owned'); } }

import { localFunction, localClass, refused } from './local-pi.mjs';
export { Theme } from '../lib/theme.mjs';
export const AgentSessionRuntime = refused('pi-coding-agent.AgentSessionRuntime');
export const ArminComponent = localClass('coding-agent', 'modes/interactive/components/armin.js', 'ArminComponent');
export const AssistantMessageComponent = localClass('coding-agent', 'modes/interactive/components/assistant-message.js', 'AssistantMessageComponent');
export const BashExecutionComponent = localClass('coding-agent', 'modes/interactive/components/bash-execution.js', 'BashExecutionComponent');
export const BranchSummaryMessageComponent = localClass('coding-agent', 'modes/interactive/components/branch-summary-message.js', 'BranchSummaryMessageComponent');
export const CompactionSummaryMessageComponent = localClass('coding-agent', 'modes/interactive/components/compaction-summary-message.js', 'CompactionSummaryMessageComponent');
export class CredentialSynchronizationError extends Error {
  constructor(providerId, operation, credential, options) {
    super(`Credential ${operation} committed for ${providerId}, but local synchronization failed`, options);
    this.name = 'CredentialSynchronizationError';
    this.providerId = providerId;
    this.operation = operation;
    this.credential = credential;
  }
}
export const CustomMessageComponent = localClass('coding-agent', 'modes/interactive/components/custom-message.js', 'CustomMessageComponent');
export const DEFAULT_COMPACTION_SETTINGS = { enabled: true, reserveTokens: 16384, keepRecentTokens: 20000 };
export const DefaultPackageManager = refused('pi-coding-agent.DefaultPackageManager');
export const ExtensionEditorComponent = refused('pi-coding-agent.ExtensionEditorComponent');
export const ExtensionInputComponent = localClass('coding-agent', 'modes/interactive/components/extension-input.js', 'ExtensionInputComponent');

export const ExtensionSelectorComponent = localClass('coding-agent', 'modes/interactive/components/extension-selector.js', 'ExtensionSelectorComponent');
export const FooterComponent = localClass('coding-agent', 'modes/interactive/components/footer.js', 'FooterComponent');
export const InteractiveMode = refused('pi-coding-agent.InteractiveMode');
export const LoginDialogComponent = refused('pi-coding-agent.LoginDialogComponent');
export const ModelRuntime = refused('pi-coding-agent.ModelRuntime');
export const ModelSelectorComponent = refused('pi-coding-agent.ModelSelectorComponent');
export const OAuthSelectorComponent = refused('pi-coding-agent.OAuthSelectorComponent');
export const ProjectTrustStore = refused('pi-coding-agent.ProjectTrustStore');
export const RpcClient = refused('pi-coding-agent.RpcClient');
export const SessionSelectorComponent = refused('pi-coding-agent.SessionSelectorComponent');
export const SettingsSelectorComponent = localClass('coding-agent', 'modes/interactive/components/settings-selector.js', 'SettingsSelectorComponent');
export const ShowImagesSelectorComponent = localClass('coding-agent', 'modes/interactive/components/show-images-selector.js', 'ShowImagesSelectorComponent');
export const SkillInvocationMessageComponent = localClass('coding-agent', 'modes/interactive/components/skill-invocation-message.js', 'SkillInvocationMessageComponent');
export const ThemeSelectorComponent = localClass('coding-agent', 'modes/interactive/components/theme-selector.js', 'ThemeSelectorComponent');
export const ThinkingSelectorComponent = localClass('coding-agent', 'modes/interactive/components/thinking-selector.js', 'ThinkingSelectorComponent');
export const ToolExecutionComponent = refused('pi-coding-agent.ToolExecutionComponent');
export const TreeSelectorComponent = localClass('coding-agent', 'modes/interactive/components/tree-selector.js', 'TreeSelectorComponent');
export const UserMessageComponent = localClass('coding-agent', 'modes/interactive/components/user-message.js', 'UserMessageComponent');
export const UserMessageSelectorComponent = localClass('coding-agent', 'modes/interactive/components/user-message-selector.js', 'UserMessageSelectorComponent');
export const VIRTUAL_MODEL_STATE_ENTRY = 'pi.virtual-model-state';
export const buildContextEntries = localFunction('coding-agent', 'core/session-manager.js', 'buildContextEntries');
export const buildSessionProjection = localFunction('coding-agent', 'core/session-manager.js', 'buildSessionProjection');
export const collectEntriesForBranchSummary = localFunction('coding-agent', 'core/compaction/branch-summarization.js', 'collectEntriesForBranchSummary');
export const compact = refused('pi-coding-agent.compact');
export const convertToPng = localFunction('coding-agent', 'utils/image-convert.js', 'convertToPng');
export const copyToClipboard = refused('pi-coding-agent.copyToClipboard');
export const createAgentSessionFromServices = refused('pi-coding-agent.createAgentSessionFromServices');
export const createAgentSessionRuntime = refused('pi-coding-agent.createAgentSessionRuntime');
export const createAgentSessionServices = refused('pi-coding-agent.createAgentSessionServices');

export const createCodemodeExtension = refused('pi-coding-agent.createCodemodeExtension');

export const createEventBus = localFunction('coding-agent', 'core/event-bus.js', 'createEventBus');
export const createExtensionRuntime = refused('pi-coding-agent.createExtensionRuntime');


export const createLocalBashOperations = refused('pi-coding-agent.createLocalBashOperations');
export const createLocalPowerShellOperations = refused('pi-coding-agent.createLocalPowerShellOperations');

export const createMcpExtension = refused('pi-coding-agent.createMcpExtension');

export const createSyntheticSourceInfo = localFunction('coding-agent', 'core/source-info.js', 'createSyntheticSourceInfo');
export const createToolSearchExtension = refused('pi-coding-agent.createToolSearchExtension');

export const detectSupportedImageMimeTypeFromFile = localFunction('coding-agent', 'utils/mime.js', 'detectSupportedImageMimeTypeFromFile');
export const discoverAndLoadExtensions = refused('pi-coding-agent.discoverAndLoadExtensions');
export const findCutPoint = localFunction('coding-agent', 'core/compaction/compaction.js', 'findCutPoint');
export const findTurnStartIndex = localFunction('coding-agent', 'core/compaction/compaction.js', 'findTurnStartIndex');
export const formatDimensionNote = localFunction('coding-agent', 'utils/image-resize.js', 'formatDimensionNote');
export const generateBranchSummary = refused('pi-coding-agent.generateBranchSummary');
export const generateDiffString = localFunction('coding-agent', 'core/tools/edit-diff.js', 'generateDiffString');
export const generateSummary = refused('pi-coding-agent.generateSummary');
export const generateSummaryWithUsage = refused('pi-coding-agent.generateSummaryWithUsage');
export const generateUnifiedPatch = localFunction('coding-agent', 'core/tools/edit-diff.js', 'generateUnifiedPatch');
export const getDocsPath = () => join(getPackageDir(), 'docs');
export const getExamplesPath = () => join(getPackageDir(), 'examples');
export const getLastAssistantUsage = localFunction('coding-agent', 'core/compaction/compaction.js', 'getLastAssistantUsage');
export const getLatestCompactionEntry = localFunction('coding-agent', 'core/session-manager.js', 'getLatestCompactionEntry');
export const getPowerShellConfig = refused('pi-coding-agent.getPowerShellConfig');
export const getShellConfig = refused('pi-coding-agent.getShellConfig');
export const hasTrustRequiringProjectResources = localFunction('coding-agent', 'core/trust-manager.js', 'hasTrustRequiringProjectResources');
export const highlightCode = localFunction('coding-agent', 'modes/interactive/theme/theme.js', 'highlightCode');
export const initTheme = refused('pi-coding-agent.initTheme');
export const loadProjectContextFiles = localFunction('coding-agent', 'core/resource-loader.js', 'loadProjectContextFiles');
export const loadSkills = localFunction('coding-agent', 'core/skills.js', 'loadSkills');
export const loadSkillsFromDir = localFunction('coding-agent', 'core/skills.js', 'loadSkillsFromDir');
export const main = refused('pi-coding-agent.main');
export const migrateSessionEntries = localFunction('coding-agent', 'core/session-manager.js', 'migrateSessionEntries');
export const parseArgs = localFunction('coding-agent', 'cli/args.js', 'parseArgs');
export const parseSessionEntries = localFunction('coding-agent', 'core/session-manager.js', 'parseSessionEntries');
export const prepareBranchEntries = localFunction('coding-agent', 'core/compaction/branch-summarization.js', 'prepareBranchEntries');
export const readStoredCredential = refused('pi-coding-agent.readStoredCredential');
export const renderDiff = localFunction('coding-agent', 'modes/interactive/components/diff.js', 'renderDiff');
export const resizeImage = localFunction('coding-agent', 'utils/image-resize.js', 'resizeImage');
export const resolveCliModel = localFunction('coding-agent', 'core/model-resolver.js', 'resolveCliModel');
export const resolveModelScopeWithDiagnostics = localFunction('coding-agent', 'core/model-resolver.js', 'resolveModelScopeWithDiagnostics');
export const runPrintMode = refused('pi-coding-agent.runPrintMode');
export const runRpcMode = refused('pi-coding-agent.runRpcMode');
export const sessionEntryToContextMessages = localFunction('coding-agent', 'core/session-manager.js', 'sessionEntryToContextMessages');
export const shouldCompact = localFunction('coding-agent', 'core/compaction/compaction.js', 'shouldCompact');
export const truncateToVisualLines = localFunction('coding-agent', 'modes/interactive/components/visual-truncate.js', 'truncateToVisualLines');
export const wrapRegisteredTool = refused('pi-coding-agent.wrapRegisteredTool');
export const wrapRegisteredTools = refused('pi-coding-agent.wrapRegisteredTools');
export function parseSkillBlock(text) {
  const header = text.match(/^<skill name="([^"]+)" location="([^"]+)">\n/);
  if (!header) return null;
  const closing = '\n</skill>';
  let end = header[0].length;
  while ((end = text.indexOf(closing, end)) !== -1) {
    const after = end + closing.length;
    if (after === text.length || (text.startsWith('\n\n', after) && after + 2 < text.length)) {
      return { name: header[1], location: header[2], content: text.slice(header[0].length, end),
        userMessage: text.slice(after + 2).trim() || undefined };
    }
    end = after;
  }
  return null;
}
export { getLanguageFromPath, formatSkillsForPrompt } from './pi-1.0.2/coding-pure.mjs';
