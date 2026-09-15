# Changelog

## [0.1.0]

### Added

- Initial official API 0.2 Ygg Browse bundle.
- Pinned confirmed background setup for Playwright 1.57.0 and isolated Chromium.
- Always-headful persistent profile, strict HTTP(S)/manual-auth/action-confirmation policy, explicit tab IDs, snapshot generations, bounded artifacts, generic presentation, and local/mocked tests.
- Explicit injected browser-session connectors with complete target identities, host-derived ownership and revision fencing, bounded capability declarations, separate select/revoke/stop/status tools, and fail-closed cleanup. Existing targets are opt-in only; native Firefox and Safari remain unsupported.
- [Connector registration](CONNECTORS.md) documents the host integration contract without enabling browser discovery.
