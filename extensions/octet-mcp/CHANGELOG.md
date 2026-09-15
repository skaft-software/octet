# octet-mcp changelog

## [Unreleased]

### Changed

- Gate Streamable HTTP MCP behind the conspicuous, one-shot
  `octet --experimental-streamable-http-mcp` process-owner flag. Configuration,
  environment, project files, session/host requests, and manifest arguments
  cannot enable it; denied activation fails before credentials, DNS, network, or
  manager workers. Local stdio MCP is unchanged.
- Remediate the nine recorded Streamable HTTP defects with address-pinned TLS,
  killable/reaped DNS helpers, immutable host-owner fencing, streaming peer
  dispatch, bounded control workers, cumulative resumption budgets, strict EOF
  and chunk framing, empty-ID cursor reset, and one startup/catalog deadline.
- Keep Streamable HTTP blocked-by-default and experimental. Multi-owner catalog
  visibility/settlement, uncooperative injected callbacks and live/platform
  qualification remain closure gates. Ownerless remotes now park until an owned
  `/mcp restart <server>`; foreign owners require a new extension process.
- Credential adapters now receive the immutable host `ResourceOwner`; there is
  still no stock credential provider, static auth header or OAuth flow.

### Added

- Add deterministic nine-defect regressions, including real local TLS verification,
  DNS-child cleanup, overlapping peer IDs, bounded controls, resumptions and
  truncated JSON/SSE/chunked streams. No external server or credential is used.
- Add deterministic configuration, runtime, and product-boundary coverage for
  the gate.
- Add an explicit, bounded Streamable HTTP transport with negotiated session
  identity, JSON/SSE response framing, no-replay SSE resumption, cancellation,
  status/content-type policy, and lifecycle reconnects.
- Add strict remote configuration, exact-origin/TLS/redirect controls, and a
  non-persistent bearer credential-adapter boundary; OAuth and static credential
  configuration remain intentionally unsupported.

<a id="010--ygg-061"></a>

## [0.1.0]

Ygg 0.6.1.

### Added

- Add the first API `0.2` dynamic-catalog MCP bridge.
- Support explicit user and digest-pinned trusted-project configuration for
  bounded local stdio tool servers, with private permissions required whenever
  explicit environment values are present.
- Add epoch-aware add/replace/remove publication, conservative approval,
  cancellation, timeout, reconnect/backoff/parking, bounded logs, and graceful
  cleanup.
- Preserve MCP text, structured, image, and audio results through Ygg's typed
  result and artifact boundaries.
- Publish the generic frontend-neutral status/activity/tree/detail/action
  snapshot used by TUI and Serve, with headless `/mcp` fallbacks.
- Bundle the tested dependency-free Python SDK, real/adversarial fixtures,
  presentation fixtures, release smoke coverage, and configuration schema.
