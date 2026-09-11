# octet-mcp changelog

## [Unreleased]

### Changed

- Gate Streamable HTTP MCP behind the conspicuous, one-shot
  `octet --experimental-streamable-http-mcp` process-owner flag. Configuration,
  environment, project files, session/host requests, and manifest arguments
  cannot enable it; denied activation fails before credentials, DNS, network, or
  manager workers. Local stdio MCP is unchanged.
- Preserve all nine original Streamable HTTP defect IDs in a commit-pinned
  compatibility/qualification ledger; keep the gate while candidate regressions
  and live-provider release qualification remain distinct.
- Pin DNS destinations while preserving TLS SNI/hostname verification, reap
  cancellable resolver subprocesses, share framing/control/resumption budgets,
  reject truncation, reset empty SSE cursors and enforce one startup deadline.
- Fence remote connections, credentials, catalogs, callbacks and late results to
  one exact host owner/session/generation. Observational commands stay inert.
- Preserve supported schema semantics (including bounded local references) and
  render structured-only results explicitly for model visibility without changing
  API `0.2` metadata/output-schema rules.

### Added

- Add a generic host-owned exact-call approval adapter for isolated API `0.2`
  `octet-mcp`, with complete review, one-use redemption and dispatch-time catalog
  fencing. This is not #383's typed app/origin/observation automation policy.
- Add explicit stateless HTTP `2026-07-28` discovery/metadata/schema-derived headers
  alongside legacy initialization, not automatic protocol fallback.
- Add capability-gated resource listing, templates and bounded opaque URI reads
  over the existing connections; never dereference a resource on the host.
- Add private per-operation HTTP form/URL elicitation and bounded modern MRTR.
  Unbound/unsupported interactions fail closed; no automatic browser actions.
  The TUI requires complete escaped context to fit its viewport with terminal
  write logging disabled. Serve cancels private input until it has a private
  prompt channel, rather than exposing context or accepting blind answers.
- Add user-command-only bearer and pre-registered public-client OAuth setup,
  discovery/PKCE/issuer/resource/scope checks, loopback/manual completion,
  serialized rotating refresh and owner-private plaintext persistence. Static
  credential/header configuration and automatic authentication/tool replay remain
  unsupported; logout is local removal, not upstream revocation.
- Add controlled transport/TLS, runtime/private-SDK, resource, storefront-shaped,
  OAuth and approval regressions, packaging checks and an MCP usage skill. These
  are not live Shopify/Wix/customer login, purchase or production qualification.
- Retain explicit gaps for subscriptions, modern stdio, advanced schema/forms,
  OAuth registration/enterprise features and unrun real-world/frontend journeys.

### Fixed

- Wait for credential-store contention within the operation's deadline,
  cancellation and owner fences, then reread the winning token. Healthy concurrent
  refresh or bearer reads no longer invalidate connections or abort the winner.
- Redact private interaction payloads without rewriting MCP error flags, content
  discriminators or known protocol fields. Check raw string types, embedded/link
  resources and the captured output schema before redaction; incompatible redacted
  payloads still fail closed.
  Arbitrary structured data, metadata and progress remain redacted.
- Correct the OAuth callback documentation: `/oauth/callback` is fixed; the
  default port is OS-selected and OAuth state is random and validated.

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
