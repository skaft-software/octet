import { createContext } from './api.mjs';
import { builtinArguments } from './builtin-arguments.mjs';
import { getAllTools, setActiveTools, toolSnapshot, validateTool } from './tools.mjs';
import { bounded, invalid, unsupported } from './errors.mjs';
import { currentToolHost, executeHostTool, prepareHostTool, settledHostTool } from './tool-host.mjs';

const shared = globalThis[Symbol.for('octet.pi.hostedExtensionRunner')] ??= (() => {
  const token = {}, catalogs = new WeakMap();
  class ExtensionRunner {
    #runtime;
    #views = new WeakMap();
    #originals = new WeakMap();
    constructor(secret, runtime) {
      if (secret !== token) unsupported('pi-coding-agent.ExtensionRunner', 'operation is not available through this host-owned shim');
      this.#runtime = runtime;
      catalogs.set(this, () => this.#catalog());
    }
    getAllRegisteredTools() {
      return [...this.#runtime.tools].map(([name, entry]) => {
        let view = this.#views.get(entry);
        if (!view) {
          const definition = Object.create(entry.definition);
          Object.defineProperty(definition, 'execute', { enumerable: true, value: (id, args, signal, onUpdate) => {
            const { runtime } = currentToolHost();
            if (runtime !== this.#runtime) invalid('tool belongs to another runner');
            return executeHostTool(name, args, { toolCallId: id, signal, onUpdate });
          } });
          view = { definition, sourceInfo: { path: this.#runtime.entrypoint(entry.factory), source: 'extension', scope: 'temporary', origin: 'top-level' } };
          this.#views.set(entry, view);
          this.#originals.set(definition, entry.definition);
        }
        return view;
      });
    }
    #catalog() {
      const tools = this.getAllRegisteredTools();
      if (!Array.isArray(tools) || tools.length > 256) invalid('registered tool catalog');
      const seen = new Set(), entries = new Map();
      for (const tool of tools) {
        const definition = this.#originals.get(tool?.definition) ?? tool?.definition;
        validateTool(definition);
        const entry = this.#runtime.tools.get(definition.name);
        if (!entry || seen.has(definition.name) || tool.sourceInfo?.path !== this.#runtime.entrypoint(entry.factory)) invalid('registered tool catalog identity');
        seen.add(definition.name);
        entries.set(definition.name, { ...entry, definition });
      }
      return entries;
    }
    createContext() {
      const store = this.#runtime.scope.getStore();
      this.#runtime.assertOwner(store);
      return createContext(this.#runtime, store);
    }
    getActiveTools() { return [...toolSnapshot(this.#runtime, this.#runtime.scope.getStore()?.factory).active_tools]; }
    getAllTools() { return getAllTools(this.#runtime, this.#runtime.scope.getStore()?.factory); }
    setActiveTools(names) { return setActiveTools(this.#runtime, this.#runtime.scope.getStore()?.factory, names); }
    async emit(event) {
      const { runtime, store } = currentToolHost();
      if (runtime !== this.#runtime) invalid('event belongs to another runner');
      if (!['tool_execution_start', 'tool_execution_update', 'tool_execution_end'].includes(event?.type)) unsupported('ExtensionRunner.emit', 'only admitted nested tool lifecycle events are available');
      bounded(event.toolCallId, 'tool call id', 256);
      bounded(event.toolName, 'tool name', 128);
      await runtime.runEvent(event.type, event, store);
    }
    async emitToolCall(event) {
      const { runtime } = currentToolHost();
      if (runtime !== this.#runtime || event?.type !== 'tool_call') invalid('tool preflight event');
      const builtin = !runtime.tools.has(event.toolName);
      return prepareHostTool(event, builtin, builtin ? (args, workspace) => builtinArguments(event.toolName, args, undefined, workspace) : undefined);
    }
    async emitToolResult(event) {
      const { runtime } = currentToolHost();
      if (runtime !== this.#runtime || event?.type !== 'tool_result') invalid('tool result event');
      return settledHostTool(event);
    }
  }
  return { ExtensionRunner, create: runtime => new ExtensionRunner(token, runtime), catalog: runner => catalogs.get(runner)() };
})();

export const ExtensionRunner = shared.ExtensionRunner;
export const createHostedRunner = runtime => shared.create(runtime);
export const hostedToolCatalog = runner => shared.catalog(runner);
