/// <reference types="vite/client" />

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fixtureBootstrap, fixtureSessions } from "./fixtures";

vi.mock("@xterm/xterm", () => ({ Terminal: class {} }));

import { SessionHeader, SessionSelectionErrorBanner } from "./App";
import { resolveDelegatedParentSessionId } from "./delegated-session";

function renderHeader(
  sessionExportAvailable: boolean,
  terminalAvailable = false,
  terminalOpen = false,
  sessionTitle = "Safe session",
) {
  const onToggleTerminal = vi.fn();
  const onRename = vi.fn();
  return {
    onRename,
    onToggleTerminal,
    ...render(
      <SessionHeader
        sidebarOpen
        sessionId="session-safe"
        sessionTitle={sessionTitle}
        projectName="Local project"
        status="idle"
        activityAvailable={false}
        activityOpen={false}
        terminalAvailable={terminalAvailable}
        terminalOpen={terminalOpen}
        pinned={false}
        archived={false}
        sessionActionsAvailable
        metadataActionsAvailable
        branchHistoryAvailable={false}
        sessionExportAvailable={sessionExportAvailable}
        activityButtonRef={createRef<HTMLButtonElement>()}
        sidebarButtonRef={createRef<HTMLButtonElement>()}
        onOpenSidebar={vi.fn()}
        onToggleActivity={vi.fn()}
        onToggleTerminal={onToggleTerminal}
        onRename={onRename}
        onPin={vi.fn()}
        onArchive={vi.fn()}
        onOpenBranchHistory={vi.fn()}
      />,
    ),
  };
}

describe("session header safe export", () => {
  afterEach(cleanup);

  it("shows the terminal control only when the host advertises it", async () => {
    const user = userEvent.setup();
    const unavailable = renderHeader(false);
    expect(
      screen.queryByRole("button", { name: "Open terminal" }),
    ).toBeNull();
    unavailable.unmount();

    const available = renderHeader(false, true, false);
    const terminal = screen.getByRole("button", { name: "Open terminal" });
    expect(terminal).toHaveAttribute("aria-pressed", "false");
    await user.click(terminal);
    expect(available.onToggleTerminal).toHaveBeenCalledOnce();
  });

  it("keeps a provisional session title internal until the user renames the task", async () => {
    const user = userEvent.setup();
    const header = renderHeader(false, false, false, "New session");

    expect(screen.getByText("New task", { exact: true })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Task actions" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    expect(screen.getByRole("textbox", { name: "Task title" })).toHaveValue(
      "New task",
    );
    await user.keyboard("{Enter}");
    expect(header.onRename).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Task actions" }));
    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    const title = screen.getByRole("textbox", { name: "Task title" });
    await user.clear(title);
    await user.type(title, "Release prep");
    await user.keyboard("{Enter}");
    expect(header.onRename).toHaveBeenCalledWith("Release prep");
  });

  it("exposes a direct same-origin download only when the host advertises it", async () => {
    expect(fixtureBootstrap.capabilities.sessionExport).toBe(false);
    const user = userEvent.setup();
    const unavailable = renderHeader(false);
    await user.click(screen.getByRole("button", { name: "Task actions" }));
    expect(
      screen.queryByRole("menuitem", { name: "Download safe export" }),
    ).toBeNull();
    unavailable.unmount();

    renderHeader(true);
    await user.click(screen.getByRole("button", { name: "Task actions" }));
    const download = screen.getByRole("menuitem", {
      name: "Download safe export",
    });
    expect(download).toHaveAttribute(
      "href",
      "/api/v1/sessions/session-safe/export",
    );
    expect(download).toHaveAttribute("download");
    expect(download.tagName).toBe("A");
  });
});

describe("workspace dock layout controls", () => {
  afterEach(cleanup);

  function renderDockHeader(options: {
    dockSplitAvailable: boolean;
    dockSplitOn: boolean;
    dockOrder?: readonly ("activity" | "inspector")[];
  }) {
    const onToggleDockSplit = vi.fn();
    const onMoveDockPane = vi.fn();
    render(
      <SessionHeader
        sidebarOpen
        sessionId="session-safe"
        sessionTitle="Dock session"
        projectName="Local project"
        status="idle"
        activityAvailable
        activityOpen
        terminalAvailable={false}
        terminalOpen={false}
        pinned={false}
        archived={false}
        sessionActionsAvailable={false}
        metadataActionsAvailable={false}
        branchHistoryAvailable={false}
        sessionExportAvailable={false}
        activityButtonRef={createRef<HTMLButtonElement>()}
        sidebarButtonRef={createRef<HTMLButtonElement>()}
        onOpenSidebar={vi.fn()}
        onToggleActivity={vi.fn()}
        onToggleTerminal={vi.fn()}
        dockSplitAvailable={options.dockSplitAvailable}
        dockSplitOn={options.dockSplitOn}
        dockOrder={options.dockOrder ?? ["activity", "inspector"]}
        onToggleDockSplit={onToggleDockSplit}
        onMoveDockPane={onMoveDockPane}
        onRename={vi.fn()}
        onPin={vi.fn()}
        onArchive={vi.fn()}
        onOpenBranchHistory={vi.fn()}
      />,
    );
    return { onToggleDockSplit, onMoveDockPane };
  }

  it("creates and merges the split dock, and never offers it without a capable layout", async () => {
    const user = userEvent.setup();
    const inert = renderDockHeader({
      dockSplitAvailable: false,
      dockSplitOn: false,
    });
    expect(
      screen.queryByRole("button", { name: "Split dock into two panes" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Move the first dock pane right" }),
    ).toBeNull();
    expect(inert.onToggleDockSplit).not.toHaveBeenCalled();
    cleanup();

    const split = renderDockHeader({
      dockSplitAvailable: true,
      dockSplitOn: false,
    });
    const toggle = screen.getByRole("button", {
      name: "Split dock into two panes",
    });
    expect(toggle).toHaveAttribute("aria-pressed", "false");
    await user.click(toggle);
    expect(split.onToggleDockSplit).toHaveBeenCalledOnce();
  });

  it("rearranges the persisted pane order one step at a time", async () => {
    const user = userEvent.setup();
    const merged = renderDockHeader({
      dockSplitAvailable: true,
      dockSplitOn: false,
    });
    expect(
      screen.getByRole("button", { name: "Move the first dock pane right" }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Move the second dock pane left" }),
    ).toBeVisible();
    await user.click(
      screen.getByRole("button", { name: "Move the first dock pane right" }),
    );
    expect(merged.onMoveDockPane).toHaveBeenLastCalledWith("activity", 1);
    await user.click(
      screen.getByRole("button", { name: "Move the second dock pane left" }),
    );
    expect(merged.onMoveDockPane).toHaveBeenLastCalledWith("inspector", -1);
    cleanup();

    const swapped = renderDockHeader({
      dockSplitAvailable: true,
      dockSplitOn: true,
      dockOrder: ["inspector", "activity"],
    });
    expect(
      screen.getByRole("button", { name: "Merge dock panes" }),
    ).toHaveAttribute("aria-pressed", "true");
    await user.click(
      screen.getByRole("button", { name: "Move the first dock pane right" }),
    );
    expect(swapped.onMoveDockPane).toHaveBeenLastCalledWith("inspector", 1);
  });
});

describe("delegated parent navigation", () => {
  it("prefers authoritative snapshot metadata over stale inferred history", () => {
    const delegated = {
      ...fixtureSessions["session-fresh"],
      sessionId: `agent-session:${"a".repeat(64)}`,
      delegatedParentSessionId: "parent-two",
    };

    expect(resolveDelegatedParentSessionId(delegated, "parent-one")).toBe(
      "parent-two",
    );
    expect(
      resolveDelegatedParentSessionId(
        { ...delegated, delegatedParentSessionId: undefined },
        "parent-one",
      ),
    ).toBe("parent-one");
    expect(
      resolveDelegatedParentSessionId(
        { ...delegated, sessionId: "ordinary-session" },
        "parent-one",
      ),
    ).toBeNull();
  });
});

describe("session selection errors", () => {
  afterEach(cleanup);

  it("surfaces the failure and offers a retry", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    render(
      <SessionSelectionErrorBanner
        message="Session failed with 500"
        onRetry={onRetry}
      />,
    );

    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Could not open that session.");
    expect(alert).toHaveTextContent("Session failed with 500");
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledOnce();
  });
});
