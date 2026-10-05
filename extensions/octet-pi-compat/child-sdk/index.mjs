// A real package root for Pi's package.json/bin lookup. The adapter loader must
// explicitly alias supported Pi imports here; this is never installed globally.
export * from '../shims/coding-agent.mjs';
export {
  createAgentSession, AgentSession, SessionManager, DefaultResourceLoader, SettingsManager,
  createCodingTools, codingTools, readTool, bashTool, editTool, writeTool, searchTool,
  createReadTool, createBashTool, createEditTool, createWriteTool, createSearchTool,
  createGrepTool, createFindTool, createLsTool,
} from '../shims/child-sdk.mjs';
