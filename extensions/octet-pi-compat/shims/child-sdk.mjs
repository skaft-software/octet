// Importable Pi child SDK exports. Host runtime glue is explicit; there is no
// dependency on an upstream coding-agent package and no provider fallback.
export {
  createAgentSession, AgentSession, SessionManager,
  createCodingTools, codingTools, readTool, bashTool, editTool, writeTool,
  createReadTool, createBashTool, createEditTool, createWriteTool, createPowerShellTool,
  createGrepTool, createFindTool, createLsTool, createReadOnlyTools,
} from '../lib/children.mjs';
import { unsupported } from '../lib/errors.mjs';
export class DefaultResourceLoader { constructor() { unsupported('DefaultResourceLoader', 'child resource discovery, callbacks and system-prompt replacement are not yet bound'); } }
export { SettingsManager } from '../lib/settings-manager.mjs';
