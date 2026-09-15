import {
  Archive,
  ArchiveRestore,
  ArrowLeftRight,
  ChevronDown,
  Columns2,
  Download,
  Folder,
  GitBranch,
  Menu,
  MoreHorizontal,
  PanelRight,
  Pencil,
  Pin,
  PinOff,
  RefreshCw,
  SquareTerminal,
  X,
} from "lucide-react";
import {
  type CSSProperties,
  type RefObject,
  lazy,
  memo,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { ActivityRail } from "./components/ActivityRail";
import { Conversation } from "./components/Conversation";
import { FleetOverview } from "./components/FleetOverview";
import { GoalBadge } from "./components/GoalBadge";
import { DevicesView } from "./components/Devices";
import {
  Inspector,
  type InspectorSelection,
} from "./components/Inspector";
import { SettingsView } from "./components/Settings";
import { Sidebar } from "./components/Sidebar";
import { ProjectsView } from "./components/Projects";
import {
  disposeTerminalCache,
  TerminalPanel,
} from "./components/TerminalPanel";
import { UsagePage } from "./pages/UsagePage";
import { OctetGlyph } from "./components/OctetGlyph";
import type {
  AttachmentRef,
  AuthorityProfile,
  DocumentReference,
  GoalState,
  ReasoningEffort,
  SessionSnapshot,
  SessionStatus,
  TrustedFileEntry,
  TranscriptSearchRequest,
  TranscriptSearchResult,
  UsagePeriod,
} from "./protocol";
import {
  AttentionNotificationManager,
  browserNotificationAdapter,
} from "./notifications";
import {
  sessionIdFromPathname,
  OctetStore,
  useOctetStore,
} from "./store";
import { displaySessionTitle, isUntitledSession } from "./session-title";
import { resolveDelegatedParentSessionId } from "./delegated-session";
import {
  goalCommandHelp,
  goalStatusMessage,
  type GoalCommand,
} from "./components/ComposerCommands/goal";
import { applyStoredTypePreferences } from "./theme";
import {
  DEFAULT_DOCK_LAYOUT,
  type DockLayout,
  type DockPaneId,
  type DockSlot,
  dockSlotFor,
  dockSplitVisible,
  moveDockPane,
  parseDockLayout,
  serializeDockLayout,
  setDockSplit,
} from "./workspace-layout";
import {
  GLOBAL_SHORTCUTS,
  registerGlobalShortcuts,
  type ShortcutAction,
} from "./shortcuts";
import {
  createTransport,
  type TransportConnectionState,
  transportModeFromSearch,
} from "./transport";

const FilesPanel = lazy(() =>
  import("./components/FilesPanel").then((module) => ({
    default: module.FilesPanel,
  })),
);

type Surface =
  | "fleet"
  | "session"
  | "projects"
  | "files"
  | "usage"
  | "settings"
  | "devices";

const statusLabel: Record<SessionStatus, string> = {
  idle: "Ready",
  working: "Working",
  needs_attention: "Needs attention",
  done: "Done",
  failed: "Failed",
  stopped: "Stopped",
  disconnected: "Reconnecting",
};

const transportMode = transportModeFromSearch(window.location.search);
const store = new OctetStore(createTransport(transportMode));
const activityPaneStorageKey = "octet.ui.activity-width";
const inspectorPaneStorageKey = "octet.ui.inspector-width";
const terminalPaneStorageKey = "octet.ui.terminal-width";
const terminalPaneOpenStorageKey = "octet.ui.terminal.open";
const dockLayoutStorageKey = "octet.ui.dock.layout";
const notificationPreferenceKey = (hostId: string) =>
  `octet.notifications.enabled.${encodeURIComponent(hostId)}`;

function writeFleetRoute() {
  const route = `/overview${window.location.search}`;
  window.history.pushState(null, "", route);
}

const MemoizedInspector = memo(
  Inspector,
  (previous, next) =>
    previous.session.sessionId === next.session.sessionId &&
    previous.session.outputs === next.session.outputs &&
    previous.session.sources === next.session.sources &&
    previous.session.previews === next.session.previews &&
    previous.selection === next.selection &&
    previous.closing === next.closing &&
    previous.modal === next.modal &&
    previous.dockSlot === next.dockSlot &&
    previous.previewsAvailable === next.previewsAvailable &&
    previous.resourceContentUrl === next.resourceContentUrl &&
    previous.onRestoreFocus === next.onRestoreFocus &&
    previous.onClose === next.onClose,
);

function storedPaneWidth(key: string, fallback: number): number {
  try {
    const value = Number(window.localStorage.getItem(key));
    return Number.isFinite(value) && value > 0 ? value : fallback;
  } catch {
    return fallback;
  }
}

function persistPaneWidth(key: string, value: number) {
  try {
    window.localStorage.setItem(key, String(Math.round(value)));
  } catch {
    // A hardened browser may disable storage; resizing still works in memory.
  }
}

function storedBoolean(key: string): boolean {
  try {
    return window.localStorage.getItem(key) === "true";
  } catch {
    return false;
  }
}

function storedDockLayout(): DockLayout {
  try {
    return parseDockLayout(window.localStorage.getItem(dockLayoutStorageKey));
  } catch {
    return DEFAULT_DOCK_LAYOUT;
  }
}

function persistDockLayout(layout: DockLayout): void {
  try {
    window.localStorage.setItem(
      dockLayoutStorageKey,
      serializeDockLayout(layout),
    );
  } catch {
    // A hardened browser may disable storage; the layout still works in memory.
  }
}

function persistBoolean(key: string, value: boolean): void {
  try {
    window.localStorage.setItem(key, String(value));
  } catch {
    // A hardened browser may disable storage; the panel still works in memory.
  }
}

function localStorageIfAvailable(): Storage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

function storedNotificationPreference(hostId: string): boolean {
  try {
    return (
      window.localStorage.getItem(notificationPreferenceKey(hostId)) === "true"
    );
  } catch {
    return false;
  }
}

function persistNotificationPreference(hostId: string, enabled: boolean) {
  try {
    window.localStorage.setItem(
      notificationPreferenceKey(hostId),
      String(enabled),
    );
  } catch {
    // A storage-hardened browser keeps the preference for this page only.
  }
}

function FixtureModeLabel() {
  if (!import.meta.env.DEV || transportMode !== "fixture") return null;
  return (
    <div className="fixture-mode-label" role="status">
      Demo data · responses and actions are simulated
    </div>
  );
}

function ConnectionBanner({
  connection,
}: {
  connection: TransportConnectionState;
}) {
  if (connection === "connected") return null;
  return (
    <div className="connection-banner" role="status">
      <RefreshCw className="spin" aria-hidden="true" />
      <span>
        {connection === "reconnecting"
          ? "Connection interrupted. Reconnecting to octet…"
          : "Connecting to local octet…"}
      </span>
      <small>Your current task remains visible while octet reconnects.</small>
    </div>
  );
}

export function SessionSelectionErrorBanner({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="session-selection-error" role="alert">
      <X aria-hidden="true" />
      <span>
        <strong>Could not open that session.</strong>
        <small>{message}</small>
      </span>
      <button type="button" onClick={onRetry}>
        Try again
      </button>
    </div>
  );
}

function LoadingState() {
  return (
    <div className="app-loading" role="status" aria-live="polite">
      <OctetGlyph />
      <span className="loading-pulse" aria-hidden="true" />
      <strong>Connecting to octet</strong>
      <p>Preparing your workspace.</p>
    </div>
  );
}

function ErrorState({
  message,
  onRetry,
}: {
  message: string;
  onRetry?: () => void;
}) {
  return (
    <div className="app-error" role="alert">
      <div className="error-mark">
        <X aria-hidden="true" />
      </div>
      <h1>octet could not connect</h1>
      <p>{message}</p>
      {onRetry ? <small>Retrying automatically in the background.</small> : null}
      <button
        className="primary-button"
        onClick={onRetry ?? (() => window.location.reload())}
      >
        <RefreshCw aria-hidden="true" />
        Try now
      </button>
    </div>
  );
}

const shortcutDescriptions: Record<ShortcutAction, string> = {
  "new-session": "Start a new task",
  "toggle-sidebar": "Show or hide the sidebar",
  "open-settings": "Open settings",
  "open-transcript-search": "Focus task and transcript search",
  "open-projects": "Open projects",
  "focus-model-picker": "Focus the model picker",
  "close-overlay": "Close the active panel or overlay",
};

function ShortcutReference() {
  return (
    <details
      className="shortcut-reference"
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        event.preventDefault();
        event.currentTarget.open = false;
        event.currentTarget.querySelector("summary")?.focus();
      }}
    >
      <summary
        aria-label="Keyboard shortcuts"
        title="Keyboard shortcuts"
      >
        <span aria-hidden="true">?</span>
      </summary>
      <div
        className="shortcut-reference-panel"
        role="dialog"
        aria-label="Keyboard shortcuts"
      >
        <strong>Keyboard shortcuts</strong>
        <dl>
          {GLOBAL_SHORTCUTS.map((shortcut) => (
            <div key={shortcut.action}>
              <dt>
                <kbd>{shortcut.label}</kbd>
              </dt>
              <dd>{shortcutDescriptions[shortcut.action]}</dd>
            </div>
          ))}
        </dl>
      </div>
    </details>
  );
}

interface HeaderProps {
  sidebarOpen: boolean;
  sessionId: string;
  sessionTitle: string;
  projectName: string;
  status: SessionStatus;
  goal?: GoalState | null;
  activityAvailable: boolean;
  activityOpen: boolean;
  terminalAvailable: boolean;
  terminalOpen: boolean;
  pinned: boolean;
  archived: boolean;
  sessionActionsAvailable: boolean;
  metadataActionsAvailable: boolean;
  branchHistoryAvailable: boolean;
  sessionExportAvailable: boolean;
  activityButtonRef: RefObject<HTMLButtonElement | null>;
  sidebarButtonRef: RefObject<HTMLButtonElement | null>;
  terminalButtonRef?: RefObject<HTMLButtonElement | null>;
  /** User-created dock split: the second pane column exists only when enabled. */
  dockSplitAvailable?: boolean;
  dockSplitOn?: boolean;
  dockOrder?: readonly DockPaneId[];
  onToggleDockSplit?: () => void;
  onMoveDockPane?: (pane: DockPaneId, offset: -1 | 1) => void;
  onOpenSidebar: () => void;
  onToggleActivity: () => void;
  onToggleTerminal: () => void;
  onRename: (title: string) => void;
  onPin: (pinned: boolean) => void;
  onArchive: (archived: boolean) => void;
  onOpenBranchHistory: () => void;
}

export function SessionHeader({
  sidebarOpen,
  sessionId,
  sessionTitle,
  projectName,
  status,
  goal = null,
  activityAvailable,
  activityOpen,
  terminalAvailable,
  terminalOpen,
  pinned,
  archived,
  sessionActionsAvailable,
  metadataActionsAvailable,
  branchHistoryAvailable,
  sessionExportAvailable,
  activityButtonRef,
  sidebarButtonRef,
  terminalButtonRef,
  dockSplitAvailable = false,
  dockSplitOn = false,
  dockOrder = [],
  onToggleDockSplit,
  onMoveDockPane,
  onOpenSidebar,
  onToggleActivity,
  onToggleTerminal,
  onRename,
  onPin,
  onArchive,
  onOpenBranchHistory,
}: HeaderProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const displayTitle = displaySessionTitle(sessionTitle);
  const [draftTitle, setDraftTitle] = useState(displayTitle);
  const menuTriggerRef = useRef<HTMLButtonElement>(null);
  const renameFinishedRef = useRef(false);

  useEffect(() => {
    if (!menuOpen) return;
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setMenuOpen(false);
      window.requestAnimationFrame(() => menuTriggerRef.current?.focus());
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [menuOpen]);

  const finishRename = (commit: boolean, restoreFocus: boolean) => {
    if (renameFinishedRef.current) return;
    renameFinishedRef.current = true;
    const nextTitle = draftTitle.trim();
    if (commit && nextTitle && nextTitle !== displayTitle.trim()) {
      onRename(draftTitle);
    }
    setRenaming(false);
    setMenuOpen(false);
    if (restoreFocus) {
      window.requestAnimationFrame(() => menuTriggerRef.current?.focus());
    }
  };

  return (
    <header className="session-header">
      <div className="session-header-leading">
        {!sidebarOpen ? (
          <button
            ref={sidebarButtonRef}
            className="icon-button open-sidebar"
            onClick={onOpenSidebar}
            aria-keyshortcuts="Control+B"
            title="Open sidebar (Ctrl+B)"
          >
            <Menu aria-hidden="true" />
            <span className="sr-only">Open sidebar</span>
          </button>
        ) : null}
        <div className="session-breadcrumb">
          <span>
            <Folder aria-hidden="true" />
            {projectName}
          </span>
          <ChevronDown aria-hidden="true" />
          {renaming ? (
            <input
              autoFocus
              value={draftTitle}
              onChange={(event) => setDraftTitle(event.target.value)}
              onBlur={() => finishRename(true, false)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  finishRename(true, true);
                }
                if (event.key === "Escape") {
                  event.preventDefault();
                  finishRename(false, true);
                }
              }}
              aria-label="Task title"
            />
          ) : (
            <strong>{displayTitle}</strong>
          )}
        </div>
      </div>

      <div className="session-header-actions">
        <ShortcutReference />
        <GoalBadge
          goal={goal}
          working={status === "working" || status === "needs_attention"}
        />
        <span className={`header-status is-${status}`}>
          {status === "working" ? (
            <span className="status-orbit" aria-hidden="true" />
          ) : null}
          {statusLabel[status]}
        </span>
        {terminalAvailable ? (
          <button
            ref={terminalButtonRef}
            className={`icon-button ${terminalOpen ? "is-active" : ""}`}
            onClick={onToggleTerminal}
            aria-label={terminalOpen ? "Close terminal" : "Open terminal"}
            aria-pressed={terminalOpen}
            title={terminalOpen ? "Close terminal" : "Open terminal"}
          >
            <SquareTerminal aria-hidden="true" />
          </button>
        ) : null}
        {activityAvailable ? (
          <button
            ref={activityButtonRef}
            className={`icon-button ${activityOpen ? "is-active" : ""}`}
            onClick={onToggleActivity}
            aria-label={activityOpen ? "Close activity" : "Open activity"}
            title={activityOpen ? "Close activity" : "Open activity"}
          >
            <PanelRight aria-hidden="true" />
          </button>
        ) : null}
        {dockSplitAvailable && onToggleDockSplit ? (
          <button
            className={`icon-button ${dockSplitOn ? "is-active" : ""}`}
            onClick={onToggleDockSplit}
            aria-pressed={dockSplitOn}
            aria-label={
              dockSplitOn ? "Merge dock panes" : "Split dock into two panes"
            }
            title={
              dockSplitOn
                ? "Merge dock panes into one column"
                : "Show two dock panes side by side"
            }
          >
            <Columns2 aria-hidden="true" />
          </button>
        ) : null}
        {dockSplitAvailable && onMoveDockPane ? (
          dockOrder.map((pane, index) => (
            <button
              key={`dock-move-${pane}`}
              className="icon-button"
              onClick={() =>
                onMoveDockPane(pane, index === 0 ? 1 : -1)
              }
              aria-label={
                index === 0
                  ? "Move the first dock pane right"
                  : "Move the second dock pane left"
              }
              title={
                index === 0
                  ? "Move the left dock pane right"
                  : "Move the right dock pane left"
              }
            >
              <ArrowLeftRight aria-hidden="true" />
            </button>
          ))
        ) : null}
        {sessionActionsAvailable ? (
          <div className="menu-anchor">
            <button
              ref={menuTriggerRef}
              className="icon-button"
              onClick={() => setMenuOpen((open) => !open)}
              aria-expanded={menuOpen}
              aria-label="Task actions"
            >
              <MoreHorizontal aria-hidden="true" />
            </button>
            {menuOpen ? (
              <>
                <button
                  className="menu-dismiss"
                  onClick={() => setMenuOpen(false)}
                  aria-label="Close menu"
                  tabIndex={-1}
                />
                <div className="session-menu" role="menu">
                  {branchHistoryAvailable ? (
                    <button
                      role="menuitem"
                      onClick={() => {
                        setMenuOpen(false);
                        onOpenBranchHistory();
                      }}
                    >
                      <GitBranch aria-hidden="true" />
                      Task history
                    </button>
                  ) : null}
                  {sessionExportAvailable ? (
                    <a
                      role="menuitem"
                      href={`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`}
                      download
                      onClick={() => setMenuOpen(false)}
                    >
                      <Download aria-hidden="true" />
                      Download safe export
                    </a>
                  ) : null}
                  {metadataActionsAvailable ? (
                    <>
                      <button
                        role="menuitem"
                        onClick={() => {
                          setDraftTitle(displayTitle);
                          renameFinishedRef.current = false;
                          setRenaming(true);
                          setMenuOpen(false);
                        }}
                      >
                        <Pencil aria-hidden="true" />
                        Rename
                      </button>
                      <button role="menuitem" onClick={() => onPin(!pinned)}>
                        {pinned ? (
                          <PinOff aria-hidden="true" />
                        ) : (
                          <Pin aria-hidden="true" />
                        )}
                        {pinned ? "Unpin" : "Pin"}
                      </button>
                      <button
                        className={archived ? undefined : "danger-row"}
                        role="menuitem"
                        onClick={() => onArchive(!archived)}
                      >
                        {archived ? (
                          <ArchiveRestore aria-hidden="true" />
                        ) : (
                          <Archive aria-hidden="true" />
                        )}
                        {archived ? "Restore from archive" : "Archive"}
                      </button>
                    </>
                  ) : null}
                </div>
              </>
            ) : null}
          </div>
        ) : null}
      </div>
    </header>
  );
}

function BranchHistorySheet({
  session,
  onClose,
  onCheckout,
}: {
  session: SessionSnapshot;
  onClose: () => void;
  onCheckout: (entryId: string) => Promise<void>;
}) {
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const byId = useMemo(
    () => new Map(session.branches.entries.map((entry) => [entry.entryId, entry])),
    [session.branches.entries],
  );
  const activeAncestry = useMemo(() => {
    const active = new Set<string>();
    let cursor = session.branches.head;
    while (cursor && !active.has(cursor)) {
      active.add(cursor);
      cursor = byId.get(cursor)?.parentEntryId;
    }
    return active;
  }, [byId, session.branches.head]);
  const currentCheckpoint = useMemo(() => {
    let cursor = session.branches.head;
    while (cursor) {
      const entry = byId.get(cursor);
      if (!entry) return undefined;
      if (entry.checkoutable) return entry.entryId;
      cursor = entry.parentEntryId;
    }
    return undefined;
  }, [byId, session.branches.head]);
  const visibleEntries = session.branches.entries.filter(
    (entry) => entry.checkoutable,
  );
  const checkoutDisabled =
    session.activeRunId !== undefined ||
    !["idle", "done", "failed", "stopped"].includes(session.status);

  useEffect(() => {
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.preventDefault();
      onClose();
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  return (
    <div className="branch-sheet-backdrop" role="presentation">
      <section
        className="branch-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="branch-sheet-title"
      >
        <header>
          <div>
            <span className="branch-sheet-glyph" aria-hidden="true">
              <GitBranch />
            </span>
            <div>
              <h2 id="branch-sheet-title">Task history</h2>
              <p>Choose where the conversation should continue.</p>
            </div>
          </div>
          <button className="icon-button" onClick={onClose} aria-label="Close history">
            <X aria-hidden="true" />
          </button>
        </header>
        <div className="branch-history-list">
          {visibleEntries.length ? (
            [...visibleEntries].reverse().map((entry) => {
              const current = entry.entryId === currentCheckpoint;
              const active = activeAncestry.has(entry.entryId);
              return (
                <article
                  className={`branch-history-row ${active ? "is-active" : ""}`}
                  key={entry.entryId}
                >
                  <span className="branch-history-node" aria-hidden="true" />
                  <div>
                    <strong>{entry.label}</strong>
                    <small>
                      {entry.kind === "userMessage"
                        ? "Your message"
                        : entry.kind === "compaction"
                          ? "Context checkpoint"
                          : "octet response"}
                      {current ? " · Current" : ""}
                    </small>
                  </div>
                  <button
                    disabled={current || checkoutDisabled || pending !== null}
                    onClick={() => {
                      setPending(entry.entryId);
                      setError(null);
                      void onCheckout(entry.entryId)
                        .then(onClose)
                        .catch((reason: unknown) => {
                          setPending(null);
                          setError(
                            reason instanceof Error
                              ? reason.message
                              : "octet could not switch checkpoints.",
                          );
                        });
                    }}
                  >
                    {pending === entry.entryId
                      ? "Switching…"
                      : current
                        ? "Current"
                        : "Switch here"}
                  </button>
                </article>
              );
            })
          ) : (
            <p className="branch-history-empty">History appears after the first message.</p>
          )}
        </div>
        <footer>
          <p>
            This changes the conversation state. Files, commands, and other side
            effects are not rolled back.
            {session.branches.truncated
              ? " Older checkpoints are not shown in this recent-history view."
              : ""}
          </p>
          {error ? <span role="alert">{error}</span> : null}
        </footer>
      </section>
    </div>
  );
}

function UtilityTopbar({
  title,
  sidebarOpen,
  onOpenSidebar,
  sidebarButtonRef,
}: {
  title: string;
  sidebarOpen: boolean;
  onOpenSidebar: () => void;
  sidebarButtonRef: RefObject<HTMLButtonElement | null>;
}) {
  return (
    <header className="utility-topbar">
      {!sidebarOpen ? (
        <button
          ref={sidebarButtonRef}
          className="icon-button"
          onClick={onOpenSidebar}
          aria-keyshortcuts="Control+B"
          title="Open sidebar (Ctrl+B)"
        >
          <Menu aria-hidden="true" />
          <span className="sr-only">Open sidebar</span>
        </button>
      ) : null}
      <strong>{title}</strong>
      <ShortcutReference />
    </header>
  );
}

export default function App() {
  const state = useOctetStore(store);
  const [sidebarOpen, setSidebarOpen] = useState(
    () => !window.matchMedia("(max-width: 760px)").matches,
  );
  const [mobileLayout, setMobileLayout] = useState(
    () => window.matchMedia("(max-width: 760px)").matches,
  );
  const [wideLayout, setWideLayout] = useState(
    () => window.matchMedia("(min-width: 1280px)").matches,
  );
  const [terminalSplitLayout, setTerminalSplitLayout] = useState(
    () => window.matchMedia("(min-width: 900px)").matches,
  );
  const [activityOpen, setActivityOpen] = useState(false);
  const [terminalOpen, setTerminalOpen] = useState(
    () =>
      window.location.pathname !== "/overview" &&
      storedBoolean(terminalPaneOpenStorageKey),
  );
  const [branchHistoryOpen, setBranchHistoryOpen] = useState(false);
  const [delegatedParentSessionId, setDelegatedParentSessionId] = useState<
    string | null
  >(null);
  const [inspector, setInspector] = useState<InspectorSelection | null>(null);
  const [inspectorClosing, setInspectorClosing] = useState(false);
  const [activityPaneWidth, setActivityPaneWidth] = useState(() =>
    storedPaneWidth(activityPaneStorageKey, 400),
  );
  const [inspectorPaneWidth, setInspectorPaneWidth] = useState(() =>
    storedPaneWidth(inspectorPaneStorageKey, 720),
  );
  const [terminalPaneWidth, setTerminalPaneWidth] = useState(() =>
    storedPaneWidth(terminalPaneStorageKey, 460),
  );
  const [dockLayout, setDockLayoutState] = useState<DockLayout>(
    storedDockLayout,
  );
  const [surface, setSurface] = useState<Surface>(() =>
    window.location.pathname === "/overview" ? "fleet" : "session",
  );
  const notificationManagerRef =
    useRef<AttentionNotificationManager | null>(null);
  const [notificationState, setNotificationState] = useState<{
    supported: boolean;
    enabled: boolean;
    permission: NotificationPermission | "unsupported";
  }>({
    supported: false,
    enabled: false,
    permission: "unsupported",
  });
  const activityButtonRef = useRef<HTMLButtonElement>(null);
  const sidebarButtonRef = useRef<HTMLButtonElement>(null);
  const terminalButtonRef = useRef<HTMLButtonElement>(null);
  const inspectorCloseTimerRef = useRef<number | null>(null);
  const paneResizeCleanupRef = useRef<(() => void) | null>(null);
  const restoreActivityFocus = useCallback(() => {
    const restore = () => activityButtonRef.current?.focus();
    restore();
    window.requestAnimationFrame(restore);
  }, []);
  const restoreSidebarFocus = useCallback(() => {
    const restore = () => sidebarButtonRef.current?.focus();
    restore();
    window.requestAnimationFrame(restore);
  }, []);
  const restoreTerminalFocus = useCallback(() => {
    const restore = () => terminalButtonRef.current?.focus();
    restore();
    window.requestAnimationFrame(restore);
  }, []);
  const closeActivity = useCallback(() => {
    setActivityOpen(false);
    restoreActivityFocus();
  }, [restoreActivityFocus]);
  const closeInspector = useCallback(() => {
    if (inspectorCloseTimerRef.current !== null) return;
    if (!wideLayout) {
      setInspector(null);
      setInspectorClosing(false);
      restoreActivityFocus();
      return;
    }
    setInspectorClosing(true);
    inspectorCloseTimerRef.current = window.setTimeout(() => {
      inspectorCloseTimerRef.current = null;
      setInspector(null);
      setInspectorClosing(false);
      restoreActivityFocus();
    }, 180);
  }, [restoreActivityFocus, wideLayout]);
  const closeSidebar = useCallback(() => {
    setSidebarOpen(false);
    restoreSidebarFocus();
  }, [restoreSidebarFocus]);

  useEffect(() => {
    applyStoredTypePreferences();
    void store.initialize();
    return () => {
      if (inspectorCloseTimerRef.current !== null) {
        window.clearTimeout(inspectorCloseTimerRef.current);
      }
      paneResizeCleanupRef.current?.();
      disposeTerminalCache();
      store.dispose();
    };
  }, []);

  useEffect(() => {
    if (!state.error || state.bootstrap) return;
    let cancelled = false;
    let delay = 1_000;
    let timer = 0;
    const retry = () => {
      timer = window.setTimeout(() => {
        if (cancelled) return;
        void store.initialize().finally(() => {
          if (cancelled || store.getSnapshot().ready) return;
          delay = Math.min(delay * 2, 8_000);
          retry();
        });
      }, delay);
    };
    retry();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [state.bootstrap, state.error]);

  useEffect(() => {
    const hostId = state.bootstrap?.host.id;
    if (!hostId) return;
    const manager = new AttentionNotificationManager(
      hostId,
      browserNotificationAdapter(),
      localStorageIfAvailable(),
    );
    notificationManagerRef.current = manager;
    let cancelled = false;
    const preferred = storedNotificationPreference(hostId);
    const initialFrame = window.requestAnimationFrame(() => {
      if (cancelled) return;
      setNotificationState({
        supported: manager.supported,
        enabled: false,
        permission: manager.permission,
      });
    });
    if (preferred && manager.permission === "granted") {
      void manager.enable().then((enabled) => {
        if (cancelled) return;
        setNotificationState({
          supported: manager.supported,
          enabled,
          permission: manager.permission,
        });
      });
    }
    return () => {
      cancelled = true;
      window.cancelAnimationFrame(initialFrame);
      manager.disable();
      if (notificationManagerRef.current === manager) {
        notificationManagerRef.current = null;
      }
    };
  }, [state.bootstrap?.host.id]);

  useEffect(() => {
    const onPopState = () => {
      const sessionId = sessionIdFromPathname(window.location.pathname);
      if (sessionId) {
        setSurface("session");
        void store.selectSession(sessionId, "none").catch(() => undefined);
        return;
      }
      if (window.location.pathname === "/overview") {
        store.cancelSessionSelection();
        setSurface("fleet");
        setInspector(null);
        setActivityOpen(false);
        setBranchHistoryOpen(false);
        setTerminalOpen(false);
        persistBoolean(terminalPaneOpenStorageKey, false);
        return;
      }
      if (window.location.pathname === "/") setSurface("session");
    };
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  const session = state.selectedSessionId
    ? state.sessions[state.selectedSessionId]
    : null;
  const selectedSummary = state.bootstrap?.sessions.find(
    (summary) => summary.id === state.selectedSessionId,
  );
  const project = state.bootstrap?.projects.find(
    (candidate) => candidate.id === session?.projectId,
  );
  const delegatedSessionReadOnly = Boolean(
    session?.sessionId.startsWith("agent-session:"),
  );
  const delegatedReturnParentSessionId = resolveDelegatedParentSessionId(
    session,
    delegatedParentSessionId,
  );
  const terminalAvailable = Boolean(
    !delegatedSessionReadOnly && state.bootstrap?.capabilities.terminal,
  );

  const closeTerminal = useCallback(
    (restoreFocus = false) => {
      setTerminalOpen(false);
      persistBoolean(terminalPaneOpenStorageKey, false);
      if (restoreFocus) restoreTerminalFocus();
    },
    [restoreTerminalFocus],
  );

  const visibleTerminalOpen =
    surface === "session" && terminalAvailable && terminalOpen;

  const activityAvailable = Boolean(
    session &&
      (session.items.some((item) => item.kind === "run_outcome") ||
        (session.extensionPresentations?.length ?? 0) > 0 ||
        session.progress.length ||
        (state.bootstrap?.capabilities.resources &&
          (session.outputs.length || session.sources.length))),
  );
  const visibleActivityOpen =
    surface === "session" && activityOpen && activityAvailable;
  // The dock hosts at most two panes. The default (unsplit) layout keeps the
  // historical single-column behaviour; a user-created split shows the activity
  // rail and the inspector side by side in the persisted order.
  const dockPanesOpen = [visibleActivityOpen, Boolean(inspector)].filter(
    Boolean,
  ).length;
  const dockSplitActive =
    surface === "session" &&
    wideLayout &&
    !visibleTerminalOpen &&
    dockSplitVisible(dockLayout, dockPanesOpen);
  const updateDockLayout = useCallback((next: DockLayout) => {
    setDockLayoutState(next);
    persistDockLayout(next);
  }, []);
  const toggleDockSplit = useCallback(() => {
    updateDockLayout(setDockSplit(dockLayout, !dockLayout.split));
  }, [dockLayout, updateDockLayout]);
  const reorderDockPane = useCallback(
    (pane: DockPaneId, offset: -1 | 1) => {
      updateDockLayout(moveDockPane(dockLayout, pane, offset));
    },
    [dockLayout, updateDockLayout],
  );
  const modalWorkspaceOpen =
    surface === "session" &&
    (branchHistoryOpen ||
      (!wideLayout && (visibleActivityOpen || Boolean(inspector))) ||
      (!terminalSplitLayout && visibleTerminalOpen));
  const closeBranchHistory = useCallback(() => {
    setBranchHistoryOpen(false);
    window.requestAnimationFrame(() => {
      document
        .querySelector<HTMLButtonElement>('[aria-label="Task actions"]')
        ?.focus();
    });
  }, []);

  const focusSidebarSearch = useCallback(() => {
    if (branchHistoryOpen) setBranchHistoryOpen(false);
    if (mobileLayout) {
      if (inspector) closeInspector();
      if (visibleActivityOpen) closeActivity();
      if (visibleTerminalOpen) closeTerminal();
    }
    setSidebarOpen(true);
    const focus = () => {
      const search = document.querySelector<HTMLInputElement>(
        ".sidebar-search input",
      );
      if (!search || search.closest("[inert]")) return;
      search.focus();
    };
    focus();
    window.requestAnimationFrame(() => {
      focus();
      window.requestAnimationFrame(focus);
    });
  }, [
    branchHistoryOpen,
    closeActivity,
    closeInspector,
    closeTerminal,
    inspector,
    mobileLayout,
    visibleActivityOpen,
    visibleTerminalOpen,
  ]);

  const focusModelPicker = useCallback(() => {
    if (surface !== "session" || modalWorkspaceOpen) return;
    if (mobileLayout && sidebarOpen) closeSidebar();
    const focus = () => {
      const picker = document.querySelector<HTMLButtonElement>(
        ".model-picker-trigger",
      );
      if (!picker || picker.disabled || picker.closest("[inert]")) return;
      picker.focus();
    };
    window.requestAnimationFrame(() => {
      focus();
      window.requestAnimationFrame(focus);
    });
  }, [
    closeSidebar,
    mobileLayout,
    modalWorkspaceOpen,
    sidebarOpen,
    surface,
  ]);

  const closeOverlay = useCallback(() => {
    if (branchHistoryOpen) {
      closeBranchHistory();
      return;
    }
    if (inspector) {
      closeInspector();
      return;
    }
    if (visibleActivityOpen) {
      closeActivity();
      return;
    }
    if (visibleTerminalOpen) {
      closeTerminal(true);
      return;
    }
    if (mobileLayout && sidebarOpen) closeSidebar();
  }, [
    branchHistoryOpen,
    closeActivity,
    closeBranchHistory,
    closeInspector,
    closeSidebar,
    closeTerminal,
    inspector,
    mobileLayout,
    sidebarOpen,
    visibleActivityOpen,
    visibleTerminalOpen,
  ]);

  const hasClosableOverlay =
    branchHistoryOpen ||
    Boolean(inspector) ||
    visibleActivityOpen ||
    visibleTerminalOpen ||
    (mobileLayout && sidebarOpen);
  const canInterrupt = Boolean(
    session?.activeRunId ||
      session?.status === "working" ||
      session?.status === "needs_attention",
  );

  useEffect(() => {
    const media = window.matchMedia("(max-width: 760px)");
    const onChange = (event: MediaQueryListEvent) => {
      setMobileLayout(event.matches);
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);

  useEffect(() => {
    const media = window.matchMedia("(min-width: 900px)");
    const onChange = (event: MediaQueryListEvent) => {
      setTerminalSplitLayout(event.matches);
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);

  useEffect(() => {
    const media = window.matchMedia("(min-width: 1280px)");
    const onChange = (event: MediaQueryListEvent) => {
      setWideLayout(event.matches);
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);

  const openOutput = useCallback(
    (outputId: string) => {
      closeTerminal();
      if (inspectorCloseTimerRef.current !== null) {
        window.clearTimeout(inspectorCloseTimerRef.current);
        inspectorCloseTimerRef.current = null;
      }
      setInspectorClosing(false);
      setActivityOpen(false);
      setInspector({ type: "output", id: outputId });
    },
    [closeTerminal],
  );
  const openSource = useCallback(
    (sourceId: string) => {
      closeTerminal();
      if (inspectorCloseTimerRef.current !== null) {
        window.clearTimeout(inspectorCloseTimerRef.current);
        inspectorCloseTimerRef.current = null;
      }
      setInspectorClosing(false);
      setActivityOpen(false);
      setInspector({ type: "source", id: sourceId });
    },
    [closeTerminal],
  );
  const openResource = useCallback(
    (
      handle: string,
      title: string,
      presentation: "text" | "diff" | "image",
    ) => {
      closeTerminal();
      if (inspectorCloseTimerRef.current !== null) {
        window.clearTimeout(inspectorCloseTimerRef.current);
        inspectorCloseTimerRef.current = null;
      }
      setInspectorClosing(false);
      setActivityOpen(false);
      setInspector({ type: "resource", handle, title, presentation });
    },
    [closeTerminal],
  );

  const paneBounds = useCallback(
    (kind: "activity" | "inspector" | "terminal") => {
      const sidebarWidth = sidebarOpen ? 296 : 0;
      if (kind === "activity") {
        return {
          min: 280,
          max: Math.max(
            280,
            Math.min(440, window.innerWidth - sidebarWidth - 520),
          ),
        };
      }
      if (kind === "terminal") {
        return {
          min: 300,
          max: Math.max(
            300,
            Math.min(720, window.innerWidth - sidebarWidth - 360),
          ),
        };
      }
      return {
        min: 520,
        max: Math.max(520, window.innerWidth - sidebarWidth - 380),
      };
    },
    [sidebarOpen],
  );

  const resizePaneBy = useCallback(
    (kind: "activity" | "inspector" | "terminal", delta: number) => {
      const bounds = paneBounds(kind);
      const current =
        kind === "activity"
          ? activityPaneWidth
          : kind === "inspector"
            ? inspectorPaneWidth
            : terminalPaneWidth;
      const next = Math.max(bounds.min, Math.min(bounds.max, current + delta));
      if (kind === "activity") {
        setActivityPaneWidth(next);
        persistPaneWidth(activityPaneStorageKey, next);
      } else if (kind === "inspector") {
        setInspectorPaneWidth(next);
        persistPaneWidth(inspectorPaneStorageKey, next);
      } else {
        setTerminalPaneWidth(next);
        persistPaneWidth(terminalPaneStorageKey, next);
      }
    },
    [activityPaneWidth, inspectorPaneWidth, paneBounds, terminalPaneWidth],
  );

  const beginPaneResize = useCallback(
    (kind: "activity" | "inspector" | "terminal", startX: number) => {
      paneResizeCleanupRef.current?.();
      const bounds = paneBounds(kind);
      const startWidth =
        kind === "activity"
          ? activityPaneWidth
          : kind === "inspector"
            ? inspectorPaneWidth
            : terminalPaneWidth;
      let nextWidth = startWidth;
      const onMove = (event: PointerEvent) => {
        nextWidth = Math.max(
          bounds.min,
          Math.min(bounds.max, startWidth + startX - event.clientX),
        );
        if (kind === "activity") setActivityPaneWidth(nextWidth);
        else if (kind === "inspector") setInspectorPaneWidth(nextWidth);
        else setTerminalPaneWidth(nextWidth);
      };
      const cleanup = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", finish);
        window.removeEventListener("pointercancel", finish);
        window.removeEventListener("blur", finish);
        document.documentElement.classList.remove("is-resizing-pane");
        paneResizeCleanupRef.current = null;
      };
      const finish = () => {
        cleanup();
        persistPaneWidth(
          kind === "activity"
            ? activityPaneStorageKey
            : kind === "inspector"
              ? inspectorPaneStorageKey
              : terminalPaneStorageKey,
          nextWidth,
        );
      };
      paneResizeCleanupRef.current = cleanup;
      document.documentElement.classList.add("is-resizing-pane");
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", finish);
      window.addEventListener("pointercancel", finish);
      window.addEventListener("blur", finish);
    },
    [activityPaneWidth, inspectorPaneWidth, paneBounds, terminalPaneWidth],
  );

  const dockResizeTargets = useMemo(() => {
    if (surface !== "session" || !wideLayout || visibleTerminalOpen) return [];
    if (dockSplitActive) {
      return [
        { pane: dockLayout.order[0], slot: "a" as DockSlot },
        { pane: dockLayout.order[1], slot: "b" as DockSlot },
      ];
    }
    if (visibleActivityOpen && !inspector) {
      return [{ pane: "activity" as DockPaneId, slot: "a" as DockSlot }];
    }
    if (inspector) {
      return [{ pane: "inspector" as DockPaneId, slot: "a" as DockSlot }];
    }
    return [];
  }, [
    dockLayout.order,
    dockSplitActive,
    inspector,
    surface,
    visibleActivityOpen,
    visibleTerminalOpen,
    wideLayout,
  ]);
  const dockWidthFor = useCallback(
    (pane: DockPaneId | undefined) =>
      pane === "inspector" ? inspectorPaneWidth : activityPaneWidth,
    [activityPaneWidth, inspectorPaneWidth],
  );
  const appClass = useMemo(
    () =>
      [
        "app-shell",
        sidebarOpen ? "has-sidebar" : "",
        dockSplitActive
          ? "has-dock-split"
          : visibleActivityOpen && !inspector
            ? "has-activity"
            : "",
        inspector ? "has-inspector" : "",
        visibleTerminalOpen ? "has-terminal" : "",
        `surface-${surface}`,
      ]
        .filter(Boolean)
        .join(" "),
    [
      dockSplitActive,
      inspector,
      sidebarOpen,
      surface,
      visibleActivityOpen,
      visibleTerminalOpen,
    ],
  );
  const appStyle = {
    "--activity-width": `${activityPaneWidth}px`,
    "--inspector-width": `${inspectorPaneWidth}px`,
    "--dock-a-width": `${dockWidthFor(dockLayout.order[0])}px`,
    "--dock-b-width": `${dockWidthFor(dockLayout.order[1])}px`,
    "--terminal-width": `${terminalPaneWidth}px`,
  } as CSSProperties;
  const selectionError = state.selectionError;
  const selectionErrorBanner = selectionError ? (
    <SessionSelectionErrorBanner
      message={selectionError.message}
      onRetry={() => {
        void store
          .selectSession(selectionError.sessionId, selectionError.routeMode)
          .catch(() => undefined);
      }}
    />
  ) : null;
  const toggleTerminal = useCallback(() => {
    if (!terminalAvailable) return;
    if (terminalOpen) {
      closeTerminal();
      return;
    }
    setTerminalOpen(true);
    persistBoolean(terminalPaneOpenStorageKey, true);
    setActivityOpen(false);
    setInspector(null);
  }, [closeTerminal, terminalAvailable, terminalOpen]);
  const openFleet = useCallback(() => {
    store.cancelSessionSelection();
    closeTerminal();
    setInspector(null);
    setActivityOpen(false);
    setBranchHistoryOpen(false);
    setSurface("fleet");
    if (window.location.pathname !== "/overview") writeFleetRoute();
    if (window.matchMedia("(max-width: 760px)").matches) {
      setSidebarOpen(false);
    }
  }, [closeTerminal]);
  const startNewSession = useCallback(() => {
    setInspector(null);
    setActivityOpen(false);
    if (
      session &&
      selectedSummary?.lifecycle === "active" &&
      session.items.length === 0 &&
      session.status === "idle" &&
      isUntitledSession(session.title)
    ) {
      setSurface("session");
      void store.selectSession(session.sessionId);
      return;
    }
    if (session) setSurface("session");
    void store.createSession().then(() => {
      if (store.selectedSession) setSurface("session");
    });
  }, [selectedSummary?.lifecycle, session]);
  const selectSession = useCallback(
    (sessionId: string) => {
      const delegatedTarget = sessionId.startsWith("agent-session:");
      if (
        delegatedTarget &&
        session &&
        !session.sessionId.startsWith("agent-session:")
      ) {
        setDelegatedParentSessionId(session.sessionId);
      } else if (!delegatedTarget) {
        setDelegatedParentSessionId(null);
      }
      const revealAfterSelection = !session;
      if (!revealAfterSelection) setSurface("session");
      setInspector(null);
      if (sessionId !== state.selectedSessionId) {
        setActivityOpen(false);
      }
      void store
        .selectSession(sessionId)
        .then(() => {
          if (
            revealAfterSelection &&
            store.getSnapshot().selectedSessionId === sessionId
          ) {
            setSurface("session");
          }
        })
        .catch(() => undefined);
      if (window.matchMedia("(max-width: 760px)").matches) {
        setSidebarOpen(false);
      }
    },
    [session, state.selectedSessionId],
  );
  const searchTranscripts = useCallback(
    (request: TranscriptSearchRequest): Promise<TranscriptSearchResult> =>
      store.searchTranscripts(request),
    [],
  );
  const activateSearchResult = useCallback(
    (sessionId: string, itemId: string) => {
      void (async () => {
        setSurface("session");
        setInspector(null);
        setActivityOpen(false);
        if (window.matchMedia("(max-width: 760px)").matches) {
          setSidebarOpen(false);
        }
        await store.selectSession(sessionId);
        for (let attempt = 0; attempt < 12; attempt += 1) {
          await new Promise<void>((resolve) =>
            window.requestAnimationFrame(() => resolve()),
          );
          const target = document.getElementById(
            `transcript-item-${itemId}`,
          );
          if (!target) continue;
          for (
            let details = target.closest("details");
            details;
            details = details.parentElement?.closest("details") ?? null
          ) {
            details.open = true;
          }
          target.scrollIntoView({ block: "center", behavior: "smooth" });
          target.focus({ preventScroll: true });
          target.classList.add("is-search-target");
          window.setTimeout(
            () => target.classList.remove("is-search-target"),
            1_800,
          );
          break;
        }
      })().catch(() => undefined);
    },
    [],
  );
  const restoreSession = useCallback(async (sessionId: string) => {
    setSurface("session");
    setInspector(null);
    setActivityOpen(false);
    await store.selectSession(sessionId);
    await store.archive(false);
    if (window.matchMedia("(max-width: 760px)").matches) {
      setSidebarOpen(false);
    }
  }, []);
  const setSessionLifecycle = useCallback(
    async (
      sessionId: string,
      lifecycle: "active" | "archived" | "trash",
    ): Promise<void> => {
      if (lifecycle === "active") {
        await restoreSession(sessionId);
        return;
      }
      await store.setSessionLifecycle(sessionId, lifecycle);
      if (lifecycle === "archived" && sessionId === state.selectedSessionId) {
        await store.createSession();
      }
    },
    [restoreSession, state.selectedSessionId],
  );
  const changeNotifications = useCallback(
    async (enabled: boolean): Promise<boolean> => {
      const manager = notificationManagerRef.current;
      const hostId = state.bootstrap?.host.id;
      if (!manager || !hostId) return false;
      if (!enabled) {
        manager.disable();
        persistNotificationPreference(hostId, false);
        setNotificationState({
          supported: manager.supported,
          enabled: false,
          permission: manager.permission,
        });
        return false;
      }
      const granted = await manager.enable();
      persistNotificationPreference(hostId, granted);
      setNotificationState({
        supported: manager.supported,
        enabled: granted,
        permission: manager.permission,
      });
      return granted;
    },
    [state.bootstrap?.host.id],
  );
  useEffect(() => {
    const manager = notificationManagerRef.current;
    const summaries = state.bootstrap?.sessions;
    if (!manager || !summaries || !notificationState.enabled) return;
    const observe = () => {
      for (const summary of summaries) {
        manager.observe(summary, {
          hidden: document.visibilityState !== "visible",
          focused: document.hasFocus(),
          focusWindow: () => window.focus(),
          openSession: selectSession,
        });
      }
    };
    observe();
    document.addEventListener("visibilitychange", observe);
    window.addEventListener("focus", observe);
    window.addEventListener("blur", observe);
    return () => {
      document.removeEventListener("visibilitychange", observe);
      window.removeEventListener("focus", observe);
      window.removeEventListener("blur", observe);
    };
  }, [
    notificationState.enabled,
    selectSession,
    state.bootstrap?.sessions,
  ]);
  const openSettings = useCallback(() => {
    setSurface("settings");
    setInspector(null);
    if (window.matchMedia("(max-width: 760px)").matches) {
      setSidebarOpen(false);
    }
  }, []);
  const openProjects = useCallback(() => {
    setSurface("projects");
    setInspector(null);
    if (window.matchMedia("(max-width: 760px)").matches) {
      setSidebarOpen(false);
    }
  }, []);
  const openFiles = useCallback(() => {
    setSurface("files");
    setInspector(null);
    setActivityOpen(false);
    if (window.matchMedia("(max-width: 760px)").matches) {
      setSidebarOpen(false);
    }
  }, []);
  const openUsage = useCallback(() => {
    setSurface("usage");
    setInspector(null);
    if (window.matchMedia("(max-width: 760px)").matches) {
      setSidebarOpen(false);
    }
  }, []);
  const openDevices = useCallback(() => {
    setSurface("devices");
    setInspector(null);
    if (window.matchMedia("(max-width: 760px)").matches) {
      setSidebarOpen(false);
    }
  }, []);
  useEffect(() => {
    return registerGlobalShortcuts({
      onAction: (action) => {
        switch (action) {
          case "new-session":
            startNewSession();
            break;
          case "toggle-sidebar":
            if (sidebarOpen) closeSidebar();
            else setSidebarOpen(true);
            break;
          case "open-settings":
            openSettings();
            break;
          case "open-transcript-search":
            focusSidebarSearch();
            break;
          case "open-projects":
            openProjects();
            break;
          case "focus-model-picker":
            focusModelPicker();
            break;
          case "close-overlay":
            if (hasClosableOverlay) closeOverlay();
            else if (canInterrupt) void store.interrupt();
            break;
        }
      },
      isEnabled: (action) =>
        action !== "close-overlay" || hasClosableOverlay || canInterrupt,
    });
  }, [
    canInterrupt,
    closeOverlay,
    closeSidebar,
    focusModelPicker,
    focusSidebarSearch,
    hasClosableOverlay,
    openProjects,
    openSettings,
    sidebarOpen,
    startNewSession,
  ]);

  const submitSession = useCallback(
    (
      prompt: string,
      attachments: AttachmentRef[],
      activeDelivery?: "steer" | "followUp",
      idempotencyKey?: string,
      documents?: DocumentReference[],
      projectFiles?: TrustedFileEntry[],
    ) =>
      store.submit(
        prompt,
        attachments,
        activeDelivery,
        idempotencyKey,
        documents,
        projectFiles,
      ),
    [],
  );
  const goalCommand = useCallback(
    async (command: GoalCommand): Promise<string> => {
      switch (command.type) {
        case "help":
          return goalCommandHelp;
        case "status":
          return goalStatusMessage(state.goal);
        case "set": {
          const goal = await store.setGoal(command.objective);
          return `Goal set: ${goal?.objective ?? command.objective}`;
        }
        case "pause": {
          const goal = await store.pauseGoal();
          return goal ? `Goal paused: ${goal.objective}` : "Goal paused.";
        }
        case "resume": {
          const goal = await store.resumeGoal();
          return goal ? `Goal resumed: ${goal.objective}` : "Goal resumed.";
        }
        case "clear":
          await store.clearGoal();
          return "Goal cleared.";
      }
    },
    [state.goal],
  );
  const interruptSession = useCallback(() => store.interrupt(), []);
  const configureSession = useCallback(
    (patch: {
      modelId?: string;
      reasoning?: ReasoningEffort;
      authority?: AuthorityProfile;
    }) => store.configure(patch),
    [],
  );
  const resolveApproval = useCallback(
    (
      requestId: string,
      decision: "allowed_once" | "allowed_session" | "denied",
    ) => store.resolveApproval(requestId, decision),
    [],
  );
  const resolveUserInput = useCallback(
    (
      requestId: string,
      answer: Parameters<OctetStore["resolveUserInput"]>[1],
    ) => store.resolveUserInput(requestId, answer),
    [],
  );
  const ingestAttachment = useCallback(
    (file: File) => store.ingestAttachment(file),
    [],
  );
  const ingestDocument = useCallback(
    (file: File) => store.ingestDocument(file),
    [],
  );
  const selectedProjectId = session?.projectId;
  const listProjectFiles = useCallback(() => {
    if (!selectedProjectId) {
      throw new Error("This conversation is not bound to a project.");
    }
    return store.getTrustedFiles(selectedProjectId);
  }, [selectedProjectId]);
  const searchProjectFiles = useCallback(
    (query: string) => {
      if (!selectedProjectId) {
        throw new Error("This conversation is not bound to a project.");
      }
      return store.searchTrustedFiles(selectedProjectId, query);
    },
    [selectedProjectId],
  );
  const readProjectFile = useCallback(
    (entryId: string) => {
      if (!selectedProjectId) {
        throw new Error("This conversation is not bound to a project.");
      }
      return store.readTrustedFile(selectedProjectId, entryId);
    },
    [selectedProjectId],
  );

  const getProjectFileTree = useCallback(
    (projectId: string, path?: string) => store.getProjectFileTree(projectId, path),
    [],
  );
  const readProjectFileContent = useCallback(
    (projectId: string, path: string, startLine?: number, endLine?: number) =>
      store.readProjectFile(projectId, path, startLine, endLine),
    [],
  );
  const searchProjectFilesystem = useCallback(
    (projectId: string, query: string) => store.searchProjectFiles(projectId, query),
    [],
  );
  const writeProjectFile = useCallback(
    (projectId: string, request: Parameters<OctetStore["writeProjectFile"]>[1]) =>
      store.writeProjectFile(projectId, request),
    [],
  );
  const getCommandDiscovery = useCallback(
    () => store.getCommandDiscovery(),
    [],
  );
  const invokeSlashCommand = useCallback(
    (invocation: string, idempotencyKey: string) =>
      store.invokeSlashCommand(invocation, idempotencyKey),
    [],
  );
  const invokeExtensionAction = useCallback(
    (
      extension: string,
      extensionInstanceId: string,
      generation: number,
      revision: number,
      action: string,
      confirmed: boolean,
    ) =>
      store.invokeExtensionAction(
        extension,
        extensionInstanceId,
        generation,
        revision,
        action,
        confirmed,
      ),
    [],
  );
  const exportSession = useCallback(() => {
    if (!session) throw new Error("No task is selected.");
    const link = document.createElement("a");
    link.href = `/api/v1/sessions/${encodeURIComponent(session.sessionId)}/export`;
    link.download = "";
    link.hidden = true;
    document.body.append(link);
    link.click();
    link.remove();
  }, [session]);
  const forkCurrentSession = useCallback(() => {
    const entryId = session?.branches.head;
    if (!entryId) {
      return Promise.reject(
        new Error("This conversation does not have a checkpoint to fork."),
      );
    }
    return store.forkConversation(entryId);
  }, [session?.branches.head]);
  const openRuntimeStatus = useCallback(() => {
    closeTerminal();
    setInspector(null);
    setActivityOpen(true);
  }, [closeTerminal]);

  const editUserTurn = useCallback(
    (entryId: string, text: string) =>
      store.editUserTurn(entryId, text),
    [],
  );
  const retryResponse = useCallback(
    (
      entryId: string,
      model?: { id: string; reasoning: ReasoningEffort },
    ) => store.retryResponse(entryId, model),
    [],
  );
  const forkConversation = useCallback(
    (entryId: string) => store.forkConversation(entryId),
    [],
  );
  const attachmentContentUrl = useCallback(
    (handle: string) => store.attachmentContentUrl(handle),
    [],
  );
  const resourceContentUrl = useCallback(
    (sessionId: string, handle: string) =>
      store.resourceContentUrl(sessionId, handle),
    [],
  );
  const renameProject = useCallback(
    (projectId: string, name: string) =>
      store.renameProject(projectId, name),
    [],
  );
  const setDefaultProject = useCallback(
    (projectId: string | null) => store.setDefaultProject(projectId),
    [],
  );
  const setProjectTrust = useCallback(
    (projectId: string, trusted: boolean) =>
      store.setProjectTrust(projectId, trusted),
    [],
  );
  const archiveProject = useCallback(
    (projectId: string) => store.archiveProject(projectId),
    [],
  );
  const loadProjectContext = useCallback(
    (projectId: string) => store.getRepositoryContext(projectId),
    [],
  );
  const loadUsageStats = useCallback(
    (period: UsagePeriod) => store.getUsageStats(period),
    [],
  );
  const loadUsageLifetime = useCallback(() => store.getUsageLifetime(), []);
  const loadUsageActivity = useCallback(() => store.getUsageActivity(), []);

  if (state.connecting) return <LoadingState />;
  if (state.error) {
    return (
      <ErrorState
        message={state.error}
        onRetry={() => void store.initialize()}
      />
    );
  }
  if (!state.bootstrap) {
    if (state.projectCatalog) {
      return (
        <ProjectsView
          catalog={state.projectCatalog}
          onboarding
          onRename={renameProject}
          onSetDefault={setDefaultProject}
          onSetTrust={setProjectTrust}
          onArchive={archiveProject}
          onLoadContext={loadProjectContext}
        />
      );
    }
    return <ErrorState message="No task was selected." />;
  }
  if (!session && surface === "session") return <LoadingState />;

  return (
    <div className={appClass} style={appStyle}>
      <Sidebar
        open={sidebarOpen}
        blocked={modalWorkspaceOpen}
        sessions={state.bootstrap.sessions}
        projects={state.bootstrap.projects}
        selectedSessionId={state.selectedSessionId}
        surface={surface}
        devicesAvailable={state.bootstrap.capabilities.connectedDevices}
        filesAvailable={state.bootstrap.capabilities.projectFileBrowser}
        onRestoreFocus={restoreSidebarFocus}
        onClose={closeSidebar}
        onNewSession={startNewSession}
        onOpenFleet={openFleet}
        onSelectSession={selectSession}
        onRestoreSession={(sessionId) => {
          void restoreSession(sessionId);
        }}
        onSetSessionLifecycle={setSessionLifecycle}
        onOpenProjects={openProjects}
        onOpenFiles={openFiles}
        onOpenUsage={openUsage}
        onOpenSettings={openSettings}
        onOpenDevices={openDevices}
        transcriptSearchAvailable={
          state.bootstrap.capabilities.transcriptSearch
        }
        onSearchTranscripts={searchTranscripts}
        onActivateSearchResult={activateSearchResult}
      />

      {surface === "session" && session ? (
        <div
          className="session-column"
          inert={
            (mobileLayout && sidebarOpen) || modalWorkspaceOpen
          }
        >
          <SessionHeader
            sidebarOpen={sidebarOpen}
            sessionId={session.sessionId}
            sessionTitle={session.title}
            projectName={project?.name ?? "Local project"}
            status={session.status}
            goal={state.goal}
            activityAvailable={activityAvailable}
            activityOpen={visibleActivityOpen}
            terminalAvailable={terminalAvailable}
            terminalOpen={visibleTerminalOpen}
            pinned={selectedSummary?.pinned ?? false}
            archived={selectedSummary?.archived ?? false}
            sessionActionsAvailable={
              (!delegatedSessionReadOnly &&
                (state.bootstrap.capabilities.sessionMetadata ||
                  state.bootstrap.capabilities.sessionBranches)) ||
              state.bootstrap.capabilities.sessionExport
            }
            metadataActionsAvailable={
              !delegatedSessionReadOnly &&
              state.bootstrap.capabilities.sessionMetadata
            }
            branchHistoryAvailable={
              !delegatedSessionReadOnly &&
              state.bootstrap.capabilities.sessionBranches &&
              session.branches.entries.some((entry) => entry.checkoutable)
            }
            sessionExportAvailable={
              state.bootstrap.capabilities.sessionExport
            }
            activityButtonRef={activityButtonRef}
            sidebarButtonRef={sidebarButtonRef}
            terminalButtonRef={terminalButtonRef}
            onOpenSidebar={() => setSidebarOpen(true)}
            onToggleActivity={() => {
              closeTerminal();
              setInspector(null);
              setActivityOpen((open) => !open);
            }}
            onToggleTerminal={toggleTerminal}
            dockSplitAvailable={
              activityAvailable && state.bootstrap.capabilities.previews
            }
            dockSplitOn={dockLayout.split}
            dockOrder={dockLayout.order}
            onToggleDockSplit={toggleDockSplit}
            onMoveDockPane={reorderDockPane}
            onRename={(title) => void store.rename(title)}
            onPin={(pinned) => {
              void store.pin(pinned);
            }}
            onArchive={(archived) => {
              void (async () => {
                if (await store.archive(archived) && archived) {
                  await store.createSession();
                }
              })();
            }}
            onOpenBranchHistory={() => setBranchHistoryOpen(true)}
          />
          <FixtureModeLabel />
          <ConnectionBanner connection={state.connection} />
          {selectionErrorBanner}
          <Conversation
            key={session.sessionId}
            session={session}
            bootstrap={state.bootstrap}
            readOnly={delegatedSessionReadOnly}
            onReturnToParent={
              delegatedReturnParentSessionId
                ? () => selectSession(delegatedReturnParentSessionId)
                : undefined
            }
            goal={state.goal}
            onGoalCommand={goalCommand}
            onSubmit={submitSession}
            onInterrupt={interruptSession}
            onConfigure={configureSession}
            onResolveApproval={resolveApproval}
            onResolveUserInput={resolveUserInput}
            onOpenOutput={openOutput}
            onOpenSource={openSource}
            onOpenResource={
              state.bootstrap.capabilities.resources
                ? openResource
                : undefined
            }
            resourceContentUrl={resourceContentUrl}
            onIngestAttachment={ingestAttachment}
            onIngestDocument={ingestDocument}
            onListProjectFiles={listProjectFiles}
            onSearchProjectFiles={searchProjectFiles}
            onReadProjectFile={readProjectFile}
            onGetCommandDiscovery={getCommandDiscovery}
            onInvokeSlashCommand={invokeSlashCommand}
            onExportSession={exportSession}
            onForkSession={forkCurrentSession}
            onOpenRuntimeStatus={openRuntimeStatus}
            onEditUserTurn={editUserTurn}
            onRetryResponse={retryResponse}
            onForkConversation={forkConversation}
            attachmentContentUrl={attachmentContentUrl}
          />
        </div>
      ) : (
        <div
          className="utility-column"
          inert={
            (mobileLayout && sidebarOpen) || modalWorkspaceOpen
          }
        >
          <UtilityTopbar
            title={
              surface === "fleet"
                ? "Command center"
                : surface === "settings"
                  ? "Settings"
                  : surface === "projects"
                    ? "Projects"
                    : surface === "files"
                      ? "Files"
                      : surface === "usage"
                        ? "Usage"
                        : "Connected devices"
            }
            sidebarOpen={sidebarOpen}
            onOpenSidebar={() => setSidebarOpen(true)}
            sidebarButtonRef={sidebarButtonRef}
          />
          <ConnectionBanner connection={state.connection} />
          {selectionErrorBanner}
          <FixtureModeLabel />
          {surface === "fleet" ? (
            <FleetOverview
              sessions={state.bootstrap.sessions}
              projects={state.bootstrap.projects}
              selectedSessionId={state.selectedSessionId}
              onNewTask={startNewSession}
              onSelectTask={selectSession}
            />
          ) : surface === "files" ? (
            <Suspense
              fallback={
                <main className="files-panel files-empty" aria-busy="true">
                  <Folder aria-hidden="true" />
                  <p role="status">Loading project files…</p>
                </main>
              }
            >
              <FilesPanel
                projects={state.bootstrap.projects}
                preferredProjectId={session?.projectId}
                writeAvailable={state.bootstrap.capabilities.projectFileWrite}
                getTree={getProjectFileTree}
                readFile={readProjectFileContent}
                searchFiles={searchProjectFilesystem}
                writeFile={writeProjectFile}
              />
            </Suspense>
          ) : surface === "settings" ? (
            <SettingsView
              notificationsSupported={notificationState.supported}
              notificationsEnabled={notificationState.enabled}
              notificationPermission={notificationState.permission}
              onNotificationsChange={changeNotifications}
            />
          ) : surface === "usage" ? (
            <UsagePage
              loadStats={loadUsageStats}
              loadLifetime={loadUsageLifetime}
              loadActivity={loadUsageActivity}
            />
          ) : surface === "projects" && state.projectCatalog ? (
            <ProjectsView
              catalog={state.projectCatalog}
              onRename={renameProject}
              onSetDefault={setDefaultProject}
              onSetTrust={setProjectTrust}
              onArchive={archiveProject}
              onLoadContext={loadProjectContext}
            />
          ) : (
            <DevicesView
              hostName={state.bootstrap.host.name}
              devices={state.bootstrap.devices}
              lanAvailable={
                state.bootstrap.capabilities.connectedDevices &&
                state.bootstrap.capabilities.lanClients &&
                state.bootstrap.capabilities.pairDevices
              }
            />
          )}
        </div>
      )}

      {terminalSplitLayout && visibleTerminalOpen ? (
        <div
          className="pane-resize-handle terminal-pane-resize-handle"
          role="separator"
          aria-label="Resize terminal"
          aria-orientation="vertical"
          aria-valuemin={300}
          aria-valuemax={paneBounds("terminal").max}
          aria-valuenow={terminalPaneWidth}
          tabIndex={0}
          onPointerDown={(event) => {
            event.preventDefault();
            beginPaneResize("terminal", event.clientX);
          }}
          onKeyDown={(event) => {
            if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") {
              return;
            }
            event.preventDefault();
            resizePaneBy(
              "terminal",
              event.key === "ArrowLeft" ? 16 : -16,
            );
          }}
        />
      ) : null}
      {visibleTerminalOpen ? (
        <TerminalPanel
          hostId={state.bootstrap.host.id}
          onClose={closeTerminal}
        />
      ) : null}

      {surface === "session" && session ? (
        <>
          {dockResizeTargets.map((target) => (
            <div
              key={target.slot}
              data-dock-slot={target.slot}
              className={
                target.slot === "b"
                  ? "pane-resize-handle dock-split-resize-handle"
                  : "pane-resize-handle"
              }
              role="separator"
              aria-label={
                target.pane === "inspector"
                  ? target.slot === "b"
                    ? "Resize inspector (second pane)"
                    : "Resize inspector"
                  : target.slot === "b"
                    ? "Resize task activity (second pane)"
                    : "Resize task activity"
              }
              aria-orientation="vertical"
              aria-valuemin={target.pane === "inspector" ? 520 : 280}
              aria-valuemax={paneBounds(target.pane).max}
              aria-valuenow={
                target.pane === "inspector" ? inspectorPaneWidth : activityPaneWidth
              }
              tabIndex={0}
              onPointerDown={(event) => {
                event.preventDefault();
                beginPaneResize(target.pane, event.clientX);
              }}
              onKeyDown={(event) => {
                if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") {
                  return;
                }
                event.preventDefault();
                resizePaneBy(
                  target.pane,
                  event.key === "ArrowLeft" ? 16 : -16,
                );
              }}
            />
          ))}
          <ActivityRail
            session={session}
            dockSlot={
              dockSplitActive
                ? (dockSlotFor(dockLayout, "activity") ?? undefined)
                : undefined
            }
            open={
              visibleActivityOpen &&
              (dockSplitActive || !inspector) &&
              !(mobileLayout && sidebarOpen)
            }
            onClose={closeActivity}
            onOpenOutput={openOutput}
            onOpenSource={openSource}
            onOpenSession={selectSession}
            onInvokeExtensionAction={invokeExtensionAction}
            onOpenResource={
              state.bootstrap.capabilities.resources
                ? openResource
                : undefined
            }
            modal={!wideLayout}
            onRestoreFocus={restoreActivityFocus}
            resourcesAvailable={state.bootstrap.capabilities.resources}
          />
          <MemoizedInspector
            session={session}
            selection={inspector}
            closing={inspectorClosing}
            modal={!wideLayout}
            dockSlot={
              dockSplitActive
                ? (dockSlotFor(dockLayout, "inspector") ?? undefined)
                : undefined
            }
            previewsAvailable={state.bootstrap.capabilities.previews}
            resourceContentUrl={resourceContentUrl}
            onRestoreFocus={restoreActivityFocus}
            onClose={closeInspector}
          />
        </>
      ) : null}
      {surface === "session" && session && branchHistoryOpen ? (
        <BranchHistorySheet
          session={session}
          onClose={closeBranchHistory}
          onCheckout={(entryId) => store.checkoutBranch(entryId)}
        />
      ) : null}
    </div>
  );
}
