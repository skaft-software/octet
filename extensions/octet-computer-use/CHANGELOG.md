# Changelog

## Unreleased

### Added

- App icon for the macOS host, built from the designer's artwork in
  `host-app/Resources/AppIcon.png` by `host-app/Tools/make-iconset.swift`. The
  plate's extent is read from the source's alpha channel rather than guessed
  from colour, so the transparent margin stays genuinely transparent and no
  background halo is baked in; every size macOS requests is rendered from the
  source so small sizes stay sharp. A from-scratch vector approximation was
  tried and removed: redrawing the design lost the exact silhouette, bevel, and
  gradient.
- Menu bar indicator on the macOS host, shown whenever computer use is
  available and tinted orange while an agent session is live. Its state is
  polled from `cua-driver sessions --json` so it reflects the driver's own
  state rather than anything the agent claims. The menu also offers
  **Stop computer use**, which runs `cua-driver revoke` to cut every live
  session in one click; revoke is deny-only, so the control cannot be used to
  widen access. An agent that can act on the desktop but leaves no trace that it
  is acting is the failure this prevents.
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
  Ask the host's own daemon for its grant state instead of `cua-driver
  permissions status`. That CLI answers only for a daemon whose identity it
  recognises and reports `unknown` for any other bundle, including octet's own
  fully-granted host, so the cursor path was being silently skipped in the
  running extension while looking correct under test.
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
