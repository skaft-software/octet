# Octet Companion for iOS

Source-only native SwiftUI controller for an authoritative Serve host.
The default composition cannot pair or connect to a live host.
 The app is a
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

The Xcode application build needs the sibling `../apple-shared` package, used by the application target. Build a copy outside the repository so the shared worktree stays clean:

```sh
rm -rf /tmp/octet-ios-verify && mkdir -p /tmp/octet-ios-verify
cd apps && rsync -a --exclude '.build' ios apple-shared /tmp/octet-ios-verify/
cd /tmp/octet-ios-verify/ios
xcodegen generate --spec project.yml
xcodebuild -project OctetCompanion.xcodeproj -scheme OctetCompanion \
  -destination 'generic/platform=iOS Simulator' -configuration Debug CODE_SIGNING_ALLOWED=NO build
```

Generating the project in place matters: `INFOPLIST_FILE` and the `Resources`
entry are relative to the project, so `xcodegen --project <elsewhere>` fails with
`error: Build input file cannot be found: '.../Resources/Info.plist'`.

Observed at the time of writing: `** BUILD SUCCEEDED **` (and
`** TEST BUILD SUCCEEDED **` for `build-for-testing`), with only two warnings
(`Metadata extraction skipped, no AppIntents.framework dependency found`,
`UnnecessaryEffectMarker` at `CompanionSessionService.swift:57`). Running the tests
*inside a simulator* still stops at app installation (`error: App installation
failed: Unable to Install "Octet Companion"` under `CODE_SIGNING_ALLOWED=NO`),
which is a signing/simulator gate rather than a source defect; the same 19 tests
execute on the host through `swift test`.

A `xcrun ... swiftc -typecheck` against the iOS 27 simulator SDK is a faster check
for the `@main` entry point and the views.

## Status

Source, unit tests, the SwiftPM host-side build and the Xcode application build
(`build` and `build-for-testing`, unsigned, simulator destination) are delivered
and were observed to succeed during earlier source checks. These observations
are not 0.8.0 candidate verification. Code signing, notarization, installation, a
simulator test *run* and any device run have not been performed and are not
claimed. The SwiftPM dependency on `../apple-shared` stays absent from
`Package.swift` because this library imports nothing from it; the Xcode app target
links it through `project.yml`.
