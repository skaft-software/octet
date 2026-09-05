/// <reference types="vite/client" />

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyStoredTypePreferences } from "./theme";

describe("stored type preferences", () => {
  const values = new Map<string, string>();

  beforeEach(() => {
    values.clear();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
      clear: () => values.clear(),
    });
  });

  afterEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("style");
    vi.unstubAllGlobals();
  });

  it("uses Local as the default interface and technical type pairing", () => {
    applyStoredTypePreferences();

    const style = document.documentElement.style;
    expect(style.getPropertyValue("--ui-family")).toContain('"Local Grotesk"');
    expect(style.getPropertyValue("--mono-family")).toContain('"Local Mono"');
    expect(values.get("octet.ui.font")).toBe("local");
  });

  it("does not read or rewrite earlier first-party storage", () => {
    localStorage.setItem("ygg.ui.font", "geist");
    localStorage.setItem("ygg.ui.size", "12");
    applyStoredTypePreferences();
    expect(document.documentElement.style.getPropertyValue("--ui-family")).toContain('"Local Grotesk"');
    expect(document.documentElement.style.getPropertyValue("--font-body")).toBe("14px");
    expect(values.get("octet.ui.font")).toBe("local");
    expect(values.get("ygg.ui.font")).toBe("geist");
    expect(values.get("ygg.ui.size")).toBe("12");
  });

  it("uses one interface size and one compact metadata size", () => {
    localStorage.setItem("octet.ui.size", "13");

    applyStoredTypePreferences();

    const style = document.documentElement.style;
    expect(style.getPropertyValue("--font-body")).toBe("13px");
    expect(style.getPropertyValue("--font-meta")).toBe("11px");
    expect(style.getPropertyValue("--font-chat")).toBe("");
    expect(style.getPropertyValue("--font-prompt")).toBe("");
    expect(style.getPropertyValue("--font-display")).toBe("");
  });
});
