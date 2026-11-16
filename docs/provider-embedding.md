# Embedding the provider layer

`octet-ai` is the provider dependency. It has no dependency on the agent,
frontend, terminal renderer, sessions, tools or extension processes. Use a Git
revision containing the provider extraction, not an unpublished install command.
Pin implementation/fixture revision
`11c5954e2e7d3278ab9eca3b37fca9904eb34e22` or a reviewed descendant. No optional
feature flags are required. The independent consumer is `examples/provider-only`.

## Explicit effects

| API | Effects |
| --- | --- |
| `provider_auth::codex::CredentialStore::memory()` / `new(path)` | Construct a handle only; no files, requests or imports. |
| `CredentialStore::is_signed_in()` | Explicit bounded local credential read; no refresh/import/write. |
| `providers::builtin_provider_definitions()` | Credential-free declarations only; no initialization or I/O. |
| `providers::codex::CodexCatalog::fallback()` / `from_inventory(body)` | Pure snapshot construction; no I/O. Supplied inventory is caller authority, not proof of authentication. |
| `CodexCatalog::load_local(&store)` | Explicit private credential/cache read; no network, refresh or import, even with expired credentials. |
| `CodexCatalog::discover(&store).await` | Explicit authorization of Codex credential refresh and account inventory, plus account-scoped cache publication. No unrelated discovery or models.dev fetch. |
| `snapshot.bind_into(&mut catalog, store, override, timeout)` | Explicit local account check and exact route binding; no network. |
| `login_with_interaction(...)` / `store.delete_async()` | Explicit login/persistence or local logout. Logout also removes that store's model cache; it does not revoke remote requests. |
| `AiClient::complete` / `stream` | Caller-authorized inference and necessary dynamic credential refresh. No tool execution or continuation. |

No constructor imports third-party credentials. The frontend explicitly opts in
to its existing one-time migration through `with_default_legacy_import`.
Use a private, absolute caller-selected credential path when embedding. The file
adapter retains the existing JSON format, bounded reads, owner/private checks,
atomic publication and cross-process refresh lock. In-memory storage shares
credential/cache state across cloned handles and requires no application path.
Applications do not parse credential JSON, copy tokens, construct Authorization
headers, or use an OpenAI API key to replace a subscription route.

## Local selection, then one inference

1. Open an explicit store and call `load_local`, or separately authorize
   `discover`. Missing login is `None` on local loading. Invalid/private-file
   failures are errors rather than missing credentials.
2. Inspect `models()` and `source()` without I/O. Conservative fallback models
   are suggestions, **not confirmed account availability**. A fresh cache is
   account/plan-bound; offline loading strips dynamic collaboration/update
   features. An authenticated inventory's omissions remain authoritative.
3. Bind into the existing `ModelCatalog` with
   `providers::codex_context::CodexContextOverride::NONE` (or a validated explicit
   budget), then resolve the exact catalog ID, such as `codex/gpt-6-luna` **only
   when present**. Missing selection is an error; there is no provider/model or
   billing-lane substitution. Binding preserves the declared Codex Responses
   runtime, headers, capabilities, context defaults/maxima and unknown pricing.
   Public OpenAI prices/capabilities do not qualify subscription routes.
4. Submit one canonical `Request` with `AiClient`. Use exact advertised reasoning
   values/defaults. Inspect terminal `StopReason`, usage and optional cost; a
   refusal, truncation or unknown incomplete reason is not ordinary completion.
   Premature EOF and failed transports are errors, not successful partial text.

`AiClient::with_stream_timeouts`, `with_initial_stream_timeout`, endpoint header
`timeout`, and `RequestOverrides::timeout_ms` expose request bounds. **Codex
omits the wire output-token cap**: `effective_output_token_cap` returns `None`
on this declared route even if the canonical request asks for a smaller output.
A consumer requiring an enforceable hard output-token ceiling must refuse that
operation; do not mistake a preference or local timeout for a provider cap.
This extraction does not repair or qualify a downstream hard-budget policy.

Library calls are explicit network authorization, not an application-wide network
firewall. A no-live policy must not invoke login/discovery/inference. Local
snapshots remain usable without those calls, including expired/stale state.

## Interaction, lifetime and cancellation

`provider_auth::codex::LoginInteraction` supplies `notify`, `open_browser` and an
optional `browser_page` callback. Select `LoginMethod::Browser` or `Device` in
the caller. Notifications carry bounded public instructions and commit progress,
not tokens/verifiers/callback codes. The caller renders prompts, chooses whether
to open a browser, and cancels by dropping its login future. No process-global
signal handler, stdin reader, opener or output writer is installed by AI.
The frontend owns its branded callback page; the library has no frontend assets.

Keep the store/catalog and the Tokio runtime alive while requests are active.
A catalog binding pins the account and plan; logout or replacement requires
reloading/rebinding, not silent credential substitution. Already-dispatched
requests are not remotely revoked by local logout.

Refresh rechecks credentials inside the process/store locks. File-lock contention
is bounded at three seconds; token HTTP work is bounded at sixty seconds. A
started refresh owns rotation and persistence independently of the inference
waiter's cancellation. Dropping a waiter before refresh starts does not authorize
a later exchange. Maintain the runtime through that bounded work before
shutdown; runtime/process termination can still lose a rotated token. Persistence
failure is a resolution failure, not permission to retry an ambiguous exchange.
There is no remote revocation/rollback or process-crash recovery promise.

The AI client does not inherit the agent's retry/replacement policy. Nonzero
request retry overrides are rejected. Preferred WebSocket setup may fall back
to HTTP only on qualified pre-generation failures. Use
`AiClient::track_request_dispatch()` per attempt and retain
`request_may_have_been_sent()` when cancelling: it is conservative possible-
dispatch evidence, **not** proof of delivery, cancellation, zero usage or safe
replay. Errors redact resolved credentials and never intentionally expose token
response bodies. Keep application logs equally credential-safe.

## Independent acceptance fixture

```sh
cargo run --manifest-path examples/provider-only/Cargo.toml --locked
cargo test --manifest-path examples/provider-only/Cargo.toml --locked
cargo tree --manifest-path examples/provider-only/Cargo.toml --locked --prefix none
```

The standalone, non-published crate's only Octet dependency is `octet-ai`. It
creates synthetic credentials under a temporary private root, uses the shared
file/cache adapter, selects one exact account inventory model and performs one
loopback Responses inference with the shared resolver. Only its fixture endpoint
and HTTP transport are overridden; Codex auth, runtime/compression, headers,
capabilities and pricing stay shared. It has no live mode or real credential
import. Tests cover terminal completion/refusal/truncation, missing terminal,
ambiguous disconnect, timeout/cancellation without replay, redirected/invalid
destinations and echoed-access-token redaction. These are fixture results, not
live OAuth, account availability, model-quality or full release qualification.

## Migration scope

The frontend now delegates Codex login/logout, inventory decoding/cache and route
composition to these same modules. API-key/environment setup, other subscription
frontends, application readiness, picker presentation, defaults and session
orchestration keep their existing owners. Existing `octet-ai` codecs and dynamic
resolver contracts remain the inference surface; no second provider package is
introduced. Public tool-definition, schema-validation and extension-host APIs
are unchanged. The frontend retains its public `CodexContextOverride::publish`
entry point in a frontend-owned configuration type; only pure budget resolution
is delegated into AI, which never publishes process environment settings.
The agent's existing `secure_fs` path re-exports the shared
filesystem primitive, preserving its public source API.

## Ultra's downstream boundary

Ultra may separately depend on `octet-agent` for its tools and extension
infrastructure, but must not run its agent loop or call `Agent::prompt`.
That independent dependency does not permit a reverse dependency from
`octet-ai`, or require the coding frontend, bootstrap or terminal initialization.
The provider-only fixture remains an `octet-ai`-only consumer. There is no blanket
tool/extension extraction or public infrastructure API migration in this work.

Ultra retains its one reducer, durable writer, deterministic scheduler,
approvals, budgets and settlement authority. Jev/TypeSafe remains its first
decision step; synthesis remains proposal-only. Provider responses, including tool
calls, are data for the caller, not authority to execute or continue a turn.
Delivering this prerequisite does not resume Ultra or update its dependency:
its owner must separately authorize integration and any live OAuth/inference
qualification. Renderer work and separate releases are not prerequisites.

The [tool/extension module dependency map](maintainers/agent-feature-boundary.md)
records the compilation coupling being separated for independent hosts.

See the [ownership map](maintainers/provider-boundary.md) and
[AI architecture](design/octet-ai.md) for the extraction and transport contracts.
