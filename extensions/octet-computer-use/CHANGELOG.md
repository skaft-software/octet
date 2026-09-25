# Changelog

## Unreleased

### Changed

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
