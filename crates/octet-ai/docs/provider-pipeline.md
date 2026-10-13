# Async provider pipeline — integration contract

Working-tree implementation, not release qualification. The canonical
`ProviderContextHook` / synchronous `SessionLeaf` service is unchanged. Encoded
payload hooks are a **separate** boundary: no Pi type or high-level context
approximation is introduced into octet-ai.

## Shared glue required by the integration owner

The native lane implements all direct modules below. Shared files are deliberately
left to their owner:

1. Add API-0.4-only `ExtensionHook::{BeforeProviderRequest,
   BeforeProviderHeaders,AfterProviderResponse}` (snake-case wire names), offer
   and require optional `pipeline_hooks_v1` when those hooks are declared and a
   consumer is configured. Do not offer on canonical 0.3 or legacy 0.1/0.2.
2. Add `ExtensionHost.provider_request_hooks:
   Vec<Arc<dyn crate::extension_provider::ProviderRequestHookFactory>>`, initialize
   empty, clone in the existing scoped host constructor, and register through
   `provider_request_hook(factory)`. In process `Extension::register`, use:
   `if self.has_provider_pipeline_hooks() { host.provider_request_hook(self.clone()); }`.
3. Bind the run's local client (not the shared client/catalog) before main and
   auxiliary provider dispatch:
   `provider_context::provider_request_client(&client,
   &extension_host.provider_request_hooks, session.resource_owner_key().as_str())?`.
   The helper returns `Result<AiClient,AgentError>`; adapt the local error path to
   the run's existing terminal handling. Disable native steering admission for
   nonempty provider-request hooks, as for existing context transformations.
   The client explicitly refuses native steering/opaque host transports rather
   than inventing wire events. Preferred ordinary Responses uses HTTP, and
   prewarming is skipped, when this pipeline is installed.
4. No change to `ExtensionHookOutput` is needed for encoded hooks: the native
   process adapter deserializes the private, strict reply below directly over
   existing `hook/run` / cancellation. No new RPC service or queue is added.

## Adapter wire

`hook/run.params` has existing `hook`, `context` with the real resource owner,
and `payload`. A single host-generated `operation_id` correlates the three phases;
a later attempt gets a different ID. Model is the secret-free
`{id,provider,api}` (native API IDs, e.g. `openai-chat`).

- `before_provider_request` payload:
  `{operation_id,model,payload:<actual codec JSON>}`.
  Reply `{provider_payload:<replacement JSON object/array>}` or `{}` (unchanged).
  This is **not** canonical messages and is never mapped to provider_context.
- `before_provider_headers` payload:
  `{operation_id,model,headers:{name:string|string[]}}`, before authoritative
  authentication/signing. Reply
  `{provider_headers:{name:string|string[]|null}}`. The reply is a patch:
  omitted names stay unchanged; null deletes; string/array replaces all values.
  In-place JS mutations must be returned explicitly, including deleted keys as
  null. Empty arrays, case-insensitive duplicate names, invalid header values,
  excessive headers, and reserved header mutation/deletion fail before send.
- `after_provider_response` payload:
  `{operation_id,model,status:<actual HTTP number>,headers:{name:string|string[]}}`.
  Reply `{}`. Awaited at real header arrival, before body consumption, including
  non-2xx. No event is fabricated at stream completion or for opaque SDK calls.

All replies may include `disposition:{action:"continue"}` (default) or a deny
which fails the attempt. Unknown fields and cross-phase transformations fail.
Normal adapter handler exceptions may be diagnosed locally according to Pi's
profile; native transport/deadline/validation failure never silently falls back.
Wire material is private: no payloads/headers/remote exceptions in ordinary
errors, logs, session records or UI. Existing frame bounds remain authoritative;
large session/body hooks fail visibly, never truncate.

## Canonical context snapshot

`ProcessContextWait` now calls the existing
`SessionLeafProcessLease::with_session_snapshot(session)` before dispatch. Thus
`context.host` carries authoritative native `session_entries`, `session_branch`,
`session_leaf_id`, `session_file`; it also sets `session_id` from the preparation
context. Adapter translates those native Entry records locally, without native
Pi dependencies or fabricated timestamps/usage. The grant remains top-level
`session_leaf`; it is never sent to the model/provider. The session snapshot
helper is owned by the session lane; its metadata visibility must retain only
public metadata and the receiving extension's own private state.

## Safety and remaining boundaries

The async chain is inert when absent. Each phase is capped by the smaller of
endpoint timeout and five seconds; dropping the future cancels pending RPC.
Existing synchronous hooks still work and precede the async hook of the same
phase. Reserved checks include deletion and every repeated header value.
Auth/signing occurs after mutation. Compact HTTP uses the same async phases.
Image requests refuse per-request async hooks; batch/image/deferred operations
are not covered by client-bound conversational hooks.

Extension provider streams retain the existing registry and ordered events.
Their transports own actual encoded/SDK callbacks; a host canonical request is
not evidence of an encoded body or HTTP response. API-0.4 provider catalog and
stream binding and adapter-local callback routing require the separate
`provider_proxy_v1` feature integration, not widening canonical API 0.3.

Focused tests are in `tests/runtime_hooks.rs` and the existing agent process
provider_context tests. Cargo/build execution belongs to the integration owner;
this lane does not claim unrun tests passed.
