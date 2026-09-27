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
- Credential adapters now receive the immutable host `ResourceOwner`; an
  unresolved `bearer` reference still has no stock credential adapter or OAuth
  flow.
- Add the bundled static credential source: `"type": "static-bearer"` with an
  extension-scoped `OCTET_MCP_*` `environment` name, read per request and never
  stored, logged, echoed, or published. Non-namespaced names (for example an
  ambient provider token) and any literal token field stay rejected, so the
  fail-closed default is unchanged.

### Added

- Support configured desktop MCP servers through a two-stage environment grant:
  a per-server `inheritEnv` array naming reviewed non-secret desktop/session
  variables the bridge copies from its own process, layered on the host's
  manifest-gated broker list. The default forwards nothing extra; unsupported,
  duplicate, malformed, oversized, and remote-transport uses are rejected at
  load time, unset names are skipped, and explicit `env` values still win.
- Support optional `confirmUnknownTools` (default `false`): after host policy
  allows a call, the bridge asks the user through the host confirmation service
  before dispatching any tool without an exact `readOnlyHint: true`. A missing,
  unavailable, failed, declined, or cancelled confirmation fails closed without
  dispatch, and the prompt never includes tool arguments.
- Add a disabled `config.cua-driver.example.json` profile and a bundled
  `cua-driver` skill for operating an externally installed Cua Driver
  (macOS, Windows, Linux). The driver is separate third-party software; this
  bundle never installs, vendors, or starts it, and the example ships disabled
  with confirmation enabled.
- Add deterministic regressions for environment filtering, explicit-value
  precedence, confirm allow/deny/headless behavior, and example-profile
  inertness.

- Add deterministic nine-defect regressions, including real local TLS verification,
  DNS-child cleanup, overlapping peer IDs, bounded controls, resumptions and
  truncated JSON/SSE/chunked streams. No external server or credential is used.
- Add deterministic configuration, runtime, and product-boundary coverage for
  the gate.
- Add an explicit, bounded Streamable HTTP transport with negotiated session
  identity, JSON/SSE response framing, no-replay SSE resumption, cancellation,
  status/content-type policy, and lifecycle reconnects.
- Add the optional permanent GET notification stream. It opens only when the
  negotiated server capabilities declare `listChanged: true`, renews each
  connection inside `requestTimeoutMs`, reconnects with the committed
  `Last-Event-ID`, fails closed on a replayed acknowledged identity
  (`sse_event_replayed`), treats `405` as `unsupported` without failing any POST
  path, and is bounded by `backoffInitialMs`→`backoffMaxMs`/`maxRestarts` and 64
  total connections.
- Add deterministic static-credential and permanent-stream regressions: scoped
  environment resolution, rotation, refusal of ambient names, absent/unset
  variables, committed-cursor reconnection, replayed identity, `405`
  non-retry, no GET without a declared capability, and a manager catalog refresh
  driven by a stream notification.
- Add strict remote configuration, exact-origin/TLS/redirect controls, and a
  non-persistent bearer credential-adapter boundary; OAuth/browser authorization
  remains policy-gated and unimplemented.

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
