/**
 * Bounded, fail-closed model for the user-created Serve workspace dock layout.
 *
 * The dock hosts at most two panes side by side. A user may create the split and
 * rearrange the panes; the arrangement is persisted per browser. Every input —
 * the stored string included — is validated here so an unknown, oversized,
 * truncated, or hostile value can only ever fall back to the default layout.
 */

export const DOCK_PANES = ["activity", "inspector"] as const;

export type DockPaneId = (typeof DOCK_PANES)[number];

/** The dock is bounded to exactly these two panes; a third cannot be created. */
export const MAX_DOCK_PANES = DOCK_PANES.length;

/** A persisted layout is a short, fixed-shape record; anything longer is refused. */
export const MAX_DOCK_LAYOUT_BYTES = 256;

export const DOCK_LAYOUT_VERSION = 1;

export interface DockLayout {
  /** Left-to-right pane order; always a permutation of {@link DOCK_PANES}. */
  readonly order: readonly DockPaneId[];
  /** Whether the user created the second dock column. */
  readonly split: boolean;
}

export type DockSlot = "a" | "b";

export const DEFAULT_DOCK_LAYOUT: DockLayout = Object.freeze({
  order: Object.freeze([...DOCK_PANES]),
  split: false,
});

function isDockPaneId(value: unknown): value is DockPaneId {
  return (
    typeof value === "string" &&
    (DOCK_PANES as readonly string[]).includes(value)
  );
}

export function parseDockLayout(raw: unknown): DockLayout {
  if (typeof raw !== "string" || raw.length === 0) return DEFAULT_DOCK_LAYOUT;
  if (new TextEncoder().encode(raw).length > MAX_DOCK_LAYOUT_BYTES) {
    return DEFAULT_DOCK_LAYOUT;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return DEFAULT_DOCK_LAYOUT;
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return DEFAULT_DOCK_LAYOUT;
  }
  const record = parsed as Record<string, unknown>;
  if (record.version !== DOCK_LAYOUT_VERSION) return DEFAULT_DOCK_LAYOUT;
  if (typeof record.split !== "boolean") return DEFAULT_DOCK_LAYOUT;
  if (!Array.isArray(record.order) || record.order.length !== MAX_DOCK_PANES) {
    return DEFAULT_DOCK_LAYOUT;
  }
  const order: DockPaneId[] = [];
  for (const entry of record.order) {
    if (!isDockPaneId(entry) || order.includes(entry)) return DEFAULT_DOCK_LAYOUT;
    order.push(entry);
  }
  if (order.length !== MAX_DOCK_PANES) return DEFAULT_DOCK_LAYOUT;
  return { order, split: record.split };
}

export function serializeDockLayout(layout: DockLayout): string {
  return JSON.stringify({
    version: DOCK_LAYOUT_VERSION,
    order: [...layout.order],
    split: layout.split,
  });
}

/** Reorder one dock pane by -1 (left) or 1 (right); never unbounded. */
export function moveDockPane(
  layout: DockLayout,
  pane: DockPaneId,
  offset: -1 | 1,
): DockLayout {
  const index = layout.order.indexOf(pane);
  if (index < 0) return layout;
  const target = index + offset;
  if (target < 0 || target >= layout.order.length) return layout;
  const order = [...layout.order];
  order[index] = order[target];
  order[target] = pane;
  return { order, split: layout.split };
}

export function setDockSplit(layout: DockLayout, split: boolean): DockLayout {
  if (layout.split === split) return layout;
  return { order: [...layout.order], split };
}

/** The grid column a pane occupies, or null when it is not part of the dock. */
export function dockSlotFor(
  layout: DockLayout,
  pane: DockPaneId,
): DockSlot | null {
  const index = layout.order.indexOf(pane);
  if (index === 0) return "a";
  if (index === 1 && layout.split) return "b";
  return null;
}

/** Whether the user-created second column is in effect for two open panes. */
export function dockSplitVisible(layout: DockLayout, openPanes: number): boolean {
  return layout.split && openPanes >= MAX_DOCK_PANES;
}
