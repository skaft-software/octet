# Native delivery

**Design reference, not an available graphical application.** The octet 0.7.5
source has no Tauri, Xcode/iOS, or Android project, no signed graphical app
builds, and no implemented LAN pairing. Native CLI and Serve runtime archives
are separate release artifacts, not these proposed apps. See the
[local web client](README.md) for source launch and version-matched package
availability.

The design uses one shared React frontend in thin system-webview shells. Tauri
2 is an unvalidated candidate, not a selected dependency; Electron is out of
scope. The [LAN specification](lan-pairing.md#native-bridge) records the required
bridge and security contracts. Its platform API sketches do not establish a
chosen shell implementation.

## macOS

The proposed macOS shell would:

- start or attach to the bundled `octet serve`;
- host the shared frontend;
- use native file and folder pickers;
- support drag and drop, notifications, and deep links;
- store device identity in Keychain;
- integrate system appearance and reduced motion;
- expose LAN pairing and connected-device management, subject to the
  loopback-owner-only administration boundary.

A distributable build would require the user's Apple Developer ID credentials,
code signing, hardened runtime, and notarization. None is qualified here.

## iOS

The proposed iOS app is a companion, not a local octet agent runtime. It would
discover and pair with a LAN host, store its device identity in Keychain, use the
responsive shared frontend, and add native files/photos, notifications, and deep
links. If the host is offline, the companion is offline.

A device build requires Apple development signing and provisioning. TestFlight
is Apple's optional beta distribution channel; it is not required merely to
compile or install on a provisioned development device. No provisioned device or
TestFlight build is asserted here.

## Android

The proposed Android app is also a companion. It would use the shared responsive
frontend, LAN discovery and pairing, Android secure storage, native file/photo
attachment, notifications, and deep links.

A user-owned signed APK is the proposed local testing format; Play internal
testing and a signed Android App Bundle are separate distribution options.
Neither a signed APK nor an App Bundle exists in this snapshot.

## Shared-code rule

Native shells contain platform integration only. Session reducers, protocol
types, transcript rendering, product rules, themes, approvals, and preview
behavior remain shared; platform-specific forks of the core interface are not
part of the design. Credentials stay in native secure storage, never JavaScript.

The LAN-v1 scope excludes background mobile execution, push notifications, and
an offline transcript cache. Notification sketches above do not override those
exclusions. Distribution remains conditional on real-server web, secure pairing,
platform, physical-device, and signing acceptance; see the
[LAN acceptance matrix](lan-pairing.md#acceptance-matrix).
Work tracking is on the [Project](https://github.com/orgs/skaft-software/projects/5).
