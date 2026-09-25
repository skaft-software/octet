# Browse focus and native-connector qualification

Candidate: base `df5a7e80` plus the Browse working diff. This is an acceptance
recipe, **not a physical-focus pass**. See [usage](README.md),
[reference](REFERENCE.md), and [connector boundary](CONNECTORS.md).

## #377 — implemented portion and remaining gates

The candidate replaces isolated `context.new_page()` calls with the fixed
internal CDP request `Target.createTarget(url="about:blank", background=true)`.
It matches the returned target ID to the new Playwright page, including when an
unrelated popup arrives first. It does not navigate the first arbitrary page
or fall back to foreground creation. Cancelled creation closes only the exact
created target; an unknown creation outcome closes the owned isolated context.
The 32-tab limit is checked before creating a window/tab.

This addresses a specific activation source during tool use, not every way the
OS or a page can activate a window. Initial explicit launch still uses visible
persistent Chromium and can activate its window. Page-initiated popups and
`window.focus()` are not suppressed by this change. No headless mode, offscreen
position, minimization, normal profile, or foreground-restoration loop is used.
**Do not close #377 solely on fixture results or the post-launch test below.**

Source evidence (read-only inspection, not browser execution):

- Playwright 1.57.0 `packages/playwright-core/src/server/chromium/crBrowser.ts`,
  `doCreateNewPage`, omits the `background` option:
  <https://github.com/microsoft/playwright/blob/v1.57.0/packages/playwright-core/src/server/chromium/crBrowser.ts>.
  The installed 1.57.0 driver has the same call at `lib/server/chromium/crBrowser.js:317`.
- Its pinned Chromium is revision 1200 / version 143.0.7499.4. In that Chromium,
  `GetNavigationParams` uses `NEW_BACKGROUND_TAB` or `SHOW_WINDOW_INACTIVE` when
  requested, and `CreateTarget` only calls `Focus()` when not background:
  <https://github.com/chromium/chromium/blob/143.0.7499.4/chrome/browser/devtools/protocol/target_handler.cc#L31-L54>
  and [the focus call](https://github.com/chromium/chromium/blob/143.0.7499.4/chrome/browser/devtools/protocol/target_handler.cc#L239-L241).
- The installed 1.57.0 `browserTypeDispatcher.js:42-48` explicitly parents the
  persistent `BrowserContextDispatcher` under `BrowserDispatcher` and returns
  both. Python `_browser_context.py:106-114` retains that Browser parent;
  `_browser_type.py:161-184` returns the context. `context.browser` is available
  on this pinned persistent path, unlike some older versions. Missing browser
  handles still fail closed without an activating fallback in the fixtures.
- A `--no-activate` flag was not found in the installed Chromium binary. Do not
  claim a guessed switch works. `--no-startup-window` suppresses the visible
  startup window and prevents Playwright's default persistent-context page wait
  from completing. Disabling all Playwright default arguments merely to evade
  that wait is not this candidate's solution.

### Dependency-free regressions

```console
cd extensions/octet-browse
env -u OCTET_BROWSE_PLAYWRIGHT_TESTS -u OCTET_BROWSE_FOCUS_TESTS \
  python3 -m unittest discover -s tests -t . -v
```

`tests/test_background_tabs.py` checks request parameters, exact target matching,
arrival races, timeout/cancellation cleanup, pre-creation ownership/URL/tab-limit
checks, persistent visible launch arguments, and no activating fallback.
`tests/test_adapters.py` checks native refusals, complete identities and ownership.
These tests deliberately do not pretend to observe a physical terminal.

### Opt-in live tests — user authorization required

The [reference](REFERENCE.md#development-and-tests) describes the existing
loopback-only integration suite. It now exercises non-activating tab creation,
not just startup and page-created popups. No network setup/download is automatic.

On macOS, a separate **interactive** test is available after the user approves
visible isolated browser automation. Manually identify the terminal application's
PID (Terminal.app or Ghostty, not the shell PID) in Activity Monitor. Substitute
that exact integer for `12345`; the test never discovers browser targets:

```console
cd extensions/octet-browse
OCTET_BROWSE_PLAYWRIGHT_TESTS=1 OCTET_BROWSE_FOCUS_TESTS=1 \
  OCTET_BROWSE_FOCUS_TERMINAL_PID=12345 PYTHONPATH=vendor:. \
  python3 -m unittest \
  tests.test_playwright_integration.PlaywrightIntegrationTests.test_post_launch_operations_retain_explicit_terminal_focus -v
```

The test launches a temporary isolated profile, then asks the operator to leave
Chromium visible and refocus the terminal. It reads only the foreground process
ID using AppKit via `osascript`, before and after repeated open/new-tab,
snapshot, wait, screenshot, navigation and close-tab operations. It asks for an
explicit final no-flicker observation; a wrong PID, unavailable terminal/probe,
lost foreground, or missing/noisy observation fails. It does not activate any
application, list browser windows or tabs, or request Accessibility control.

The test does **not** run octet's TUI, qualify initial startup focus, measure every
transient OS focus change, or exercise popup/manual-auth journeys. Before release:

1. Record exact base plus diff/build hash, Playwright/Chromium version, macOS
   version, terminal version, monitor/Spaces arrangement and operator consent.
2. Run the normal integration test and the interactive focus test; retain exact
   commands, stdout/stderr and the operator's flicker observation.
3. In the actual octet TUI, keep the browser visible on-screen; exercise initial
   explicit open, repeated open, new/existing-tab navigation, semantic
   click/type/press/scroll, snapshot, screenshot, popup, cancellation, external
   browser close/crash, status, and explicit reopen. Use only loopback fixtures
   and dummy non-secret fields. Record which application retained physical focus
   and any transient flicker/movement; do not infer it from browser DOM focus.
4. Verify manual authentication remains possible when the user intentionally
   selects the visible browser, then manually returns to the TUI. Never automate
   credentials. A focus fix must not fight that intentional switch.
5. Repeat on each supported desktop/window-manager combination (including Linux)
   and record failures separately. Initial-launch/popup focus is still a known
   gap and requires a supported, qualified non-activating implementation before
   claiming the full issue closed.

No physical or live-browser steps above were run while authoring this candidate.
