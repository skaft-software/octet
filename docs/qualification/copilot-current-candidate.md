# GitHub Copilot current qualification candidate

Status: **source/fixture candidate only; not release-qualified**.

## Bounded implementation

`crates/octet-coding-agent/src/providers/copilot.rs` already provides the
host-owned seam used by this candidate:

- `CopilotHost` owns device login, OAuth state, exchange, refresh, and discovery;
- `CopilotDeviceLogin` exposes bounded display fields while redacting `Debug`;
- `CopilotSession` retains only a short-lived in-memory credential and validated
  dynamic headers behind `octet_ai::Auth::Dynamic`;
- `CopilotProvider::register_models` requires availability, exchange, and
  credential-free discovery, validates/bounds the inventory, stages routes, and
  merges atomically; and
- each discovered model's explicit `Protocol` selects the declared Chat or
  Responses route. Unsupported protocols are rejected rather than inferred from
  names.

The new `crates/octet-coding-agent/tests/copilot_current.rs` is the dedicated
fixture surface. It uses only fixed sentinel values and existing checked-in
SSE/error bodies; it performs no GitHub request, environment credential read, or
credential persistence.

## Fixture matrix (proposed; UNRUN)

| Fixture | Contract exercised |
| --- | --- |
| `device_fixture_stays_host_owned_and_out_of_standalone_catalogs` | device display/status, exchange seam, host-owned definition, no standalone Copilot preset, redacted display debug |
| `unavailable_discovery_and_unsupported_protocol_fixtures_fail_closed` | login/discovery errors, unsupported protocol, no endpoint/model mutation |
| `protocol_metadata_fixture_selects_routes_without_model_name_heuristics` | per-model Chat/Responses metadata and endpoint binding |
| `chat_and_responses_stream_fixtures_preserve_route_and_usage` | deterministic request paths, auth/dynamic headers, SSE decoding, text, usage, finish |
| `refresh_fixture_replaces_stale_primary_and_dynamic_headers` | safety-skew refresh, replacement primary credential and dynamic header |
| `refresh_error_fixture_discards_the_stale_session` | refresh error mapping, stale-session removal, next-request exchange fallback |
| `error_fixture_redacts_primary_and_dynamic_credentials_everywhere` | HTTP error body and provider/model/session/catalog debug redaction |
| `cancel_fixture_closes_an_inflight_http_stream` | dropping an active SSE stream closes the loopback transport |

## API and comparator evidence

Actual Octet API evidence read from this checkout:

- `CopilotHost` has `availability`, `begin_device_login`, `poll_device_login`,
  `exchange`, `refresh`, and `discover_models` methods.
- `CopilotEndpoint` requires an HTTPS origin root (literal loopback HTTP only for
  deterministic tests); userinfo, path, query, and fragment are rejected.
- `CopilotProvider::register_models` calls host availability/exchange/discovery,
  uses `route_for_protocol`, and stages before catalog mutation.
- Generic preset bootstrap skips `HostOwned` authentication; the separate
  authenticated coding-host adapter now contributes eligible models to shared
  catalog construction without an environment/static preset.
- `octet-ai` dynamic auth marks primary and extra headers sensitive and sanitizes
  provider error diagnostics; stream dropping is the cancellation boundary.

Comparator evidence: Pi checkout
`08dc60bc52d89d6823a9738cc90b1916e5e446e5`, `@earendil-works/pi-ai@0.85.1`,
MIT-licensed. Its Copilot adapter demonstrates proxy-derived account base URLs,
OAuth device flow and refresh, account `/models` policy filtering, and model
metadata dispatch among Anthropic Messages, OpenAI Completions, and Responses;
it also derives `X-Initiator`, `Openai-Intent`, and vision headers per request.
The pinned Octet ledger remains `@earendil-works/pi-coding-agent@0.84.4` and
records Copilot unsupported because exact 0.84.4 route/auth evidence and live
OAuth validation are absent. No Pi implementation was copied.

Codex comparator `3d3df0a0cad5d3d8d3340b633787e9dd304ea463` is Apache-2.0; the
inspected Copilot-related reference is an MCP origin, not a Copilot OAuth or
inference implementation. No Codex code was copied.

## Exact integration gap and gates

The [coding-host adapter candidate](copilot-host-current-candidate.md) owns
GitHub.com's device/OAuth endpoints, private credential storage, vetted inference
origins and dynamic catalog registration. Source CLI flag dispatch now supports
`copilot`/`github-copilot`, and synchronous shared catalog construction calls the
auth adapter. Existing native-host catalog/run callers inherit those models
without any new auth command or OAuth field. TUI slash auth, custom Enterprise
policy, full inventory/protocol/reasoning parity and native/live qualification
remain open; source imports and authored fixtures do not close #249.

Coordinator source review also made `availability` a fresh-credential boundary
and serialized explicit exchange/refresh with resolver invalidation. The private
provider unit fixtures cover failed, cancelled and invalidated replacement; the
host fixtures cover fresh cache invalidation after local credential changes and
rejected inference origins. These additional Rust fixtures are **UNRUN**.

Still required:

- Rust verifier execution of the dedicated test and existing provider unit tests
  (including compilation, formatting, and dependency/build checks);
- completed-stack review and caller qualification while preserving native-host
  and extension API authority boundaries;
- live account-scoped model inventory/OAuth validation, explicitly outside
  ordinary CI; and
- physical terminal/installed-host acceptance if the embedding integration is
  ever exposed.

No live login, token persistence, catalog credential, or public publication was
performed here.
