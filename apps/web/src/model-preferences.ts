import type { ModelSummary, ReasoningEffort } from "./protocol";

/** The UI's auxiliary value for disabling provider reasoning. */
export const AUXILIARY_REASONING_OFF: ReasoningEffort = "off";

/** Keep persisted data bounded even when it comes from a remote endpoint. */
export const MAX_MODEL_PREFERENCES = 256;
export const MAX_MODEL_ID_LENGTH = 256;
export const MAX_REASONING_EFFORT_LENGTH = 128;

export type ModelPreferences = Readonly<Record<string, ReasoningEffort>>;

/**
 * Persistence is deliberately injected. The web store does not choose a
 * browser-global storage area, and this boundary can be backed by the serve
 * state directory once the host transport exposes it.
 */
export interface ModelPreferenceStorage {
  load(): Promise<unknown> | unknown;
  save(preferences: ModelPreferences): Promise<void> | void;
}

export const MODEL_PREFERENCES_STORAGE_KEY =
  "octet.web.model-reasoning-preferences";

export type StringValueStorage = Pick<Storage, "getItem" | "setItem">;

/** Best-effort browser-local persistence; host/session state remains separate. */
export function createBrowserModelPreferenceStorage(
  storage?: StringValueStorage | null,
): ModelPreferenceStorage | null {
  let resolved = storage;
  if (resolved === undefined && typeof window !== "undefined") {
    try {
      resolved = window.localStorage;
    } catch {
      resolved = null;
    }
  }
  if (!resolved) return null;

  return {
    load: () => {
      try {
        return resolved!.getItem(MODEL_PREFERENCES_STORAGE_KEY) ?? {};
      } catch {
        return {};
      }
    },
    save: (preferences) => {
      resolved!.setItem(
        MODEL_PREFERENCES_STORAGE_KEY,
        JSON.stringify(serializeModelPreferences(preferences)),
      );
    },
  };
}

export const createLocalModelPreferenceStorage =
  createBrowserModelPreferenceStorage;

export interface ModelReasoningSource {
  reasoning?: readonly ReasoningEffort[];
  defaultReasoning?: ReasoningEffort;
}

export type ModelPreferenceModel = Pick<
  ModelSummary,
  "id" | "reasoning" | "defaultReasoning"
>;

export type ReasoningCapabilityState = "known" | "unknown";

export interface ModelReasoningCapabilities {
  state: ReasoningCapabilityState;
  efforts: readonly ReasoningEffort[];
  defaultReasoning?: ReasoningEffort;
}

export type ReasoningPreferenceValidation =
  | { valid: true; value: ReasoningEffort }
  | {
      valid: false;
      reason: "invalid-effort" | "unknown-capabilities" | "unsupported";
    };

export type ModelReasoningResolution =
  | {
      state: "known";
      effort: ReasoningEffort;
      reasoning: ReasoningEffort;
      source: "preference" | "default" | "minimum" | "off";
    }
  | {
      state: "unknown";
      effort: undefined;
      reasoning: undefined;
      source: "unknown";
    };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isModelId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= MAX_MODEL_ID_LENGTH &&
    value.trim().length > 0
  );
}

export function isWellFormedReasoningEffort(
  value: unknown,
): value is ReasoningEffort {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= MAX_REASONING_EFFORT_LENGTH &&
    value.trim().length > 0
  );
}

function emptyPreferences(): ModelPreferences {
  return Object.freeze(Object.create(null) as Record<string, ReasoningEffort>);
}

/**
 * Parse either the direct JSON mapping used by the config file or a versioned
 * envelope accepted by newer hosts. Invalid entries are ignored rather than
 * becoming active configuration.
 */
export function normalizeModelPreferences(value: unknown): ModelPreferences {
  let parsed = value;
  if (typeof parsed === "string") {
    try {
      parsed = JSON.parse(parsed) as unknown;
    } catch {
      return emptyPreferences();
    }
  }
  if (!isRecord(parsed)) return emptyPreferences();

  const versioned =
    parsed.version === 1 && isRecord(parsed.preferences)
      ? parsed.preferences
      : parsed;
  const normalized: Record<string, ReasoningEffort> = Object.create(null);
  let count = 0;
  for (const [modelId, effort] of Object.entries(versioned)) {
    if (count >= MAX_MODEL_PREFERENCES) break;
    if (!isModelId(modelId) || !isWellFormedReasoningEffort(effort)) continue;
    normalized[modelId] = effort;
    count += 1;
  }
  return Object.freeze(normalized);
}

export const parseModelPreferences = normalizeModelPreferences;

/** Return a safe direct mapping suitable for JSON persistence. */
export function serializeModelPreferences(
  preferences: ModelPreferences | unknown,
): Record<string, ReasoningEffort> {
  const normalized = normalizeModelPreferences(preferences);
  const serialized: Record<string, ReasoningEffort> = Object.create(null);
  for (const [modelId, effort] of Object.entries(normalized)) {
    serialized[modelId] = effort;
  }
  return serialized;
}

/**
 * Derive capabilities only from the endpoint's reasoning catalog. A missing
 * catalog is different from an empty catalog: the former must not be guessed.
 */
export function modelReasoningCapabilities(
  model: ModelReasoningSource | null | undefined,
): ModelReasoningCapabilities {
  if (!model || !Array.isArray(model.reasoning)) {
    return { state: "unknown", efforts: [] };
  }

  const efforts: ReasoningEffort[] = [];
  for (const effort of model.reasoning) {
    if (isWellFormedReasoningEffort(effort) && !efforts.includes(effort)) {
      efforts.push(effort);
    }
  }

  const defaultReasoning =
    isWellFormedReasoningEffort(model.defaultReasoning) &&
    (model.defaultReasoning === AUXILIARY_REASONING_OFF ||
      efforts.includes(model.defaultReasoning))
      ? model.defaultReasoning
      : undefined;

  return {
    state: "known",
    efforts: Object.freeze(efforts),
    defaultReasoning,
  };
}

function capabilitiesFor(
  model: ModelReasoningSource | ModelReasoningCapabilities | null | undefined,
): ModelReasoningCapabilities {
  if (!model) return { state: "unknown", efforts: [] };
  if ("state" in model && "efforts" in model) return model;
  return modelReasoningCapabilities(model);
}

/** Endpoint-advertised values, with optional explicit auxiliary Off. */
export function supportedReasoningEfforts(
  model: ModelReasoningSource | ModelReasoningCapabilities | null | undefined,
  includeAuxiliaryOff = false,
): ReasoningEffort[] {
  const capabilities = capabilitiesFor(model);
  if (capabilities.state === "unknown") return [];
  const efforts = [...capabilities.efforts];
  if (includeAuxiliaryOff && !efforts.includes(AUXILIARY_REASONING_OFF)) {
    efforts.push(AUXILIARY_REASONING_OFF);
  }
  return efforts;
}

export function isReasoningEffortSupported(
  effort: unknown,
  model: ModelReasoningSource | ModelReasoningCapabilities | null | undefined,
  options: { allowAuxiliaryOff?: boolean } = {},
): effort is ReasoningEffort {
  const capabilities = capabilitiesFor(model);
  if (capabilities.state === "unknown" || !isWellFormedReasoningEffort(effort)) {
    return false;
  }
  if (
    options.allowAuxiliaryOff !== false &&
    effort === AUXILIARY_REASONING_OFF
  ) {
    return true;
  }
  return capabilities.efforts.includes(effort);
}

export function validateReasoningPreference(
  effort: unknown,
  model: ModelReasoningSource | ModelReasoningCapabilities | null | undefined,
  options: { allowAuxiliaryOff?: boolean } = {},
): ReasoningPreferenceValidation {
  if (!isWellFormedReasoningEffort(effort)) {
    return { valid: false, reason: "invalid-effort" };
  }
  const capabilities = capabilitiesFor(model);
  if (capabilities.state === "unknown") {
    return { valid: false, reason: "unknown-capabilities" };
  }
  if (isReasoningEffortSupported(effort, capabilities, options)) {
    return { valid: true, value: effort };
  }
  return { valid: false, reason: "unsupported" };
}

function hasPreference(
  preferences: ModelPreferences | undefined,
  modelId: string,
): boolean {
  return (
    preferences !== undefined &&
    Object.prototype.hasOwnProperty.call(preferences, modelId)
  );
}

/** Invalid or stale saved values are retained on disk but never applied. */
export function preferredReasoningForModel(
  model: ModelPreferenceModel | null | undefined,
  preferences: ModelPreferences | undefined,
): ReasoningEffort | undefined {
  if (!model || !hasPreference(preferences, model.id)) return undefined;
  const effort = preferences![model.id];
  return validateReasoningPreference(effort, model).valid ? effort : undefined;
}

/**
 * Select a model's saved value, endpoint default, advertised minimum (the
 * endpoint's first value), or auxiliary Off in that order. No provider/model
 * name is inspected here.
 */
export function resolveReasoningForModel(
  model: ModelPreferenceModel | null | undefined,
  preferences: ModelPreferences | undefined,
): ModelReasoningResolution {
  if (!model) {
    return {
      state: "unknown",
      effort: undefined,
      reasoning: undefined,
      source: "unknown",
    };
  }

  const capabilities = modelReasoningCapabilities(model);
  if (capabilities.state === "unknown") {
    return {
      state: "unknown",
      effort: undefined,
      reasoning: undefined,
      source: "unknown",
    };
  }

  const preferred = preferredReasoningForModel(model, preferences);
  const effort =
    preferred ??
    capabilities.defaultReasoning ??
    capabilities.efforts[0] ??
    AUXILIARY_REASONING_OFF;
  const source =
    preferred !== undefined
      ? "preference"
      : capabilities.defaultReasoning !== undefined
        ? "default"
        : capabilities.efforts.length > 0
          ? "minimum"
          : "off";
  return { state: "known", effort, reasoning: effort, source };
}

export const resolveEffectiveReasoning = resolveReasoningForModel;

// Keep this relationship visible to consumers that use the endpoint model
// type directly, while all capability decisions remain catalog-based.
export type EndpointModelReasoning = ModelSummary;
