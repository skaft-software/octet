# Changelog

## Unreleased (target: v0.8.2)

### Added

- Linux support across X11 and Wayland desktops, including Omarchy (Hyprland),
  Sway and other wlroots compositors, GNOME, and KDE Plasma. Status and the
  action gate read the driver's Linux session report (X11, native Wayland,
  AT-SPI) instead of macOS grants. Wayland sessions enable the driver's native
  backend, and the child receives the desktop-identity and compositor names it
  uses to pick a route (`XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP`,
  `DESKTOP_SESSION`, `KDE_FULL_SESSION`, `HYPRLAND_INSTANCE_SIGNATURE`,
  `SWAYSOCK`, `XDG_STATE_HOME`). Apps launched on Linux inherit the
  host-sanitized `PATH`, `HOME`, and locale.
- The model-adaptive agent cursor on Linux. The direct runtime draws the
  bundled themes through Cua's X11 or layer-shell overlay, best-effort. Themes
  are written straight into the driver's store (the wheel has no theme
  compiler), both at setup and on first cursor use, so they work however the
  driver was provisioned. Verified on Xvfb and headless Sway, including a
  model switch recoloring the cursor.
- GNOME Wayland: bundle Cua's MIT-licensed WinRects GNOME Shell helper from
  the matching driver release, with a `SetThemeColor` pin for the model color.
  `/computer-use setup` installs and enables it; status reports when a login is
  still needed.
- Provision on Debian and Ubuntu without `python3-venv`: when the host Python
  cannot bootstrap pip, install the published Linux wheel directly after
  verifying the package index's SHA-256.
- Pinned upstream Cua Driver `jev-use` recipe as owner-fenced background jobs:
  `computer_use_jev_use_status`, `computer_use_jev_use_setup`,
  `computer_use_jev_use_run`, `computer_use_jev_use_choose`, plus
  `/computer-use jev-use status|setup|run|status JOB_ID|cancel JOB_ID`.
  Setup explicitly installs the commit-pinned source and locked Python/optional
  TypeScript dependencies; runs own a separate MCP session and isolated browser
  against the local form fixture with independent `/state` readback. Mock by
  default, live via `/computer-use jev` key, visual path capability-gated.
  Refused under per-action confirmation policy.
- An attended, opt-in live observe-act-verify smoke (`tests/live_smoke.py`)
  that refuses to run in CI, without a confirmation flag, or without an
  interactive terminal, and a no-driver status/fail-closed test through the
  real entrypoint. See the README's Windows section.

### Fixed

- Negotiate screenshot artifacts and stage large captures under the host scratch
  directory instead of exceeding the inline artifact limit. Report bounded
  publication errors while retaining a usable accessibility tree.
- Show window/app handles in model-visible text and replace references to hidden
  structured details with actionable targeting and query guidance.
- `/computer-use setup` no longer fails on Linux, whose driver ships no
  cursor-theme compiler.
- Status no longer reports null `accessibility`/`screen_recording` fields on
  hosts that do not have those grants.
- Desktop-host tests pin Darwin, so their macOS semantics no longer depend on
  the machine running the suite.
- Stage full-size screenshots on Windows, where `O_NOFOLLOW` does not exist:
  exclusive creation plus a final-path check against the resolved scratch
  root replaces it, and junctions are rejected wherever symlinks were. The
  optional Jev key gets a protected current-user-only ACL on Windows and is
  not stored if that cannot be applied.

## Earlier changes

### Added

- Bundle 24 MIT-attributed Cua dotLottie cursor themes with an Octet-inspired,
  slightly smaller silhouette and the stable model-adaptive TUI prompt colors.
  `/computer-use setup` installs the compiled themes locally; cursor sessions
  select the matching installed theme on the next tool boundary after a model
  switch, verify read-back, and fall back to `cua.default` before theme setup.
- `computer_use_hotkey`, `computer_use_invoke_menu`, and
  `computer_use_move_cursor`. Cursor movement is restricted to an exact window
  target and screenshot-local coordinates; it never moves the real OS pointer.
- Bind cursor initialization and eligible actions to one session, and make
  public session start/end operations switch or clear that action binding.
- Bound candidate/region/history schemas and project oversized structured
  snapshots below the host's 256 KiB limit while retaining targeting handles.
- Keep Jev chooser-only: validate confidence and probabilities, reject low
  confidence real actions, and never execute or verify its suggestion.
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
- Start a selected desktop host in the background when needed. On macOS, if the
  signed Cua host cannot prove its grant, runtime selection fails closed instead
  of silently switching to direct mode.

### Fixed

- `computer_use_setup` honors an explicitly requested driver version. The request
  is validated before any reuse, and an existing install is reused only when it
  is unpinned or the installed driver actually reports that version; a mismatch
  (or an install that cannot report a version) recreates the octet-owned venv
  and installs the pin. An unpinned request still reuses the installed driver
  without network access, and an abbreviated release such as `0.29` matches the
  installed `0.29.0` rather than reinstalling on every call.
- Resolve the provisioned venv's site-packages on Windows (`Lib/site-packages`)
  instead of the POSIX `lib/pythonX.Y/site-packages` path, so the optional
  `typesafe-sdk` no longer reads as uninstalled on Windows.

### Changed

- Use a short, straight 80 ms cursor glide without pronounced turns or spring
  bounce, and delay idle fade to 5 seconds.
- Make the signed `/Applications/CuaDriver.app` the default macOS host because
  it provides the permission identity and visible agent-cursor overlay. A missing
  or ungranted host reports `unavailable`; macOS direct mode is an explicit
  `OCTET_CUA_DESKTOP_HOST=0` opt-out, not an automatic fallback.
- Require the selected macOS host to prove live permissions and report its
  actual runtime/binary. Query the daemon's own permission state rather than the
  CLI's incomplete status probe; do not report cursor readiness until the same
  action session has cursor motion configured, enabled, and verified.
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
  `computer_use_press_key`, `computer_use_hotkey`, `computer_use_invoke_menu`,
  `computer_use_move_cursor`, `computer_use_scroll`, `computer_use_launch_app`,
  `computer_use_start_session`, and `computer_use_end_session`, forwarding only
  allowlisted, bounded arguments.
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
