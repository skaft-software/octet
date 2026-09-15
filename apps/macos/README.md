# Octet Serve for macOS

Native SwiftUI companion for an authoritative Serve host. This target is deliberately source-only: it expects the sibling `../apple-shared` shared client for discovery, TLS/HostId validation, pairing, Keychain storage, bootstrap, replay, reconnect, and idempotent commands.

## Current build status (observed, not inferred)

- The sibling package now compiles: `swift build` in `apps/apple-shared` -> `Build complete!`.
- This target still does **not** build. `apps/macos/Package.swift` names the shared package's only
  real product (`OctetServe`), excludes `Sources/OctetMacOS/Resources/Info.plist` from the executable
  target (`scripts/build-app.sh` installs it into `Contents/Info.plist`; SwiftPM forbids Info.plist as
  a resource), and the three source files that referenced the absent module import `OctetServe`
  instead. `swift build` then reports exactly three errors, all in
  `Sources/OctetMacOS/MacOSClientFactory.swift` and all caused by one missing surface:
  `cannot find type 'ServeClient' in scope` (`:7`), `cannot find 'ServeClientConfiguration' in scope`
  (`:11`), `cannot find 'ServeClient' in scope` (`:17`).
- Missing primitive (nothing was invented here): a shared client module that exports the API this
  target consumes — `ServeClient`, `ServeClientConfiguration`, `ServeClientError`,
  `ServeConnectionState`, `ServeBootstrap`, `ServeCommands`/`ServeCommand`/`ServeCommandResult` and
  `ServeEvent` (transport, TLS/HostId validation, pairing, Keychain credential storage, replay,
  reconnect, idempotent commands). None of those names is declared anywhere in this tree
  (`apps/apple-shared/Sources/OctetServe` holds wire DTOs only: `Identifiers`, `JSON`,
  `RuntimeModels`, `WireEnums`, `WireModels`). The alternative is to rewire this target onto the
  app-owned boundary idiom that `apps/ios` uses (`ServeClientBoundary` + a closure factory), which is
  a redesign rather than a missing file.
- Still hardware/qualification-gated and not claimed: Xcode app build, code signing, notarization,
  DMG/install/update, a live Serve host, LAN discovery, pairing, and sleep/wake reconnect.

## Dependency contract

`Package.swift` expects the sibling package at `apps/apple-shared` (the app path is `apps/macos`, so the relative package path is `../apple-shared`) and its `OctetServe` product. The required public API is recorded in the private macOS handoff artifact and must be kept compatible by the shared package; no HTTP, WebSocket, QR, TLS, or credential implementation belongs in this target.

## Scope

- LAN discovery and manual host inspection, fingerprint-word confirmation, one-time ticket pairing, approval polling, cancellation, revocation, and host-identity-change handling.
- Inventory bootstrap followed by session snapshots and live host events; streamed assistant/tool output, progress, source/output references, approvals, user-input requests, interrupt, steer, and follow-up controls.
- PR status and links are rendered from host-authoritative session state.
- Sleep/wake and reconnect preserve selected session and drafts. Ambiguous mutations are reconciled by command ID and are never submitted a second time by the UI.
- Passive notifications contain only a session/event route. Clicking one revalidates authority and cursor state before opening a session.
- SwiftUI accessibility labels, keyboard commands, focus-safe background updates, and native menu/window behavior.

## Source-only packaging tools

The scripts under `scripts/` are authored but intentionally not run in this source-only delivery:

- `build-app.sh` assembles a versioned unsigned `.app` from a release SwiftPM build and requires `CONFIRM_BUILD=OCTET_BUILD` before replacing an existing output.
- `sign-app.sh` signs with an explicitly supplied Developer ID identity and entitlements; it refuses an absent identity.
- `make-dmg.sh` verifies the app signature and creates a distributable DMG; replacing an existing DMG requires `CONFIRM_DMG=OCTET_DMG`.
- `notarize-app.sh` submits a Developer ID-signed app through an explicit notarytool keychain profile, staples and validates it, and replaces it only with `CONFIRM_NOTARIZE=OCTET_NOTARIZE`.
- `install.sh` verifies the bundle identity/signature and installs a locally supplied, already signed app with `CONFIRM_INSTALL=OCTET_INSTALL`.
- `update.sh` verifies a local artifact's SHA-256, signature, bundle identity, and version, then replaces the app only with `CONFIRM_UPDATE=OCTET_UPDATE`.
- `remove.sh` requires an explicit confirmation before deleting only the installed app; shared pairing credentials remain owned by the shared Serve client package.

Replacement operations stage beside the installed app and retain a rollback copy until the new app has been moved into place. No script removes quarantine metadata.

No artifact is signed, notarized, installed, updated, removed, built, or live-tested by this change.
