# Changelog

## Unreleased

### Added

- An options menu under `/extensions` replaces the `/browse` slash command in
  the terminal UI. It shows the setup and browser state and offers only what
  applies: **Set up the browser** (or **Follow setup progress**), **Open the
  browser** or **Close the browser**, **Check status**, and **Reset the browser
  profile**. Setup now reports each step live, including Chromium's download
  percentage; only the percentage and size are read back from the install log.
  Esc stops following while the install continues. The web UI keeps the
  `/browse` command for now.

### Deprecated

- Deprecate this bundle in favour of the computer-use extension, which drives a
  locally installed MIT-licensed Cua Driver for native desktop control on
  macOS, Windows, and Linux. Browse remains published, installable, and
  unchanged in behavior: it is retained because its visible, isolated,
  Octet-owned Chromium profile and manual-authentication boundary are a safety
  property the computer-use path does not provide. No removal date is set; it
  will be retired only after that path demonstrably covers the isolated-browser
  case. See [deprecation](README.md#deprecation).

- Request non-activating creation for tool-created isolated tabs, matching the exact target and cleaning up cancelled creation without a foreground fallback. Chromium remains visible; initial-launch and page-popup focus are not yet resolved or physically qualified. See [qualification](QUALIFICATION.md).
- Document [native Firefox/Safari connector prerequisites](CONNECTORS.md#native-firefoxsafari-prerequisites-378) and test the existing fail-closed native/identity/ownership boundary; native operation remains unsupported.

## [0.1.0]

### Added

- Initial official API 0.2 Ygg Browse bundle.
- Pinned confirmed background setup for Playwright 1.57.0 and isolated Chromium.
- Always-headful persistent profile, strict HTTP(S)/manual-auth/action-confirmation policy, explicit tab IDs, snapshot generations, bounded artifacts, generic presentation, and local/mocked tests.
- Explicit injected browser-session connectors with complete target identities, host-derived ownership and revision fencing, bounded capability declarations, separate select/revoke/stop/status tools, and fail-closed cleanup. Existing targets are opt-in only; native Firefox and Safari remain unsupported.
- [Connector registration](CONNECTORS.md) documents the host integration contract without enabling browser discovery.
