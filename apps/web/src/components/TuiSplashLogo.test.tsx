/// <reference types="vite/client" />
/// <reference types="node" />

import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import bundledMark from "../assets/octet-glyph.svg?raw";
import { TuiSplashLogo } from "./TuiSplashLogo";
import {
  renderTuiSplashFrame,
  TUI_SPLASH_DURATION_SECONDS,
} from "./tuiSplash";

afterEach(() => {
  cleanup();
  delete document.documentElement.dataset.motion;
});

describe("TUI startup splash", () => {
  it("pins the bundled symbol to the approved native master", () => {
    const canonicalMark = readFileSync(resolve("../../docs/assets/octet/marks/mark-gradient.svg"), "utf8");
    expect(bundledMark).toBe(canonicalMark);
    const page = new DOMParser().parseFromString(readFileSync(resolve("index.html"), "utf8"), "text/html");
    const favicon = page.querySelector('link[rel="icon"]')!.getAttribute("href")!;
    expect(decodeURIComponent(favicon.replace("data:image/svg+xml,", ""))).toBe(canonicalMark);
    expect(page.title).toBe("octet");
    const svg = new DOMParser().parseFromString(bundledMark, "image/svg+xml");
    const columns = [...svg.querySelectorAll("[data-bit]")];
    expect(columns).toHaveLength(8);
    expect(columns.map((column) => column.getAttribute("data-value")).join("")).toBe("01101111");
  });

  it("cancels its finite animation on unmount", () => {
    vi.spyOn(window, "requestAnimationFrame").mockReturnValue(17);
    const cancel = vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
    const { unmount } = render(<TuiSplashLogo modelAccent="#cc785c" />);
    unmount();
    expect(cancel).toHaveBeenCalledWith(17);
  });

  it("renders the complete 01101111 byte immediately and only shimmers color", () => {
    const frames = [0, 0.4, 1.45, TUI_SPLASH_DURATION_SECONDS].map((time) =>
      renderTuiSplashFrame(time, "#cc785c"),
    );
    for (const frame of frames) {
      expect(frame.light).toHaveLength(16);
      expect(frame.dark).toHaveLength(16);
      expect(frame.dark.slice(0, 8).map((cell) => cell.glyph).join("")).toBe(" ██ ████");
      expect(frame.dark.slice(8).map((cell) => cell.glyph).join("")).toBe("████████");
      expect(frame.dark.filter((cell) => cell.color !== null)).toHaveLength(14);
      expect(frame.light.map((cell) => cell.glyph)).toEqual(frame.dark.map((cell) => cell.glyph));
    }
    expect(frames[2]!.dark.map((cell) => cell.color)).not.toEqual(
      frames[3]!.dark.map((cell) => cell.color),
    );
  });

  it("keeps the byte geometry stable while adapting its colors by model", () => {
    const openAi = renderTuiSplashFrame(
      TUI_SPLASH_DURATION_SECONDS,
      "#1f1f1f",
    );
    const anthropic = renderTuiSplashFrame(
      TUI_SPLASH_DURATION_SECONDS,
      "#cc785c",
    );

    expect(openAi.dark.map((cell) => cell.glyph)).toEqual(
      anthropic.dark.map((cell) => cell.glyph),
    );
    expect(openAi.dark.map((cell) => cell.color)).not.toEqual(
      anthropic.dark.map((cell) => cell.color),
    );
  });

  it("shows the final frame without animation when motion is reduced", () => {
    document.documentElement.dataset.motion = "reduced";
    const { container } = render(<TuiSplashLogo modelAccent="#cc785c" />);

    expect(container.querySelector(".tui-splash-logo")).toHaveAttribute(
      "data-animation",
      "settled",
    );
    expect(
      container.querySelectorAll(
        '.tui-splash-cell:not([style*="transparent"])',
      ).length,
    ).toBeGreaterThan(0);
  });

  it("starts the sequence again when a new session key remounts it", () => {
    let nextFrame: FrameRequestCallback | undefined;
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      nextFrame = callback;
      return 1;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
    const { container, rerender } = render(
      <TuiSplashLogo key="session-one" modelAccent="#cc785c" />,
    );

    expect(container.querySelector(".tui-splash-logo")).toHaveAttribute(
      "data-animation",
      "animating",
    );
    act(() => nextFrame?.(performance.now() + 2_300));
    expect(container.querySelector(".tui-splash-logo")).toHaveAttribute(
      "data-animation",
      "settled",
    );

    rerender(<TuiSplashLogo key="session-two" modelAccent="#34a853" />);
    expect(container.querySelector(".tui-splash-logo")).toHaveAttribute(
      "data-animation",
      "animating",
    );
  });
});
