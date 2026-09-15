# Octet Serve for macOS

Native SwiftUI companion for an authoritative Serve host. This target is deliberately source-only: it uses the sibling `../apple-shared` `OctetServeClient` for discovery, TLS/HostId validation, pairing, Keychain storage, bootstrap, replay, reconnect, and idempotent commands.

## Scope

- LAN discovery and manual host inspection, fingerprint-word confirmation, one-time ticket pairing, approval polling, cancellation, revocation, and host-identity-change handling.
- Inventory bootstrap followed by session snapshots and live host events; streamed assistant/tool output, progress, source/output references, approvals, user-input requests, interrupt, steer, and follow-up controls.
- PR status and links are rendered from host-authoritative session state.
- Sleep/wake and reconnect preserve selected session and drafts. Ambiguous mutations are reconciled by command ID and are never submitted a second time by the UI.
- Passive notifications contain only a session/event route. Clicking one revalidates authority and cursor state before opening a session.
- SwiftUI accessibility labels, keyboard commands, focus-safe background updates, and native menu/window behavior.

## Dependency contract

`Package.swift` expects the sibling package at `apps/apple-shared` (the app path is `apps/macos`, so the relative package path is `../apple-shared`) and its `OctetServeClient` product. The required public API is recorded in the private macOS handoff artifact and must be kept compatible by the shared package; no HTTP, WebSocket, QR, TLS, or credential implementation belongs in this target.

## Source-only packaging tools

The scripts under `scripts/` are authored but intentionally not run in this source-only delivery:

- `build-app.sh` assembles a versioned unsigned `.app` from a release SwiftPM build and requires `CONFIRM_BUILD=OCTET_BUILD` before replacing an existing output.
- `sign-app.sh` signs with an explicitly supplied Developer ID identity and entitlements; it refuses an absent identity.
- `make-dmg.sh` verifies the app signature and creates a distributable DMG; replacing an existing DMG requires `CONFIRM_DMG=OCTET_DMG`.
- `notarize-app.sh` submits a Developer ID-signed app through an explicit notarytool keychain profile, staples and validates it, and replaces it only with `CONFIRM_NOTARIZE=OCTET_NOTARIZE`.
- `install.sh` verifies the bundle identity/signature and installs a locally supplied, already signed app with `CONFIRM_INSTALL=OCTET_INSTALL`.
- `update.sh` verifies a local artifact's SHA-256, signature, bundle identity, and version, then replaces the app only with `CONFIRM_UPDATE=OCTET_UPDATE`.
- `remove.sh` requires an explicit confirmation before deleting only the installed app; shared pairing credentials remain owned by `OctetServeClient`.

Replacement operations stage beside the installed app and retain a rollback copy until the new app has been moved into place. No script removes quarantine metadata.

No artifact is signed, notarized, installed, updated, removed, built, or live-tested by this change.
