# GitHub Copilot coding-host adapter candidate

Status: **source and mock-fixture candidate; Rust checks UNRUN; not release-qualified**.
This extends the [existing provider-seam candidate](copilot-current-candidate.md).
CLI flag dispatch and shared catalog composition are now connected in source;
this does not establish build, installed-host, live or full-provider acceptance.

## Implemented surface

- `auth::copilot::login(&CredentialStore, headless)` drives GitHub.com's device
  flow, presenting only the fixed verification URL and bounded user code through
  the existing control-safe output boundary. Headless never launches a browser.
- `auth::copilot::logout(&CredentialStore)` removes exactly the selected Copilot
  credential. It does not revoke GitHub authorization or delete other stores.
- `auth::copilot::register_available_models(&mut ModelCatalog, offline)` and
  `CopilotProvider::register_available_models` opt into the coding-host adapter.
  The explicit-store variant and `CopilotCodingHost` support isolated hosts.
- `CopilotCodingHost` implements the existing `CopilotHost`; the provider/session,
  `Auth::Dynamic`, protocol routing and staged catalog contracts are preserved.
- `--login copilot [--headless]` and `--logout copilot` (alias `github-copilot`)
  dispatch before workspace/session startup. Shared online catalog construction,
  setup rebuilds and Codex-only logout use the auth-owned synchronous bridge.
  It joins a scoped discovery thread rather than nesting `block_on` inside the
  caller's Tokio runtime; offline/missing credentials skip runtime/client creation.
- Existing NDJSON `models` and catalog-backed `run` consume that same catalog.
  No auth command, OAuth payload field, public target or extension API 0.3
  native-provider authority is added. TUI slash auth remains unimplemented.

## Credential and transport contract

Only versioned GitHub OAuth state is saved in
`~/.octet/credentials/copilot.json`. The store rejects unknown fields, invalid
versions, empty/control/whitespace tokens and tokens above 4096 bytes. The entire
file is bounded to 16 KiB. All I/O uses `octet_agent::secure_fs` owner-private,
no-symlink/no-hard-link descriptor-bound operations; publication and deletion
compare the selected bytes instead of overwriting a concurrent login. Errors and
`Debug` never include paths, body text, device state or token contents. No Codex,
custom-provider, editor, environment or third-party credentials are imported.

Device authorization uses `https://github.com/login/device/code` and
`https://github.com/login/oauth/access_token`; only
`https://github.com/login/device` may be presented/opened. The GitHub OAuth token
is sent only to `https://api.github.com/copilot_internal/v2/token` as `token`
auth. Inference auth uses the returned short-lived token as Bearer. GitHub's
public device client ID is not a secret and is not advertised as an Octet-owned
OAuth application. Its continuing service acceptance is **not live-qualified**.

Production requests are HTTPS-only, reject redirects, have a 10-second connect
and 30-second whole-operation bound, and bound JSON bodies (64 KiB auth, 1 MiB
inventory). Device lifetime is at most 30 minutes; polling honors pending,
expiry, denial and repeated five-second `slow_down` backoff. No detached poll,
save or refresh task survives a dropped operation.

The inference endpoint must be an HTTPS root on port 443, without userinfo,
query or fragment, on exactly one of:

- `api.githubcopilot.com`
- `api.individual.githubcopilot.com`
- `api.business.githubcopilot.com`
- `api.enterprise.githubcopilot.com`

Token-embedded proxy hints, custom Enterprise authorities, suffix matches and
redirect targets do not grant authority. A later origin change fails closed;
rebuild the host/catalog instead of silently rebinding registered endpoints.
The test-only constructor accepts one literal loopback origin and is not a
production configuration or environment override.

Inference sessions/dynamic headers are in memory only. The generic resolver
checks host availability even before returning a fresh cached credential. The
coding host compares the selected private credential with its bound bytes;
logout, replacement and invalid private storage prevent subsequent resolution.
Explicit provider exchange/refresh clear the cache before transport and share the
resolve/invalidation lock through installation, so failure or cancellation leaves
no old credential and completed invalidation cannot be undone by a late install.
Exchange and refresh also check the private credential before and after transport.
A rejected or changed inference origin permanently fences that host, including
fresh credentials in other resolvers sharing it; a new host/catalog is required.

A fresh exchange follows provider invalidation. Initial registration currently
uses two exchanges (origin binding, resolver installation) and one `/models`
request; it does not reuse a provider-invalidated session. Logout callers must
still discard old catalog metadata, cancel active inference and discard active
device hosts: already-resolved/in-flight requests are not remotely revoked, and
no cross-process remote revocation or pending-device cancellation is claimed.
Cancellation does not revoke the remote device code.

## Inventory and routing

Offline returns before path resolution, filesystem access or client creation;
missing credentials make no request. Neither path adds endpoints/models or
imports an inventory cache. Existing caller entries are not pruned. Online
failure is a redacted error, never a static or stale inventory fallback.

At most 128 returned entries are considered. Only picker-enabled, policy-enabled
chat models with explicit `/chat/completions` or `/responses` support qualify;
Chat wins when both are listed. Anthropic-only models are not relabeled. Model
limits, IDs, duplicate detection, tools/parallel-tools consistency, protocol
selection and catalog merge validation reuse the existing provider. Every record
is type-checked before eligibility filtering; malformed siblings invalidate the
whole inventory. Model IDs/labels echoing the known OAuth or inference token are
rejected, as is an exchange response that echoes OAuth as its inference token.
No raw response or token is serialized into model metadata.

This candidate intentionally does **not** qualify inferred reasoning controls,
vision, structured output, request-specific initiator headers, Anthropic routes,
custom Enterprise policy or an offline inventory cache. Reasoning-flagged models
are withheld rather than given fabricated effort semantics; vision/structured
output are not advertised. This is not full Copilot inventory parity.

## Mock qualification matrix (authored, UNRUN)

`crates/octet-coding-agent/tests/copilot_host_current.rs` includes the owned auth
source and existing output boundary while using the public SDK provider seam.
It needs no new SDK re-export. All credentials are sentinels and all HTTP is
loopback; tests never call production login or online default-store registration.

Coverage authored: offline/missing readiness; headless login; exact logout;
private modes/link rejection; malformed/oversized stores; denied/expired/
cancelled/pending polling; backoff and concurrent login replacement; device
presentation and authority rejection; Chat/Responses route preservation;
secret-echo rejection; empty/unsupported/duplicate/malformed/oversized inventory
atomicity; refresh and replaced/deleted credentials; concurrent logout during
exchange; changed-origin refusal; redirect, malformed/oversized body,
expired-session and HTTP-error redaction. The changed-origin fixture is rejected
at the exact mock-origin boundary; cross-production-origin pinning is source
review only, not exercised by allowing a mock to reach production.
Existing `copilot_current.rs` remains the SSE, provider refresh and stream-drop
qualification surface; its prior UNRUN status is not upgraded here.

Coordinator source review added an actual catalog-resolver regression for fresh
credentials after logout, account replacement and malformed private storage;
the changed-origin fixture now checks sticky host and fresh-resolver rejection
without a second exchange. Private provider unit fixtures use an explicit gate,
not a timing race, to check both exchange and refresh against invalidation,
cancellation and failed replacement. A blocking-bridge fixture exercises retained
client/host refresh after its scoped runtime exits; a Unix-only CLI fixture uses
both logout aliases against an isolated HOME and preserves a sibling store. No
CLI login or default online catalog is executed by those fixtures. **All these
Rust fixtures remain UNRUN.**

## Coordinator verification (not executed here)

```sh
cargo fmt --all -- --check
cargo check --locked -p octet-coding-agent --all-targets
cargo test --locked -p octet-coding-agent --test copilot_host_current
cargo test --locked -p octet-coding-agent --test copilot_current
cargo test --locked -p octet-coding-agent --lib providers::copilot::tests
```

Review the final diff and keep formatting fixes scoped to owned files. The sole
Rust producer remains blocked by Temper access; no alternate build or dependency
change is authorized. Source hashing/patch checks are not Rust verification.
TUI slash auth, full protocol/inventory parity, startup/cancellation and installed
host/physical-terminal acceptance, plus explicitly authorized account-scoped live
qualification, remain open. No real credential, live device flow or provider
request was used in this review; #249 remains incomplete.
