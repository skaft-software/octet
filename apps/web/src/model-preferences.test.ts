import { describe, expect, it } from "vitest";
import {
  AUXILIARY_REASONING_OFF,
  isReasoningEffortSupported,
  modelReasoningCapabilities,
  normalizeModelPreferences,
  preferredReasoningForModel,
  resolveReasoningForModel,
  serializeModelPreferences,
  supportedReasoningEfforts,
  validateReasoningPreference,
  type ModelPreferenceModel,
} from "./model-preferences";

const model = (
  overrides: Partial<ModelPreferenceModel> = {},
): ModelPreferenceModel => ({
  id: "provider/model-a",
  reasoning: ["low", "high"],
  defaultReasoning: "low",
  ...overrides,
});

describe("model reasoning preferences", () => {
  it("normalizes bounded mappings and preserves provider-defined effort names", () => {
    const preferences = normalizeModelPreferences({
      "provider/model-a": "vendor-custom",
      "provider/model-b": "",
      "provider/model-c": 4,
      "   ": "low",
    });

    expect(preferences).toEqual({ "provider/model-a": "vendor-custom" });
    expect(
      normalizeModelPreferences(
        JSON.stringify({ version: 1, preferences: { "provider/model-a": "high" } }),
      ),
    ).toEqual({ "provider/model-a": "high" });
    expect(serializeModelPreferences(preferences)).toEqual({
      "provider/model-a": "vendor-custom",
    });
  });

  it("distinguishes unknown capabilities from an empty endpoint catalog", () => {
    expect(modelReasoningCapabilities(undefined)).toEqual({
      state: "unknown",
      efforts: [],
    });
    expect(modelReasoningCapabilities({ reasoning: [] })).toEqual({
      state: "known",
      efforts: [],
      defaultReasoning: undefined,
    });
    expect(supportedReasoningEfforts(undefined, true)).toEqual([]);
    expect(supportedReasoningEfforts({ reasoning: [] }, true)).toEqual([
      AUXILIARY_REASONING_OFF,
    ]);
  });

  it("validates saved values against the current model catalog", () => {
    const current = model();
    const saved = normalizeModelPreferences({
      "provider/model-a": "high",
      "provider/model-b": "removed-by-endpoint",
    });

    expect(preferredReasoningForModel(current, saved)).toBe("high");
    expect(
      preferredReasoningForModel(
        model({ reasoning: ["low"], defaultReasoning: "low" }),
        saved,
      ),
    ).toBeUndefined();
    expect(
      validateReasoningPreference("vendor-custom", current),
    ).toMatchObject({ valid: false, reason: "unsupported" });
    expect(validateReasoningPreference("vendor-custom", undefined)).toEqual({
      valid: false,
      reason: "unknown-capabilities",
    });
    expect(isReasoningEffortSupported(AUXILIARY_REASONING_OFF, current)).toBe(
      true,
    );
    expect(
      isReasoningEffortSupported(AUXILIARY_REASONING_OFF, current, {
        allowAuxiliaryOff: false,
      }),
    ).toBe(false);
  });

  it("uses endpoint defaults and minimums without inferring from model names", () => {
    expect(
      resolveReasoningForModel(
        model({ id: "a-model", defaultReasoning: "high" }),
        undefined,
      ),
    ).toMatchObject({ reasoning: "high", source: "default" });
    expect(
      resolveReasoningForModel(
        model({ id: "luna-ish", reasoning: ["vendor-minimum"] }),
        undefined,
      ),
    ).toMatchObject({ reasoning: "vendor-minimum", source: "minimum" });
    expect(
      resolveReasoningForModel(
        model({ id: "empty-model", reasoning: [] }),
        undefined,
      ),
    ).toMatchObject({ reasoning: "off", source: "off" });

    const exactIdentity = normalizeModelPreferences({
      "provider/model-a": "high",
    });
    expect(
      preferredReasoningForModel(model({ id: "other/model-a" }), exactIdentity),
    ).toBeUndefined();
  });
});
