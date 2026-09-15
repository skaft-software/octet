# Octet Companion for iOS

Native SwiftUI controller for an authoritative Serve host. The app is a
companion, not an agent runtime: it renders host-reported state and sends
host-validated commands, and it is offline when the host is offline.

## Layout

| Path | Contents |
| --- | --- |
| `Package.swift` | SwiftPM package used for host-side build and unit tests. |
| `project.yml` | XcodeGen spec for the iOS application target `OctetCompanion` and its `OctetCompanionTests` bundle. |
| `Sources/OctetCompanionApp.swift` | The `@main` application entry point (application target only, not part of the SwiftPM library). |
| `Sources/OctetCompanion/` | Host-neutral library: wire models/decoders, command envelopes, credential + pairing stores, the session service actor, the app model and the SwiftUI views. |
| `Tests/OctetCompanionTests/` | Behavioral unit tests with an in-memory pairing/credential store and a scripted transport. |
| `Resources/` | `Info.plist` and the keychain entitlements for the application target. |

## Boundaries

- **Fail-closed composition.** `CompanionComposition.makeService()` composes the
  unavailable client factory and the refusing pairing adapter by default, so a build
  that has not bound a real transport can pair nothing, connect to nothing and send
  nothing. `CompanionComposition.isHostBound(factory:)` lets the UI say so instead of
  pretending the host is merely offline.
- **Transport adapter.** The shared `OctetServe` package (and the app's
  `ServeClientFactory`/`ServeClientTransport` boundary) owns TLS/pinning, discovery,
  pairing transport, replay and idempotency. No HTTP, WebSocket, QR or credential
  implementation belongs in this target.
- **Host-authoritative UI.** Transcript text, pending requests, the active run and
  command acknowledgements all come from the host. Unknown host payloads are dropped
  rather than rendered, provisional entries are labelled, and an approval is answered
  only for the `requestID` and actor generation the host reported.
- **Bounded input.** The app model refuses empty prompts/answers and enforces the
  encoder's own limits (256 KiB prompt, 16 KiB answer) before the service is called.

## Build and test

Host-side library build and unit tests (macOS, no simulator required):

```sh
cd apps/ios
swift build
swift test
```

iOS type check of the `@main` entry point and views (uses the iOS simulator SDK):

```sh
xcrun --sdk iphonesimulator swiftc -typecheck -target arm64-apple-ios16.0-simulator \
  -swift-version 5 -module-name OctetCompanion \
  Sources/OctetCompanionApp.swift Sources/OctetCompanion/*.swift Sources/OctetCompanion/Views/*.swift
```

The Xcode application build also needs a compiling sibling
`../apple-shared` package: `xcodegen generate --spec project.yml` followed by
`xcodebuild -scheme OctetCompanion -destination 'generic/platform=iOS Simulator'`.
At the time of writing `apps/apple-shared/Sources/OctetServe` does not compile
(`WireEnums.swift` uses `extension` as an enum case name; `RuntimeModels.swift:44` is a
collapsed line), so the application target cannot be linked yet. That package is
owned outside this directory.

## Status

Source and unit tests are delivered. No Xcode application build, signing,
notarization, installation, simulator run or device run has been performed or is
claimed; the SwiftPM dependency on `../apple-shared` is intentionally absent from
`Package.swift` until that package compiles.
