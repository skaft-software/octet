# Computer-use composition contract

Status: **Cua Driver–backed, packaged, tested.** The bundle provisions and drives
a locally installed MIT-licensed [Cua Driver](https://github.com/trycua/cua) over
local stdio MCP on macOS, Windows, and Linux. It replaces the earlier inert API
`0.3` prototype (mocked backends, no live dispatch) that this package used to
contain. See [README.md](README.md) for install, permissions, the published tool
table, and test commands.

## Boundary and authority

- The driver is third-party software installed from the standard package index
  into `~/.octet/computer-use`. It is not vendored, not forked, and contains no
  part of OpenAI's CUA runtime. This bundle never downloads a driver outside the
  user-triggered `computer_use_setup` / `/computer-use` provisioning step, and
  never runs a piped remote install script.
- The bundle **never grants an operating-system permission**. macOS
  Accessibility/Screen Recording, a Windows interactive session, and Linux
  AT-SPI in a live display session are the user's to grant. `computer_use_status`
  reports status without prompting.
- `confirmations = true` is declared in the manifest. Every driver action not
  annotated `readOnlyHint: true` requires an explicit user confirmation before
  dispatch. A declined, cancelled, unavailable, or failed confirmation denies
  the call and returns an error without invoking the driver. An unknown (never
  described) tool is treated as effectful.
- The driver child process receives only a reviewed, non-secret set of desktop
  session variables. Provider tokens and arbitrary ambient environment are not
  forwarded. The manifest `[capabilities] environment` list is the host-level
  gate; the runtime's `SESSION_ENVIRONMENT` is the bundle-level gate; both must
  agree before a name reaches the child.
- Credential, payment, and one-time-code entry stays manual. The bundle never
  types secrets and never echoes a typed value.

## Protocol and lifetime

- One `DriverClient` per host owner, started lazily on first use and started with
  `mcp --direct` so the driver owns its runtime inside the child rather than
  auto-launching a separate app or daemon. A single long-lived reader owns the
  driver's stdout for the process lifetime.
- The republished octet tool set is a small reviewed subset of the driver's
  catalog, so octet's published catalog does not churn with upstream releases.
  Only allowlisted, type-checked, bounded arguments are forwarded to the driver;
  an unrecognised argument is dropped.
- Tool results are bounded: text is truncated to a fixed limit and image blocks
  are reported as a count rather than inlined as base64.
- Session-scoped work is expected to use `computer_use_start_session` /
  `computer_use_end_session` so the driver's per-session cursor, recording, and
  cleanup state is released. The client is closed on extension shutdown.

## Platform notes

- **macOS** — the driver reaches the desktop through a helper app identity
  (`com.trycua.driver`). With `mcp --direct` the runtime runs in-process and
  relies on the user's Accessibility/Screen Recording grants to that helper.
  Without them, observation and action fail closed.
- **Windows** — the driver runs as the interactive user; a locked or
  headless session is not drivable.
- **Linux** — needs a live display session and AT-SPI 2. X11/XWayland is more
  widely supported than native Wayland; Wayland input is compositor-dependent.

## Not qualified here

- This bundle does not implement a *browser* surface. For page-level work,
  including anything authenticated, prefer the still-supported
  [octet-browse](../octet-browse/README.md) (now deprecated for new automation
  but retained for its isolated, manual-auth profile).
- Physical-teardown detection, hard process-loss cleanup, and cross-platform
  installed-package qualification remain with the driver project.
