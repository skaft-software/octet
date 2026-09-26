# Changelog

## Unreleased

### Added

- Optional macOS host app (`build-host-app.sh`, sources under `host-app/`) that
  supplies the AppKit main thread and Window Server access the agent-cursor
  overlay needs. It starts `cua-driver serve` under its own bundle identifier
  `com.octet.computeruse`, shows no window, and never takes focus. Identifiers
  are distinct from Cua's `com.trycua.driver` so the two apps' TCC grants cannot
  be confused. Verified on macOS 27: the grant survives a full host restart and
  the cursor renders, which the stock `CuaDriver.app` cannot do on that release.
  macOS-only, optional, and not installed by `octet extension install`.
- Start an installed-but-idle desktop host once, in the background, when
  choosing a runtime. If it still cannot prove its grant, octet falls back to the
  direct runtime.

### Changed

- Make the `direct` runtime the shipped default. It runs inside octet's own
  process and inherits the Accessibility and Screen Recording grants of the app
  running octet, so a user needs no separate helper app, no extra bundle, and no
  desktop-host install. `computer_use_status` now names the live runtime and
  points at that app rather than at a helper.
- Treat an installed desktop host as absent unless its own permissions are
  live. On some macOS releases the host's grant never persists, so it re-prompts
  on every launch and every action fails; the direct runtime works immediately
  there. `OCTET_CUA_DESKTOP_HOST=0` forces the direct runtime, `=1` requires the
  host.
- Resolve the installed macOS bundle's executable from its own
  `CFBundleExecutable` rather than assuming one layout, so the stock
  `CuaDriver.app` and the source-built `CuaDriverLocal.app` both work. The
  desktop host remains the only path to the visible agent cursor.
  A declared `CFBundleExecutable` that is not a known driver name is no longer
  trusted: octet's own host declares the host binary there and ships the driver
  beside it, and returning the host would hand the client a binary that cannot
  speak MCP.
- Replace the inert API `0.3` mocked prototype with a real, Cua Driver–backed
  API `0.4` bundle. The bundle now provisions a locally installed MIT-licensed
  [Cua Driver](https://github.com/trycua/cua) on request and drives native
  desktop applications on macOS, Windows, and Linux over local stdio MCP.
- Raise the manifest to API `0.4` with `filesystem`/`process`/`network` and
  `confirmations = true`, and declare the reviewed desktop-session environment
  names the driver child needs to reach the interactive session.

### Added

- `computer_use_setup` provisions the driver from the package index into
  `~/.octet/computer-use`, reusing an existing install and never touching a
  global Python environment. `computer_use_status` reports version, self-check,
  and OS permission state without prompting.
- Republish a reviewed subset of the driver's tools: `computer_use_installed_apps`,
  `computer_use_windows`, `computer_use_window_state`, `computer_use_desktop_state`
  (read-only) and `computer_use_click`, `computer_use_type_text`,
  `computer_use_press_key`, `computer_use_scroll`, `computer_use_launch_app`,
  `computer_use_start_session`, `computer_use_end_session` (each requiring an
  explicit user confirmation). Only allowlisted, bounded arguments are forwarded.
- Add a `computer-use` skill documenting the observe-act-verify loop, the
  confirmation boundary, and the manual-permission requirement.
- Add deterministic tests for argument sanitisation, result bounding, the
  fail-closed confirmation gate, manifest/runtime registration consistency, and
  child-environment least privilege, plus live-driver integration tests that
  skip cleanly when no local driver is present.

### Removed

- The API `0.3` prototype entry point and its obsolete handshake test. The
  design record is retained in git history; the shipped surface is now the
  driver-backed bundle.
