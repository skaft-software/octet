# Changelog

## Unreleased

- Request non-activating creation for tool-created isolated tabs, matching the exact target and cleaning up cancelled creation without a foreground fallback. Chromium remains visible; initial-launch and page-popup focus are not yet resolved or physically qualified. See [qualification](QUALIFICATION.md).
- Document [native Firefox/Safari connector prerequisites](CONNECTORS.md#native-firefoxsafari-prerequisites-378) and test the existing fail-closed native/identity/ownership boundary; native operation remains unsupported.

## [0.1.0]

### Added

- Initial official API 0.2 Ygg Browse bundle.
- Pinned confirmed background setup for Playwright 1.57.0 and isolated Chromium.
- Always-headful persistent profile, strict HTTP(S)/manual-auth/action-confirmation policy, explicit tab IDs, snapshot generations, bounded artifacts, generic presentation, and local/mocked tests.
- Explicit injected browser-session connectors with complete target identities, host-derived ownership and revision fencing, bounded capability declarations, separate select/revoke/stop/status tools, and fail-closed cleanup. Existing targets are opt-in only; native Firefox and Safari remain unsupported.
- [Connector registration](CONNECTORS.md) documents the host integration contract without enabling browser discovery.
