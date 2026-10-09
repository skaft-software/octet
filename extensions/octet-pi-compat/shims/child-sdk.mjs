// Importable Pi child SDK exports. Host runtime glue is explicit; there is no
// dependency on an upstream coding-agent package and no provider fallback.
export {
  createAgentSession, AgentSession, SessionManager,
  createCodingTools, codingTools, readTool, bashTool, editTool, writeTool, searchTool,
  createReadTool, createBashTool, createEditTool, createWriteTool, createSearchTool,
} from '../lib/children.mjs';
import { unsupported } from '../lib/errors.mjs';
export class DefaultResourceLoader { constructor() { unsupported('DefaultResourceLoader', 'child resource discovery, callbacks and system-prompt replacement are not yet bound'); } }
export class SettingsManager {
  constructor() { unsupported('SettingsManager', 'native child settings inherit from the owning host'); }
  static create() { unsupported('SettingsManager.create', 'Pi settings files cannot replace host policy'); }
  static inMemory() { unsupported('SettingsManager.inMemory', 'native child settings inherit from the owning host'); }
}
export const createGrepTool = () => unsupported('createGrepTool', 'native search is not a transparent Pi grep implementation');
export const createFindTool = () => unsupported('createFindTool', 'native search is not a transparent Pi find implementation');
export const createLsTool = () => unsupported('createLsTool', 'native search is not a transparent Pi ls implementation');
