export type ShortcutAction =
  | "new-session"
  | "toggle-sidebar"
  | "open-settings"
  | "open-transcript-search"
  | "open-projects"
  | "focus-model-picker"
  | "close-overlay";

export interface ShortcutDefinition {
  action: ShortcutAction;
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  allowInTextInput?: boolean;
  label: string;
}

/**
 * The app-level bindings intentionally use the primary modifier on both
 * platforms. The visible labels stay Ctrl-based so the web UI has one stable,
 * copyable vocabulary and the browser's native Meta bindings remain usable.
 */
export const GLOBAL_SHORTCUTS: readonly ShortcutDefinition[] = [
  {
    action: "new-session",
    key: "n",
    ctrl: true,
    label: "Ctrl+N",
  },
  {
    action: "toggle-sidebar",
    key: "b",
    ctrl: true,
    label: "Ctrl+B",
  },
  {
    action: "open-settings",
    key: ",",
    ctrl: true,
    label: "Ctrl+,",
  },
  {
    action: "open-transcript-search",
    key: "f",
    ctrl: true,
    shift: true,
    label: "Ctrl+Shift+F",
  },
  {
    action: "open-projects",
    key: "p",
    ctrl: true,
    shift: true,
    label: "Ctrl+Shift+P",
  },
  {
    action: "focus-model-picker",
    key: "m",
    label: "M",
  },
  {
    action: "close-overlay",
    key: "Escape",
    allowInTextInput: true,
    label: "Escape",
  },
];

export const SHORTCUTS = GLOBAL_SHORTCUTS;

function primaryModifierPressed(
  event: Pick<KeyboardEvent, "ctrlKey" | "metaKey">,
): boolean {
  // Ctrl+Meta is an extra modifier combination, not a platform equivalent.
  return event.ctrlKey !== event.metaKey;
}

/** Text entry controls own their printable and navigation keys. */
export function isTextInputTarget(target: EventTarget | null): boolean {
  if (!(typeof Element !== "undefined" && target instanceof Element)) {
    return false;
  }

  for (
    let element: Element | null = target;
    element;
    element = element.parentElement
  ) {
    if (
      (typeof HTMLInputElement !== "undefined" &&
        element instanceof HTMLInputElement) ||
      (typeof HTMLTextAreaElement !== "undefined" &&
        element instanceof HTMLTextAreaElement) ||
      (typeof HTMLSelectElement !== "undefined" &&
        element instanceof HTMLSelectElement) ||
      element.getAttribute("role") === "textbox" ||
      (element instanceof HTMLElement && element.isContentEditable)
    ) {
      return true;
    }
  }
  return false;
}

export function matchesShortcut(
  event: Pick<
    KeyboardEvent,
    "key" | "ctrlKey" | "metaKey" | "shiftKey" | "altKey"
  >,
  shortcut: ShortcutDefinition,
): boolean {
  const wantsPrimary = shortcut.ctrl === true;
  const hasPrimary = primaryModifierPressed(event);
  if (wantsPrimary !== hasPrimary) return false;
  if (Boolean(shortcut.shift) !== event.shiftKey) return false;
  if (event.altKey) return false;
  return event.key.toLocaleLowerCase() === shortcut.key.toLocaleLowerCase();
}

export function shortcutForEvent(
  event: Pick<
    KeyboardEvent,
    "key" | "ctrlKey" | "metaKey" | "shiftKey" | "altKey"
  >,
): ShortcutDefinition | undefined {
  return GLOBAL_SHORTCUTS.find((shortcut) => matchesShortcut(event, shortcut));
}

export function shortcutLabel(action: ShortcutAction): string | undefined {
  return GLOBAL_SHORTCUTS.find((shortcut) => shortcut.action === action)?.label;
}

export interface GlobalShortcutOptions {
  onAction: (action: ShortcutAction, event: KeyboardEvent) => void;
  isEnabled?: (action: ShortcutAction) => boolean;
  target?: Document;
}

/** Register once at the app boundary and return a deterministic cleanup. */
export function registerGlobalShortcuts({
  onAction,
  isEnabled,
  target = document,
}: GlobalShortcutOptions): () => void {
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.defaultPrevented) return;
    const shortcut = shortcutForEvent(event);
    if (!shortcut) return;
    if (!shortcut.allowInTextInput && isTextInputTarget(event.target)) return;
    if (isEnabled && !isEnabled(shortcut.action)) return;
    event.preventDefault();
    onAction(shortcut.action, event);
  };

  target.addEventListener("keydown", onKeyDown);
  return () => target.removeEventListener("keydown", onKeyDown);
}

export const installGlobalShortcuts = registerGlobalShortcuts;
