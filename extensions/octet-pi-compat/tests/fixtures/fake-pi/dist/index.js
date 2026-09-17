// Deliberately tiny, deterministic stand-in for Pi's public extension loader.
// It implements only the public methods consumed by bridge.mjs. The aggregate
// hooks deliberately model ordered source loading and one shared event bus.

import { appendFileSync, existsSync, lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function fixtureDelay(name) {
  const value = Number.parseInt(process.env[name] ?? "0", 10);
  return Number.isSafeInteger(value) && value >= 0 ? value : 0;
}
const fixtureMode = process.env.OCTET_PI_FIXTURE_MODE ?? "";
const fixtureApiVersion = process.env.OCTET_PI_FIXTURE_API_VERSION ?? "0.2";
const fixtureEvents = (process.env.OCTET_PI_FIXTURE_EVENTS ?? "")
  .split(",")
  .map((event) => event.trim())
  .filter(Boolean);
const fixtureProviderRegistrationDelay = Number.parseInt(
  process.env.OCTET_PI_FIXTURE_PROVIDER_REGISTER_DELAY_MS ?? "0",
  10,
);
const providerRegistrationDelay = Number.isSafeInteger(fixtureProviderRegistrationDelay)
  && fixtureProviderRegistrationDelay >= 0
  ? fixtureProviderRegistrationDelay
  : 0;
const providerBeforeRequestDelay = fixtureDelay("OCTET_PI_FIXTURE_PROVIDER_BEFORE_REQUEST_DELAY_MS");
const providerAdapterDelay = fixtureDelay("OCTET_PI_FIXTURE_PROVIDER_ADAPTER_DELAY_MS");
const providerSetupMutation = process.env.OCTET_PI_FIXTURE_PROVIDER_SETUP_MUTATION ?? "";
const providerSetupMutationStage = process.env.OCTET_PI_FIXTURE_PROVIDER_SETUP_MUTATION_STAGE ?? "";
const fixtureProviderAuth = process.env.OCTET_PI_FIXTURE_PROVIDER_AUTH ?? "host_credential";
const fixtureInitialProviderCount = Number.parseInt(
  process.env.OCTET_PI_FIXTURE_INITIAL_PROVIDER_COUNT ?? "1",
  10,
);
const initialProviderCount = Number.isSafeInteger(fixtureInitialProviderCount)
  ? Math.max(1, Math.min(fixtureInitialProviderCount, 2))
  : 1;
let fixtureCancellationObserved = false;
let fixtureProviderSetupStage = "idle";
let fixtureProviderSetupAbortObserved = false;
let fixtureProviderLateIteratorClosed = false;
let fixtureProviderActions = null;
let fixtureProviderSetupMutationTriggered = false;
// Bounded observation records for fixture behaviour that the assertions must
// distinguish from its own timing: tool executions and marker-file effects.
const fixtureExecutionLog = [];
const FIXTURE_TOOL_USAGE = {
  input: 11,
  output: 7,
  cacheRead: 3,
  cacheWrite: 5,
  cacheWrite1h: 2,
  reasoning: 4,
  totalTokens: 26,
  // Pi encodes unpriced usage as all-zero cost; the bridge accepts it and
  // carries the counters natively.
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};
const FIXTURE_HOOK_USAGE = {
  input: 1,
  output: 2,
  cacheRead: 0,
  cacheWrite: 0,
  reasoning: 0,
  totalTokens: 3,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

function fixtureUsageCase(name) {
  switch (name) {
    case "negative":
      return { input: -1, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0 };
    case "missing":
      return { input: 1, output: 2 };
    case "unknown":
      return { ...FIXTURE_TOOL_USAGE, extra: 1 };
    case "cost-unknown":
      return { ...FIXTURE_TOOL_USAGE, cost: { ...FIXTURE_TOOL_USAGE.cost, extra: 1 } };
    case "cost-missing":
      return {
        ...FIXTURE_TOOL_USAGE,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      };
    case "cost-nan":
      return { ...FIXTURE_TOOL_USAGE, cost: { ...FIXTURE_TOOL_USAGE.cost, total: Number.NaN } };
    case "cost-negative":
      return { ...FIXTURE_TOOL_USAGE, cost: { ...FIXTURE_TOOL_USAGE.cost, total: -0.5 } };
    case "cost-not-a-number":
      return { ...FIXTURE_TOOL_USAGE, cost: { ...FIXTURE_TOOL_USAGE.cost, total: "0.5" } };
    case "cost-priced":
      return { ...FIXTURE_TOOL_USAGE, cost: { ...FIXTURE_TOOL_USAGE.cost, total: 0.0004 } };
    case "reasoning-exceeds":
      return { ...FIXTURE_TOOL_USAGE, output: 3, reasoning: 4 };
    case "cache-1h-exceeds":
      return { ...FIXTURE_TOOL_USAGE, cacheWrite: 1, cacheWrite1h: 2 };
    case "no-cost": {
      const { cost, ...rest } = FIXTURE_TOOL_USAGE;
      return rest;
    }    default:
      throw new Error(`unknown fixture usage case ${name}`);
  }
}

function fixtureMarkerPath() {
  return process.env.OCTET_PI_FIXTURE_MARKER ?? "";
}

function recordFixtureExecution(name, phase) {
  fixtureExecutionLog.push({ name, phase, at: Date.now() });
  if (fixtureExecutionLog.length > 64) fixtureExecutionLog.shift();
}

function fixtureModeForPath(path) {
  const value = String(path ?? "");
  if (value.endsWith("unsafe-provider-extension.mjs")) return "unsafe-provider";
  if (value.endsWith("provider-headers-hook-extension.mjs")) return "provider-headers-hook";
  if (value.endsWith("provider-extension.mjs")) return "provider";
  return fixtureMode || "default";
}

function mutateProviderDuringSetup(stage) {
  if (
    !fixtureProviderActions
    || fixtureProviderSetupMutationTriggered
    || providerSetupMutationStage !== stage
  ) return;
  fixtureProviderSetupMutationTriggered = true;
  if (providerSetupMutation === "update") {
    fixtureProviderActions.registerProvider("fixture-provider", fixtureProviderConfig(2));
  } else if (providerSetupMutation === "unregister") {
    fixtureProviderActions.unregisterProvider("fixture-provider");
  }
}

async function* fixtureProviderStream({ request, signal }) {
  if (request.direct === true) {
    yield { kind: "started", payload: { response_id: "fixture-direct-response" } };
    yield { kind: "text_start", payload: { index: 0 } };
    yield { kind: "text_delta", payload: { index: 0, delta: "direct" } };
    yield { kind: "text_end", payload: { index: 0 } };
    // The Pi spelling is normalized even for an API 0.3-shaped adapter event.
    yield { kind: "finished", payload: { stop_reason: "stop" } };
    return;
  }
  yield { type: "start", partial: { responseId: "fixture-response" } };
  yield { type: "text_start", contentIndex: 0 };
  yield {
    type: "text_delta",
    contentIndex: 0,
    delta: `${request.fixture_hook === true ? "hooked" : "unhooked"}:${request.prompt ?? ""}`,
  };
  if (request.hold === true) {
    await new Promise((resolve) => {
      if (signal.aborted) resolve();
      else signal.addEventListener("abort", resolve, { once: true });
    });
    fixtureCancellationObserved = signal.aborted;
    return;
  }
  yield { type: "text_end", contentIndex: 0 };
  yield {
    type: "toolcall_start",
    contentIndex: 1,
    toolCall: { type: "toolCall", id: "fixture-call", name: "fixture_echo", arguments: { value: "tool" } },
  };
  yield { type: "toolcall_delta", contentIndex: 1, delta: "{\"value\":\"tool\"}" };
  yield {
    type: "toolcall_end",
    contentIndex: 1,
    toolCall: { type: "toolCall", id: "fixture-call", name: "fixture_echo", arguments: { value: "tool" } },
  };
  yield {
    type: "done",
    reason: "stop",
    message: { usage: { input: 2, output: 3, cacheRead: 1, cacheWrite: 0, totalTokens: 5 } },
  };
}

async function fixtureProviderAdapter(input) {
  fixtureProviderSetupStage = "adapter";
  console.error("fixture provider setup adapter");
  mutateProviderDuringSetup("adapter");
  if (input.signal) {
    if (input.signal.aborted) fixtureProviderSetupAbortObserved = true;
    else input.signal.addEventListener("abort", () => {
      fixtureProviderSetupAbortObserved = true;
    }, { once: true });
  }
  // Deliberately do not cooperate with the AbortSignal while acquiring. This
  // fixture verifies that the bridge closes an adapter resource which appears
  // after a provider mutation has already fenced setup.
  if (providerAdapterDelay > 0) await sleep(providerAdapterDelay);
  const iterator = fixtureProviderStream(input);
  const close = iterator.return?.bind(iterator);
  iterator.return = async (...args) => {
    fixtureProviderLateIteratorClosed = true;
    fixtureProviderSetupStage = "adapter_iterator_closed";
    return close ? close(...args) : { done: true, value: undefined };
  };
  return iterator;
}

function fixtureProviderConfig(revision = 1, secondary = false) {
  const providerLabel = secondary ? "Fixture second provider" : "Fixture provider";
  const modelLabel = secondary ? "Fixture second model" : "Fixture model";
  return {
    name: revision === 1 ? providerLabel : `${providerLabel} refreshed`,
    api: "openai-completions",
    octetAuth: fixtureProviderAuth === "none"
      ? { kind: "none" }
      : { kind: "host_credential", subject: "fixture-credential" },
    models: [
      {
        id: secondary ? "fixture-second-model" : "fixture-model",
        name: revision === 1 ? modelLabel : `${modelLabel} refreshed`,
        contextWindow: 8192,
        maxTokens: revision === 1 ? 1024 : 2048,
        reasoning: false,
      },
    ],
    octetStream: fixtureProviderAdapter,
  };
}

function makeTool(name, execute) {
  return {
    definition: {
      name,
      label: name,
      description: `${name} fixture tool`,
      parameters: {
        type: "object",
        properties: { value: { type: "string" } },
        additionalProperties: false,
      },
      execute,
    },
    sourceInfo: { path: "fixture-extension.mjs" },
  };
}

export function createEventBus() {
  const listeners = new Map();
  return {
    on(event, handler) {
      const handlers = listeners.get(event) ?? [];
      handlers.push(handler);
      listeners.set(event, handlers);
    },
    emit(event, payload) {
      for (const handler of listeners.get(event) ?? []) handler(payload);
    },
  };
}

function fixtureExtension(path, mode) {
  const registration = fixtureMode === "registration";
  const handlers = new Map(fixtureEvents.map((event) => [event, []]));
  if (mode === "provider-headers-hook") handlers.set("before_provider_headers", [() => {}]);
  return {
    path,
    fixtureMode: mode,
    handlers,
    shortcuts: registration ? new Map([["ctrl+alt+p", {}]]) : new Map(),
    flags: registration ? new Map([["plan", { default: false }]]) : new Map(),
    messageRenderers: registration ? new Map([["fixture", {}]]) : new Map(),
    entryRenderers: registration ? new Map([["fixture", {}]]) : new Map(),
    markdownTransformer: registration ? (() => "fixture") : null,
  };
}

async function loadFixtureExtensions(paths, eventBus) {
  console.log("fixture loader wrote to console.log");
  process.stdout.write("fixture loader wrote directly to stdout\n");
  delete globalThis.__octetPiAggregateShared;
  const runtime = {
    aggregate: {
      loadOrder: [],
      eventOrder: [],
      globalMarker: null,
    },
  };
  const extensions = [];
  const errors = [];
  for (const path of paths) {
    try {
      const entrypoint = ["index.ts", "index.js", "index.mjs"]
        .map((name) => join(path, name)).find((entry) => existsSync(entry)) ?? path;
      const module = await import(pathToFileURL(entrypoint).href);
      if (typeof module.installFakePiAggregate === "function") {
        await module.installFakePiAggregate({ eventBus, runtime });
      }
      const extension = fixtureExtension(path, fixtureModeForPath(path));
      if (extension.fixtureMode === "ui-bridge") {
        // Opt-in fixture loader: execute the selected extension's real command
        // registration, rather than replacing its UI behavior with canned calls.
        extension.commands = new Map();
        await module.default({
          registerCommand(name, definition) {
            extension.commands.set(name, { ...definition, name });
          },
        });
      }
      extensions.push(extension);
    } catch (error) {
      errors.push({ path, error });
    }
  }
  return { extensions, runtime, errors };
}

// Model Pi's ambient discovery so tests catch executing an unselected source
// before the bridge's post-load aggregate count check.
export async function discoverAndLoadExtensions(paths, cwd, agentDir, eventBus) {
  const discovered = [];
  for (const directory of [join(cwd, ".pi", "extensions"), join(agentDir, "extensions")]) {
    if (!existsSync(directory)) continue;
    for (const name of readdirSync(directory)) {
      if (name.endsWith(".js")) discovered.push(join(directory, name));
    }
  }
  return loadFixtureExtensions([...discovered, ...paths], eventBus);
}

export class SettingsManager {
  static inMemory() {
    return { fixtureInMemory: true };
  }
}

// Deliberately limited fixture model of Pi's public inert package resolver.
// Production delegates manifest/glob semantics to the installed Pi runtime.
export class DefaultPackageManager {
  constructor(options) {
    if (options.settingsManager?.fixtureInMemory !== true) throw new Error("resolver requires in-memory settings");
  }
  async resolveExtensionSources(paths, options) {
    if (options?.temporary !== true) throw new Error("resolver requires explicit temporary sources");
    const extensions = paths.flatMap((path) => {
      if (!lstatSync(path).isDirectory()) return [{ path, enabled: true }];
      const manifestPath = join(path, "package.json");
      const manifest = existsSync(manifestPath)
        ? JSON.parse(readFileSync(manifestPath, "utf8").replace(/^\uFEFF/, "")) : null;
      return (manifest?.pi?.extensions ?? []).map((entry) => ({ path: resolve(path, entry), enabled: true }));
    });
    return { extensions };
  }
}

export class DefaultResourceLoader {
  constructor(options) {
    if (options.settingsManager?.fixtureInMemory !== true) {
      throw new Error("fixture requires in-memory settings, not ambient package configuration");
    }
    this.options = options;
  }

  async loadProjectTrustExtensions() {
    const { additionalExtensionPaths, cwd, agentDir, eventBus, noExtensions } = this.options;
    // Pi's package manager considers even a prompts-only directory a package,
    // suppressing its extension loader's root index fallback.
    const paths = additionalExtensionPaths.flatMap((path) => {
      if (!lstatSync(path).isDirectory()) return [path];
      const manifestPath = join(path, "package.json");
      const manifest = existsSync(manifestPath)
        ? JSON.parse(readFileSync(manifestPath, "utf8").replace(/^\uFEFF/, "")) : null;
      if (manifest?.pi) return (manifest.pi.extensions ?? []).map((entry) => resolve(path, entry));
      if (["extensions", "skills", "prompts", "themes"].some((name) => existsSync(join(path, name)))) {
        return [];
      }
      return [path];
    });
    return noExtensions
      ? loadFixtureExtensions(paths, eventBus)
      : discoverAndLoadExtensions(paths, cwd, agentDir, eventBus);
  }
}

export class ExtensionRunner {
  constructor(extensions, runtime, _cwd, sessionManager, modelRegistry) {
    this.extensions = extensions;
    this.runtime = runtime;
    this.sessionManager = sessionManager;
    this.modelRegistry = modelRegistry;
    this.ui = null;
    this.actions = null;
    this.contextActions = null;
    this.commandContext = null;
    this.errorHandler = null;
    this.providerBindings = null;
    this.providerActions = null;
    this.providerAfterStatus = null;
    this.fixtureMode = extensions[0]?.fixtureMode ?? (fixtureMode || "default");
    this.flagValues = new Map();
    this.localEvents = new Map();
    this.tools = [
      makeTool("fixture_echo", async (_id, input) => {
        console.log("fixture tool console output", input.value ?? "");
        if (fixtureMode === "validation") console.error(`fixture execution input type: ${typeof input.value}`);
        return {
          content: [
            { type: "text", text: input.value ?? "echo" },
            { type: "image", data: "aGVsbG8=", mimeType: "image/png", alt: "fixture" },
          ],
          details: { fixture: true },
        };
      }),
      makeTool("fixture_prompt", async (_id, _input, signal, _update, context) => {
        const value = await context.ui.input("fixture input");
        if (signal.aborted) throw new Error("aborted fixture prompt");
        return { content: [{ type: "text", text: value ?? "missing" }] };
      }),
      makeTool("fixture_progress", async (_id, _input, _signal, onUpdate) => {
        await onUpdate?.({ content: [{ type: "text", text: "halfway" }] });
        return { content: [{ type: "text", text: "complete" }] };
      }),
    ];
    if (fixtureMode === "prepared-input") {
      this.tools[0].definition.prepareArguments = (args) => args.raw === "invalid"
        ? { value: { invalid: true } } : { value: String(args.raw) };
    }
    if (fixtureApiVersion === "0.3") {
      this.tools.push(makeTool("fixture_hold", async (_id, _input, signal) => {
        await new Promise((resolve) => {
          if (signal.aborted) resolve();
          else signal.addEventListener("abort", resolve, { once: true });
        });
        if (signal.aborted) throw new Error("fixture hold cancelled");
        return { content: [{ type: "text", text: "fixture hold released" }] };
      }));
    }
    if (this.runtime?.aggregate?.loadOrder.length) {
      this.tools.push(
        makeTool("aggregate_state", async () => ({
          content: [{ type: "text", text: JSON.stringify(this.runtime.aggregate) }],
        })),
        makeTool("aggregate_wait", async (_id, _input, _signal, _update, context) => {
          const value = await context.ui.input("aggregate input");
          return { content: [{ type: "text", text: value ?? "missing" }] };
        }),
      );
    }
    if (this.fixtureMode === "result-contract") {
      this.tools.push(
        makeTool("fixture_usage", async () => ({
          content: [{ type: "text", text: "usage result" }],
          details: { fixture: "usage" },
          usage: FIXTURE_TOOL_USAGE,
        })),
        makeTool("fixture_terminate", async () => ({
          content: [{ type: "text", text: "terminate result" }],
          details: { fixture: "terminate" },
          terminate: true,
        })),
        makeTool("fixture_usage_terminate", async () => ({
          content: [{ type: "text", text: "usage terminate result" }],
          details: { fixture: "usage-terminate" },
          usage: FIXTURE_TOOL_USAGE,
          terminate: true,
        })),
        makeTool("fixture_usage_case", async (_id, input) => (input.value === "terminate-type"
          ? {
              content: [{ type: "text", text: "terminate type result" }],
              details: { fixture: "terminate-type" },
              terminate: "yes",
            }
          : input.value === "terminate-false"
          ? {
              content: [{ type: "text", text: "terminate false result" }],
              details: { fixture: "terminate-false" },
              terminate: false,
            }
          : {
              content: [{ type: "text", text: "usage case result" }],
              details: { fixture: "usage-case" },
              usage: fixtureUsageCase(String(input.value ?? "")),
            })),
        makeTool("fixture_hook_result", async () => ({
          content: [{ type: "text", text: "hook result" }],
          details: { fixture: "hook-result" },
        })),
      );
    }
    if (this.fixtureMode === "zero-effect") {
      const marker = (name) => async (_id, input) => {
        recordFixtureExecution(name, "execute");
        const target = fixtureMarkerPath();
        if (target) appendFileSync(target, `${name} ${String(input.value ?? "")}\n`);
        return { content: [{ type: "text", text: `${name} executed` }], details: { fixture: name } };
      };
      this.tools.push(
        makeTool("fixture_marker_a", marker("fixture_marker_a")),
        makeTool("fixture_marker_b", marker("fixture_marker_b")),
      );
      // Pi's declared preparation shim runs before validation: this fixture
      // produces an invalid prepared object instead of a valid schema value.
      const prepared = this.tools.find((tool) => tool.definition.name === "fixture_marker_a");
      prepared.definition.prepareArguments = (args) => {
        if (args?.raw === undefined) return args;
        return args.raw === "invalid" ? { value: { invalid: true } } : { value: String(args.raw) };
      };
    }
    if (this.fixtureMode === "projection") {
      this.tools.push(
        makeTool("fixture_snippet", async (_id, input) => ({
          content: [{ type: "text", text: `snippet ${input.value ?? ""}` }],
        })),
        makeTool("fixture_sequential", async () => {
          recordFixtureExecution("fixture_sequential", "start");
          await sleep(80);
          recordFixtureExecution("fixture_sequential", "end");
          return { content: [{ type: "text", text: "sequential result" }] };
        }),
        makeTool("fixture_parallel_probe", async () => {
          recordFixtureExecution("fixture_parallel_probe", "start");
          await sleep(80);
          recordFixtureExecution("fixture_parallel_probe", "end");
          return { content: [{ type: "text", text: "parallel result" }] };
        }),
        makeTool("fixture_execution_log", async () => ({
          content: [{ type: "text", text: JSON.stringify(fixtureExecutionLog) }],
        })),
      );
      const snippetTool = this.tools.find((tool) => tool.definition.name === "fixture_snippet");
      snippetTool.definition.description = "Snippet projection fixture";
      snippetTool.definition.promptSnippet = "  Snippet  with\n  whitespace\tcollapse ";
      snippetTool.definition.promptGuidelines = [
        "  First guideline  ",
        "",
        "First guideline",
        "Second guideline",
      ];
      const sequentialTool = this.tools.find((tool) => tool.definition.name === "fixture_sequential");
      sequentialTool.definition.executionMode = "sequential";
    }
    if (this.fixtureMode.startsWith("sampling-")) {
      const sampling = {
        "sampling-prefer": { type: "json_schema", strict: "prefer" },
        "sampling-require": { type: "json_schema", strict: "require" },
        "sampling-grammar": { type: "grammar", variants: { openai_lark: "start: /[a-z]+/" } },
        "sampling-unknown": { type: "json_schema_v2", strict: "prefer" },
      }[this.fixtureMode];
      const tool = makeTool("fixture_sampling", async () => ({
        content: [{ type: "text", text: "sampling result" }],
      }));
      tool.definition.constrainedSampling = sampling;
      this.tools.push(tool);
    }
    if (this.fixtureMode === "provider") {
      this.tools.push(
        makeTool("fixture_provider_update", async () => {
          this.providerActions.registerProvider("fixture-provider", fixtureProviderConfig(2));
          return { content: [{ type: "text", text: "provider updated" }] };
        }),
        makeTool("fixture_provider_unregister", async () => {
          this.providerActions.unregisterProvider("fixture-provider");
          return { content: [{ type: "text", text: "provider unregistered" }] };
        }),
        makeTool("fixture_provider_unsafe", async () => {
          this.providerActions.registerProvider("unsafe-provider", {
            ...fixtureProviderConfig(1),
            baseUrl: "https://must-not-cross-the-host-boundary.invalid",
          });
          return { content: [{ type: "text", text: "unexpected" }] };
        }),
        makeTool("fixture_provider_hook_status", async () => ({
          content: [{ type: "text", text: String(this.providerAfterStatus) }],
        })),
        makeTool("fixture_provider_cancel_status", async () => ({
          content: [{ type: "text", text: String(fixtureCancellationObserved) }],
        })),
        makeTool("fixture_provider_setup_status", async () => ({
          content: [{
            type: "text",
            text: JSON.stringify({
              stage: fixtureProviderSetupStage,
              abort_observed: fixtureProviderSetupAbortObserved,
              late_iterator_closed: fixtureProviderLateIteratorClosed,
            }),
          }],
        })),
        makeTool("fixture_provider_many_parts", async () => ({
          content: Array.from({ length: 257 }, (_unused, index) => ({
            type: "text",
            text: `part ${index}`,
          })),
        })),
      );
    }
  }

  bindCore(actions, contextActions, providerActions) {
    this.actions = actions;
    this.contextActions = contextActions;
    // Retain the historical name for API 0.2 public-surface probes.
    this.providerBindings = providerActions;
    this.providerActions = providerActions;
    if (this.fixtureMode === "provider") fixtureProviderActions = providerActions;
    const registerInitialProvider = () => {
      if (this.fixtureMode === "provider") {
        providerActions.registerProvider("fixture-provider", fixtureProviderConfig(1));
        if (initialProviderCount > 1) {
          providerActions.registerProvider("fixture-second-provider", fixtureProviderConfig(1, true));
        }
      } else if (this.fixtureMode === "unsafe-provider") {
        providerActions.registerProvider("unsafe-provider", {
          ...fixtureProviderConfig(1),
          apiKey: "must-not-cross-the-host-boundary",
        });
      }
    };
    if (providerRegistrationDelay > 0 && this.fixtureMode === "provider") {
      setTimeout(registerInitialProvider, providerRegistrationDelay);
    } else {
      registerInitialProvider();
    }
  }

  bindCommandContext(context) {
    this.commandContext = context;
  }

  setUIContext(ui, mode) {
    this.ui = ui;
    this.uiMode = mode;
  }

  onError(handler) {
    this.errorHandler = handler;
  }

  async emitBeforeProviderRequest(request) {
    if (this.fixtureMode !== "provider") return request;
    fixtureProviderSetupStage = "before_request";
    fixtureProviderSetupAbortObserved = false;
    fixtureProviderLateIteratorClosed = false;
    console.error("fixture provider setup before_request");
    mutateProviderDuringSetup("before_request");
    if (providerBeforeRequestDelay > 0) await sleep(providerBeforeRequestDelay);
    fixtureProviderSetupStage = "before_request_complete";
    if (request.fixture_unsafe_mutation === true) {
      request.authorizationHeader = "must-not-reach-pi-adapter";
      return undefined;
    }
    return { ...request, fixture_hook: true };
  }

  async emitAfterProviderResponse(status, headers) {
    if (this.fixtureMode !== "provider") return;
    if (headers === undefined || Object.keys(headers).length !== 0) {
      throw new Error("provider response headers crossed the fixture boundary");
    }
    this.providerAfterStatus = status;
  }

  getAllRegisteredTools() {
    return this.tools.slice();
  }

  getRegisteredCommands() {
    const commands = [
      {
        name: "add-tool",
        description: "Add a dynamic fixture tool",
        handler: async () => {
          this.tools.push(makeTool("fixture_dynamic", async () => ({
            content: [{ type: "text", text: "dynamic" }],
          })));
          this.actions.refreshTools();
        },
      },
      {
        name: "add-marker-tool",
        description: "Add a dynamic tool that observes its own execution",
        handler: async () => {
          this.tools.push(makeTool("fixture_marker_c", async (_id, input) => {
            recordFixtureExecution("fixture_marker_c", "execute");
            const target = fixtureMarkerPath();
            if (target) appendFileSync(target, `fixture_marker_c ${String(input.value ?? "")}\n`);
            return { content: [{ type: "text", text: "fixture_marker_c executed" }] };
          }));
          this.actions.refreshTools();
        },
      },
      {
        name: "ui-methods",
        description: "Validate current Pi UI method names",
        handler: async () => {
          for (const name of [
            "editor",
            "addAutocompleteProvider",
            "getEditorComponent",
            "getAllThemes",
            "getTheme",
            "setTheme",
            "getToolsExpanded",
            "setToolsExpanded",
          ]) {
            if (typeof this.ui[name] !== "function") throw new Error(`missing UI method ${name}`);
          }
          if (this.ui.theme.fg("accent", "plain") !== "plain") {
            throw new Error("compatibility theme did not preserve text");
          }
          try {
            this.ui.getEditorComponent();
            throw new Error("unsupported UI method unexpectedly succeeded");
          } catch (error) {
            if (!String(error).includes("Pi compatibility API is not supported by octet")) throw error;
          }
          this.ui.notify("ui-current-methods-explicit");
        },
      },
      {
        name: "host-state",
        description: "Validate host state bindings",
        handler: async () => {
          if (this.actions.getSessionName() !== "fixture session") {
            throw new Error(`unexpected session name ${this.actions.getSessionName()}`);
          }
          if (this.actions.getThinkingLevel() !== "high") {
            throw new Error(`unexpected thinking level ${this.actions.getThinkingLevel()}`);
          }
        },
      },
      {
        name: "unsupported",
        description: "Exercise an unsupported session action",
        handler: async (_arguments, context) => context.newSession(),
      },
    ];
    commands.push({
      name: "surface-probe",
      description: "Exercise one declared Pi public-surface fixture",
      handler: async (argumentsText, context) => this.probeSurface(String(argumentsText).trim(), context),
    });
    return [...commands, ...this.extensions.flatMap((extension) => [...(extension.commands?.values() ?? [])])];
  }

  setFlagValue(name, value) {
    this.flagValues.set(name, value);
  }

  getFlag(name) {
    if (this.flagValues.has(name)) return this.flagValues.get(name);
    return this.extensions[0]?.flags?.get(name)?.default;
  }

  async expectExplicit(target, action) {
    try {
      await action();
    } catch (error) {
      const message = String(error);
      if (message.includes("Pi compatibility API is not supported by octet")) {
        this.ui.notify(`surface:${target}:explicit`);
        return;
      }
      throw error;
    }
    throw new Error(`${target} unexpectedly succeeded`);
  }

  async probeSurface(target, context) {
    if (!target) throw new Error("surface-probe requires AREA.SURFACE");
    const explicit = (action) => this.expectExplicit(target, action);
    const bounded = (value) => this.ui.notify(
      `surface:${target}:bounded${value === undefined ? "" : `:${JSON.stringify(value)}`}`,
    );

    if (target.startsWith("extension_api.")) {
      const name = target.slice("extension_api.".length);
      if (["on", "registerTool", "registerCommand"].includes(name)) return bounded();
      if (["registerShortcut", "registerFlag", "registerMessageRenderer", "registerMarkdownTransformer", "registerEntryRenderer"].includes(name)) {
        return bounded();
      }
      if (name === "getFlag") return bounded(this.getFlag("fixture"));
      if (name === "sendMessage") return explicit(() => this.actions.sendMessage({ role: "assistant", content: "fixture" }));
      if (name === "sendUserMessage") return explicit(() => this.actions.sendUserMessage({ role: "user", content: "fixture" }));
      if (name === "appendEntry") return explicit(() => this.actions.appendEntry({ type: "custom", data: "fixture" }));
      if (name === "setSessionName") return explicit(() => this.actions.setSessionName("fixture"));
      if (name === "getSessionName") return bounded(this.actions.getSessionName());
      if (name === "setLabel") return explicit(() => this.actions.setLabel("fixture", "label"));
      if (name === "exec") return explicit(() => {
        if (typeof this.actions.exec !== "function") {
          throw new Error("Pi compatibility API is not supported by octet: pi.exec binding");
        }
        return this.actions.exec("true");
      });
      if (name === "getActiveTools") {
        // Declared reduction: bridge-local Pi tools, never the octet policy.
        const active = this.actions.getActiveTools();
        const expected = this.tools.map((tool) => tool.definition.name).sort();
        if (JSON.stringify([...active].sort()) !== JSON.stringify(expected)) {
          throw new Error("getActiveTools must report the bridge-local Pi tools");
        }
        return bounded();
      }
      if (name === "getAllTools") {
        const all = this.actions.getAllTools();
        const expected = this.tools.map((tool) => tool.definition.name).sort();
        if (JSON.stringify(all.map((tool) => tool.name).sort()) !== JSON.stringify(expected)) {
          throw new Error("getAllTools must report the bridge-local Pi tool information");
        }
        return bounded();
      }
      if (name === "getCommands") return bounded();
      if (name === "setActiveTools") return explicit(() => this.actions.setActiveTools(["fixture_echo"]));
      if (name === "setModel") return explicit(() => this.actions.setModel("fixture"));
      if (name === "getThinkingLevel") return bounded();
      if (name === "setThinkingLevel") return explicit(() => this.actions.setThinkingLevel("high"));
      if (name === "registerProvider") return explicit(() => this.providerBindings.registerProvider({ id: "fixture" }));
      if (name === "unregisterProvider") return explicit(() => this.providerBindings.unregisterProvider("fixture"));
      if (name === "events.emit") {
        this.localEvents.set("fixture", { value: true });
        return bounded();
      }
      if (name === "events.on") return bounded();
      throw new Error(`unknown extension API fixture ${name}`);
    }

    if (target.startsWith("ui_context.")) {
      const name = target.slice("ui_context.".length);
      if (name === "select") {
        await this.ui.select("Fixture choice", ["one", "two"]);
        return bounded();
      }
      if (name === "confirm") {
        await this.ui.confirm("Fixture confirm", "detail");
        return bounded();
      }
      if (name === "input") {
        await this.ui.input("Fixture input", "placeholder");
        return bounded();
      }
      if (name === "notify") return bounded();
      if (name === "setStatus") {
        this.ui.setStatus("fixture", "status");
        return bounded();
      }
      if (name === "theme") {
        if (this.ui.theme.bold("fixture") !== "fixture") throw new Error("theme text was not preserved");
        return bounded();
      }
      const calls = {
        editor: () => this.ui.editor("Fixture editor", "seed"),
        onTerminalInput: () => this.ui.onTerminalInput(() => {}),
        setWorkingMessage: () => this.ui.setWorkingMessage("fixture"),
        setWorkingVisible: () => this.ui.setWorkingVisible(true),
        setWorkingIndicator: () => this.ui.setWorkingIndicator("fixture"),
        setHiddenThinkingLabel: () => this.ui.setHiddenThinkingLabel("fixture"),
        setWidget: () => this.ui.setWidget("fixture", "widget"),
        setFooter: () => this.ui.setFooter(() => null),
        setHeader: () => this.ui.setHeader(() => null),
        setTitle: () => this.ui.setTitle("fixture"),
        custom: () => this.ui.custom(() => null),
        pasteToEditor: () => this.ui.pasteToEditor("fixture"),
        setEditorText: () => this.ui.setEditorText("fixture"),
        getEditorText: () => this.ui.getEditorText(),
        addAutocompleteProvider: () => this.ui.addAutocompleteProvider(() => []),
        setEditorComponent: () => this.ui.setEditorComponent(() => null),
        getEditorComponent: () => this.ui.getEditorComponent(),
        getAllThemes: () => this.ui.getAllThemes(),
        getTheme: () => this.ui.getTheme(),
        setTheme: () => this.ui.setTheme("fixture"),
        getToolsExpanded: () => this.ui.getToolsExpanded(),
        setToolsExpanded: () => this.ui.setToolsExpanded(true),
      };
      if (!calls[name]) throw new Error(`unknown UI fixture ${name}`);
      return explicit(calls[name]);
    }

    if (target.startsWith("context.")) {
      const name = target.slice("context.".length);
      // These rows declare a reduced read-only projection. Observe the reduced
      // value itself so a drifting bridge fails this fixture instead of merely
      // completing its probe.
      if (name === "mode") {
        if (context.mode !== "rpc") throw new Error(`ctx.mode must stay rpc, saw ${String(context.mode)}`);
        return bounded();
      }
      if (name === "model") {
        if (context.model !== undefined) throw new Error("ctx.model must not be reconstructed");
        return bounded();
      }
      if (name === "scopedModels") {
        if (!Array.isArray(context.scopedModels) || context.scopedModels.length !== 0) {
          throw new Error("ctx.scopedModels must stay an empty read-only snapshot");
        }
        return bounded();
      }
      if (name === "getContextUsage") {
        if (context.getContextUsage() !== undefined) throw new Error("ctx.getContextUsage must report unknown");
        return bounded();
      }
      if (name === "isProjectTrusted") {
        if (context.isProjectTrusted() !== false) throw new Error("ctx.isProjectTrusted must stay conservative");
        return bounded();
      }
      if (name === "getSystemPromptOptions") {
        const options = context.getSystemPromptOptions?.();
        if (!options || Object.keys(options).some((key) => key !== "cwd")) {
          throw new Error("ctx.getSystemPromptOptions must expose only the canonical cwd");
        }
        return bounded();
      }
      if (name === "isIdle") {
        // Declared reduction: the bridged turn lifecycle is the only idleness
        // signal the bridge observes, so the probe reports that value.
        const idle = context.isIdle();
        if (typeof idle !== "boolean") throw new Error("ctx.isIdle must report a boolean");
        return bounded(idle);
      }
      if (["ui", "hasUI", "cwd", "thinkingLevel", "signal", "waitForIdle"].includes(name)) {
        if (name === "waitForIdle") await context.waitForIdle();
        return bounded();
      }
      if (name === "sessionManager") return explicit(() => this.sessionManager.getEntries());
      if (name === "modelRegistry") return explicit(() => this.modelRegistry.getModel());
      // The cancellation behavior itself is covered by the bridge cancellation
      // fixture; invoking it here would intentionally cancel this probe request.
      if (name === "abort") return bounded();
      const actions = {
        abort: () => this.contextActions.abort(),
        hasPendingMessages: () => this.contextActions.hasPendingMessages(),
        shutdown: () => this.contextActions.shutdown(),
        compact: () => this.contextActions.compact(),
        getSystemPrompt: () => this.contextActions.getSystemPrompt(),
        newSession: () => context.newSession(),
        fork: () => context.fork(),
        navigateTree: () => context.navigateTree(),
        switchSession: () => context.switchSession(),
        reload: () => context.reload(),
        // A replacement-session context exists only after newSession/fork/
        // switchSession. Those operations are refused by the bridge, so these
        // two rows are observed through the refusal that makes a replacement
        // context unreachable rather than through a canned fixture error.
        "replacement.sendMessage": () => context.newSession(),
        "replacement.sendUserMessage": () => context.newSession(),
      };
      if (!actions[name]) throw new Error(`unknown context fixture ${name}`);
      return explicit(actions[name]);
    }
    throw new Error(`unknown surface fixture ${target}`);
  }

  createContext() {
    // Model the public runtime's context assembly: the read-only surfaces the
    // runtime exposes come from the bound bridge actions, so probing them
    // observes the bridge rather than a fixture-invented value.
    const actions = this.contextActions ?? {};
    return {
      ui: this.ui,
      mode: this.uiMode ?? "tui",
      model: actions.getModel?.(),
      scopedModels: actions.getScopedModels?.(),
      isProjectTrusted: () => actions.isProjectTrusted?.() === true,
      getContextUsage: () => actions.getContextUsage?.(),
      isIdle: () => actions.isIdle?.() === true,
      ...(actions.getSystemPromptOptions ? { getSystemPromptOptions: () => actions.getSystemPromptOptions() } : {}),
    };
  }

  createCommandContext() {
    return { ...this.createContext(), ...this.commandContext, ui: this.ui };
  }

  async emitBeforeAgentStart(prompt) {
    this.ui?.notify("event:before_agent_start:start");
    const result = {
      systemPrompt: `system context for ${prompt}`,
      messages: [{ role: "user", content: [{ type: "text", text: `message context for ${prompt}` }] }],
    };
    this.ui?.notify("event:before_agent_start:end");
    return result;
  }

  async emitContext(messages) {
    this.ui?.notify("event:context:start");
    const result = [...messages, { role: "user", content: "context event contribution" }];
    this.ui?.notify("event:context:end");
    return result;
  }

  async emitToolCall(event) {
    if (fixtureApiVersion === "0.2") this.ui?.notify("event:tool_call:start");
    if (fixtureMode === "invalid-hook-input" || event.input?.value === "hook-invalid") {
      event.input.value = { invalid: true };
    }
    if (event.input?.mutateNative) event.input.value = "mutated";
    const result = {
      block: false,
      ...(event.input?.terminate ? { terminate: true } : {}),
    };
    if (fixtureApiVersion === "0.2") this.ui?.notify("event:tool_call:end");
    return result;
  }

  async emitToolResult(event) {
    // API 0.3 deliberately does not select notification surfaces. Preserve
    // API 0.2 lifecycle assertions without turning provider tool calls into
    // unsupported UI calls.
    if (this.fixtureMode === "default") {
      this.ui.notify("event:tool_result:start");
      this.ui.notify(`terminal:tool_result:${event.toolCallId}`);
    }
    const selector = event.input?.value;
    if (this.fixtureMode === "result-contract") {
      if (selector === "hook-usage") return { ...event, usage: FIXTURE_HOOK_USAGE };
      if (selector === "hook-usage-passthrough") return { ...event };
      if (selector === "hook-terminate") return { ...event, terminate: true };
      if (selector === "hook-terminate-matching") return { ...event, terminate: false };
      if (selector === "hook-details") return { ...event, details: { transformed: true } };
      if (selector !== "transform") return undefined;
    }
    const result = selector === "transform"
      ? {
          ...event,
          content: [{ type: "text", text: "transformed" }],
          details: { transformed: true },
          isError: true,
          usage: FIXTURE_HOOK_USAGE,
        }
      : undefined;
    if (this.fixtureMode === "default") this.ui.notify("event:tool_result:end");
    return result;
  }

  async emit(event) {
    const type = event.type;
    this.ui?.notify(`event:${type}:start`);
    if (type === "turn_start") await sleep(80);
    if (type === "session_start") {
      this.errorHandler?.({
        extensionPath: this.extensions[0].path,
        event: "session_start",
        error: new Error("fixture lifecycle failure"),
      });
    }
    this.ui?.notify(`event:${type}:end`);
  }
}
