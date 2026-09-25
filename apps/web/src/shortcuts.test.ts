import { describe, expect, it, vi } from "vitest";
import {
  GLOBAL_SHORTCUTS,
  isTextInputTarget,
  matchesShortcut,
  registerGlobalShortcuts,
  shortcutForEvent,
  shortcutLabel,
} from "./shortcuts";

describe("global shortcuts", () => {
  it("documents the command-center bindings", () => {
    expect(GLOBAL_SHORTCUTS.map((shortcut) => shortcut.label)).toEqual([
      "Ctrl+N",
      "Ctrl+B",
      "Ctrl+,",
      "Ctrl+Shift+F",
      "Ctrl+Shift+P",
      "M",
      "Escape",
    ]);
    expect(shortcutLabel("open-projects")).toBe("Ctrl+Shift+P");
  });

  it("matches Ctrl and the platform primary modifier without accepting extra modifiers", () => {
    const shortcut = GLOBAL_SHORTCUTS[0]!;
    expect(
      matchesShortcut(
        { key: "n", ctrlKey: true, metaKey: false, shiftKey: false, altKey: false },
        shortcut,
      ),
    ).toBe(true);
    expect(
      matchesShortcut(
        { key: "N", ctrlKey: false, metaKey: true, shiftKey: false, altKey: false },
        shortcut,
      ),
    ).toBe(true);
    expect(
      matchesShortcut(
        { key: "n", ctrlKey: true, metaKey: true, shiftKey: false, altKey: false },
        shortcut,
      ),
    ).toBe(false);
    expect(
      matchesShortcut(
        { key: "n", ctrlKey: true, metaKey: false, shiftKey: true, altKey: false },
        shortcut,
      ),
    ).toBe(false);
    expect(
      matchesShortcut(
        { key: "n", ctrlKey: true, metaKey: false, shiftKey: false, altKey: true },
        shortcut,
      ),
    ).toBe(false);
  });

  it("identifies text entry targets", () => {
    const input = document.createElement("input");
    const textarea = document.createElement("textarea");
    const textbox = document.createElement("div");
    textbox.setAttribute("role", "textbox");
    expect(isTextInputTarget(input)).toBe(true);
    expect(isTextInputTarget(textarea)).toBe(true);
    expect(isTextInputTarget(textbox)).toBe(true);
    expect(isTextInputTarget(document.createElement("button"))).toBe(false);
  });

  it("dispatches shortcuts, prevents browser defaults, and keeps text input safe", () => {
    const onAction = vi.fn();
    const cleanup = registerGlobalShortcuts({ onAction });

    const newSession = new KeyboardEvent("keydown", {
      key: "n",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    document.dispatchEvent(newSession);
    expect(newSession.defaultPrevented).toBe(true);
    expect(onAction).toHaveBeenLastCalledWith("new-session", newSession);

    const input = document.createElement("input");
    document.body.append(input);
    input.focus();
    const inInput = new KeyboardEvent("keydown", {
      key: "b",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    input.dispatchEvent(inInput);
    expect(inInput.defaultPrevented).toBe(false);
    expect(onAction).toHaveBeenCalledTimes(1);

    const escape = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });
    input.dispatchEvent(escape);
    expect(escape.defaultPrevented).toBe(true);
    expect(onAction).toHaveBeenLastCalledWith("close-overlay", escape);

    cleanup();
    const afterCleanup = new KeyboardEvent("keydown", {
      key: "n",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    document.dispatchEvent(afterCleanup);
    expect(onAction).toHaveBeenCalledTimes(2);
  });

  it("returns the matching action for discovery and lets callers disable it", () => {
    expect(
      shortcutForEvent({
        key: "f",
        ctrlKey: true,
        metaKey: false,
        shiftKey: true,
        altKey: false,
      })?.action,
    ).toBe("open-transcript-search");

    const onAction = vi.fn();
    const cleanup = registerGlobalShortcuts({
      onAction,
      isEnabled: (action) => action !== "open-settings",
    });
    const event = new KeyboardEvent("keydown", {
      key: ",",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    document.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    expect(onAction).not.toHaveBeenCalled();
    cleanup();
  });
});
