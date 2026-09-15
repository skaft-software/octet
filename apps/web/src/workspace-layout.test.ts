import { describe, expect, it } from "vitest";
import {
  DEFAULT_DOCK_LAYOUT,
  MAX_DOCK_LAYOUT_BYTES,
  dockSlotFor,
  dockSplitVisible,
  moveDockPane,
  parseDockLayout,
  serializeDockLayout,
  setDockSplit,
} from "./workspace-layout";

describe("workspace dock layout", () => {
  it("defaults to the historical single-column dock", () => {
    expect(DEFAULT_DOCK_LAYOUT.order).toEqual(["activity", "inspector"]);
    expect(DEFAULT_DOCK_LAYOUT.split).toBe(false);
    expect(dockSlotFor(DEFAULT_DOCK_LAYOUT, "activity")).toBe("a");
    expect(dockSlotFor(DEFAULT_DOCK_LAYOUT, "inspector")).toBeNull();
    expect(dockSplitVisible(DEFAULT_DOCK_LAYOUT, 2)).toBe(false);
  });

  it("round-trips a user-created split in the persisted order", () => {
    const layout = setDockSplit(
      moveDockPane(DEFAULT_DOCK_LAYOUT, "activity", 1),
      true,
    );
    expect(layout.order).toEqual(["inspector", "activity"]);
    expect(parseDockLayout(serializeDockLayout(layout))).toEqual(layout);
    expect(dockSlotFor(layout, "inspector")).toBe("a");
    expect(dockSlotFor(layout, "activity")).toBe("b");
    expect(dockSplitVisible(layout, 1)).toBe(false);
    expect(dockSplitVisible(layout, 2)).toBe(true);
  });

  it("keeps the existing order when a pane cannot move further", () => {
    const layout = setDockSplit(DEFAULT_DOCK_LAYOUT, true);
    expect(moveDockPane(layout, "activity", -1)).toEqual(layout);
    expect(moveDockPane(layout, "inspector", 1)).toEqual(layout);
    expect(moveDockPane(layout, "activity", 1)).toEqual({
      order: ["inspector", "activity"],
      split: true,
    });
    expect(setDockSplit(layout, true)).toEqual(layout);
  });

  it("fails closed for every unknown, hostile, or oversized stored value", () => {
    const invalidInputs: unknown[] = [
      null,
      undefined,
      42,
      [],
      "",
      "{",
      "null",
      '"activity"',
      JSON.stringify({ version: 2, order: ["activity", "inspector"], split: true }),
      JSON.stringify({ order: ["activity", "inspector"], split: true }),
      JSON.stringify({ version: 1, order: ["activity", "inspector"], split: "yes" }),
      JSON.stringify({ version: 1, order: ["activity"], split: true }),
      JSON.stringify({ version: 1, order: ["activity", "activity"], split: true }),
      JSON.stringify({ version: 1, order: ["activity", "terminal"], split: true }),
      JSON.stringify({
        version: 1,
        order: ["activity", "inspector", "terminal"],
        split: true,
      }),
      "x".repeat(MAX_DOCK_LAYOUT_BYTES + 1),
    ];
    for (const input of invalidInputs) {
      expect(parseDockLayout(input)).toEqual(DEFAULT_DOCK_LAYOUT);
    }
  });

  it("keeps the persisted record bounded", () => {
    const encoded = serializeDockLayout({
      order: ["inspector", "activity"],
      split: true,
    });
    expect(encoded.length).toBeLessThanOrEqual(MAX_DOCK_LAYOUT_BYTES);
    expect(parseDockLayout(encoded)).toEqual({
      order: ["inspector", "activity"],
      split: true,
    });
  });
});
