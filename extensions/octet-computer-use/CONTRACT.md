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
- Effectful actions follow Octet's confirmation policy. Gated profiles and
  `OCTET_CUA_CONFIRM=1` require approval before dispatch; declined, unavailable,
  or failed confirmation denies the call. Full-access mode does not add a
  per-action prompt by default. Unknown driver tools remain effectful.
- The driver child process receives only a reviewed, non-secret set of desktop
  session variables. Provider tokens and arbitrary ambient environment are not
  forwarded. The manifest `[capabilities] environment` list is the host-level
  gate; the runtime's `SESSION_ENVIRONMENT` is the bundle-level gate; both must
  agree before a name reaches the child. On Linux only, the child also receives
  `LINUX_LAUNCH_ENVIRONMENT` (`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`,
  `TMPDIR`, and locale), the host-sanitized baseline every octet tool
  subprocess already has, because the Linux driver launches apps with its own
  environment. In a Wayland session the child also gets
  `CUA_DRIVER_RS_ENABLE_WAYLAND=1`, the driver's native Wayland opt-in.
- Credential, payment, and one-time-code entry stays manual. The bundle never
  types secrets and never echoes a typed value.

## Protocol and lifetime

- One `DriverClient` per host owner, started lazily on first use. The default
  macOS path connects to the signed Cua Driver app's daemon; direct MCP mode is
  used only on non-macOS or after an explicit macOS opt-out. A single long-lived
  reader owns the driver's stdout for the process lifetime.
- The republished octet tool set is a small reviewed subset of the driver's
  catalog, so octet's published catalog does not churn with upstream releases.
  Only allowlisted, type-checked, bounded arguments are forwarded; an
  unrecognised argument is dropped.
- Text, image delivery, and structured driver output are bounded. Oversized
  structured snapshots preserve targeting fields such as snapshot/window IDs,
  element tokens, and coordinates within Octet's 256 KiB host limit.
- Cursor setup and eligible driver actions share one action session. On the
  desktop host, startup configures motion, enables the overlay, and verifies the
  resulting enabled state before reporting readiness. Failures surface and block
  actions rather than silently proceeding cursorless.
- Public start/end operations switch or clear that same session; subsequent
  actions attach to the active session, and extension shutdown ends it.

## Platform notes

- **macOS** — by default, the signed `/Applications/CuaDriver.app` is the
  desktop host and owns the daemon's permission identity and cursor overlay. If
  the host is missing or cannot prove its permissions, runtime status is
  `unavailable`; octet does not silently fall back to a cursorless direct
  runtime. `OCTET_CUA_DESKTOP_HOST=0` explicitly opts into direct mode. An
  alternate app is only selected through the explicit developer override.
- **Windows** — the driver runs as the interactive user; a locked or
  headless session is not drivable.
- **Linux** — needs a live display session; there is no grant. Readiness is
  read from the driver's `check_permissions` session report: X11 or a Wayland
  session with the native backend enabled counts as ready, and only a missing
  display holds effectful actions. AT-SPI 2 is reported but not required,
  because the driver can act by pixel. Wayland input is compositor-dependent
  (wlroots virtual pointer on Hyprland/Sway, portal/libei elsewhere). The
  Linux runtime is always direct, with no cursor overlay, so cursor themes are
  not installed.

## Not qualified here

- This bundle does not implement a *browser* surface. For page-level work,
  including anything authenticated, prefer the still-supported
  [octet-browse](../octet-browse/README.md) (now deprecated for new automation
  but retained for its isolated, manual-auth profile).
- Physical-teardown detection, hard process-loss cleanup, and cross-platform
  installed-package qualification remain with the driver project.
