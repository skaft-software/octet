# octet Extension API 0.4 Reference

**Hand-maintained reference for the feature-negotiated API `0.4` wire.** This
file is not generated from the API `0.3` schema. Update it alongside the host,
SDK and tests when a 0.4 contract changes. The [protocol reference](PROTOCOL-REFERENCE.md)
contains detailed method payloads (including older examples of shared services);
the [authoring guide](../extensions.md) covers manifests, trust and activation.
Actual host offers and frontend bindings determine service availability. Source
SDK availability is not a registry-publication promise.

## Version policy

The current host retains these runtime and bundle version gates:

| API | Wire | Runtime eligible | Bundle eligible |
| --- | --- | --- | --- |
| `0.1` | frozen text-oriented JSON-RPC | yes | no |
| `0.2` | feature-negotiated JSON-RPC | yes | yes |
| `0.3` | distinct canonical JSON-RPC | yes | yes |
| `0.4` | current feature-negotiated JSON-RPC | yes | yes |

Declare `api_version = "0.4"` for new extensions. Initialization must match that
exact version; neither a manifest retag nor a package version translates a wire.
Distributed bundles also require an exact `requires_octet` host pin. Trust,
enablement and process policy remain separate gates. Executable extensions run
with the user's OS authority, not in an OS sandbox.

API `0.3`'s live schema (`protocol/extension-api-v0.3.schema.json`),
[generated bindings](../../sdk/typescript/README.md#retained-canonical-api-03-bindings)
and conformance fixtures remain separate. Their canonical offer/selection,
method table and event bus are **not** the 0.4 handshake or feature list.

<a id="canonical-framing-and-json-rpc-envelopes"></a>
<a id="jsonrpcrequest"></a>
<a id="jsonrpcnotification"></a>
<a id="jsonrpcsuccessresponse"></a>
<a id="jsonrpcerrorresponse"></a>

## Framing and JSON-RPC envelopes

Use UTF-8 JSON-RPC 2.0, one JSON object followed by LF per message, over
stdin/stdout. Stdout is protocol-only; put bounded human diagnostics on stderr.
A single writer must serialize complete frames without interleaving or abandoning
partial writes. Requests carry `id`, `method` and method-specific `params`;
notifications omit `id`. Responses echo the request ID and contain exactly one
of `result` or `error`. Host request IDs are unsigned integers; reverse-request
IDs may also be nonempty strings of at most 256 UTF-8 bytes.

API `0.4` does **not** require canonical key order, canonical whitespace/escapes,
or the canonical 0.3 offer, parser or framing. JSON validity, closed records,
numeric/depth bounds and error vocabularies are checked at their respective
method boundaries, not imposed by a global generated 0.3 envelope contract.
In particular, portable-integer checks on provider/service data must not be
mistaken for a ban on fractional numbers in every 0.4 payload.

The ordinary host transport defaults to a 1 MiB **whole-line** cap including LF.
The paired session snapshot transport additionally advertises its own encoded
payload-byte profile (excluding LF); obey both that profile and the physical
line cap. It does not install 0.3's negotiated canonical frame bound. See
[transport defaults](PROTOCOL-REFERENCE.md#transport-defaults) and
[session transport negotiation](PROTOCOL-REFERENCE.md#11-initialize) for the
actual offered profile; implementation sources are listed under
[ownership](#ownership-and-conformance).

## Initialization

### `InitializeRequest`

The first host request is `initialize`. Its params contain:

| Field | 0.4 meaning |
| --- | --- |
| `api_version` | Exactly `"0.4"`. |
| `octet_version` | Running host version, independent of the API number. |
| `extension` | `{name, version, manifest_path, source}`; source is `global`, `project` or `explicit`. |
| `workspace` | Active workspace path. |
| `capabilities`, `contributes` | Manifest declarations, not a process sandbox. |
| `host` | Initial session/model/reasoning/active-skill state and available additive host facts. |
| `flag_values` | Host-resolved declared CLI values; omitted when there are no flags. |
| `protocol` | Required 0.4 feature/limit offer described below. |

### `InitializeFlagValue`

Each flag value is `{ "name": "flag-name", "value": true }`, with the actual
boolean, portable signed integer, or bounded string selected by the manifest's
flag type. There are at most 64 declarations/values. Values are resolved by the
host before launch; extensions do not reparse CLI arguments. No-flags omission,
empty arrays and explicit null are not interchangeable. The data record uses
closed fields and preserves its JSON value; admission applies the declared type,
integer and string bounds. See [CLI flags](../extensions.md#api-04-cli-flags).

<a id="contractoffer"></a>

### Feature offer (not `ContractOffer`)

`protocol` has `version: "0.4"`, `required_features`, `optional_features` and
`limits`. The foundation requires `request_cancellation` and `content_parts`.
Other requirements depend on declarations and the bound host. For example:

```json
{
  "version": "0.4",
  "required_features": ["request_cancellation", "content_parts"],
  "optional_features": ["request_progress", "dynamic_tools", "lifecycle_events"],
  "limits": {"max_concurrent_requests": 64}
}
```

This is an illustrative subset, **not** the complete offer to echo blindly.
`limits` may also include `max_message_bytes` and `resource_refs_v1` registry
bounds. Paired session features carry `session_snapshot_transport_v1`; configured
bulk storage may add `bulk_objects_v1` transport context. These are host-supplied
profiles, not extension-selected filesystem paths or model-visible values.

<a id="contractselection"></a>
<a id="initializeresponse"></a>

### `InitializeResponse` and feature selection

The result contains `api_version: "0.4"`, tool/command definitions and `protocol`:

```json
{
  "api_version": "0.4",
  "tools": [],
  "commands": [],
  "protocol": {
    "version": "0.4",
    "features": ["request_cancellation", "content_parts"],
    "limits": {"max_concurrent_requests": 4}
  }
}
```

Both version strings must match the manifest. Select all required features and
only supported offered optional features, without duplicates; obey dependencies.
Concurrency must be positive and is capped by the host. Optional renderer and
shortcut catalogs must match their declarations/negotiated discovery rules.
Without dynamic discovery the tool and command names must exactly match the
manifest, with no duplicates. `dynamic_tools` makes initialize's tools epoch
zero; `runtime_commands` permits a generation-fixed initialize command catalog.
Live `commands/register` additionally requires `runtime_command_registration_v1`.

`lifecycle_events` optionally selects exact lifecycle method names. An absent or
empty subscription selects all six observations; a nonempty list requires the
feature. See [initialization validation](PROTOCOL-REFERENCE.md#11-initialize)
for schemas, dependent features and paired-profile validation. Do not send a
canonical 0.3 `contract`, schema ID, encoding, capabilities or method selection.

## Capabilities

Manifest capabilities describe reviewed consent and contributions. Protocol
features select implemented host services. Neither is the canonical 0.3
capability table, nor authority to manufacture a resource owner. Optional
services require their actual host binding and, where applicable, a manifest
declaration; do not infer availability from this document alone.

Examples include `request_progress`, `artifacts`, `dynamic_tools`,
`runtime_commands`, `session_entries`, `session_control_v1`,
`session_compaction_v1`, `resource_refs_v1`, `operation_descriptors_v1`,
`bulk_objects_v1`, `tool_composition_v1`, `agent_sessions`, `provider_proxy_v1`
and `provider_inheritance_v1`. Each has its own dependency/ownership rules.
`event_bus`, `migration.adapter.v1` and canonical `theme_selection` are not 0.4
feature aliases. See [detailed services](PROTOCOL-REFERENCE.md#2-extension-to-host-messages).

## Methods and terminal semantics

| Direction | Methods / family | Contract |
| --- | --- | --- |
| Host → extension | `initialize` | Exact version and feature negotiation before registration. |
| Host → extension | `tool/call`, `command/execute`, `hook/run` | Typed result, scoped context and cancellation. |
| Host → extension | `context/collect`, `status/collect`, `menu/collect`, `tool/render` | Declared, bounded semantic contributions; actual frontend availability varies. |
| Host → extension | `shutdown`, `$/cancelRequest`, negotiated lifecycle notifications | Bounded cleanup; cancellation is not rollback or replay permission. |
| Extension → host | `notification`, `confirmation/request`, `input/request`, `$/progress` | Declaration/feature checks and parent correlation where required. |
| Extension → host | `tools/register`, `tools/unregister`, `commands/register` | Negotiated live catalogs; changes affect the next request boundary. |
| Extension → host | `session/*`, `model/select` | Bound active-session driver, owner fences and service-specific params. |
| Extension → host | `agent/*` | Extension-owned child conversations, distinct from active-session control. |
| Both | Provider registration/auth and stream messages | See [provider records](#provider-records); not a generic proxy for host credentials. |

This table is an index, not a closed generated method registry. Detailed
[host methods](PROTOCOL-REFERENCE.md#1-host-to-extension-methods) and
[reverse methods](PROTOCOL-REFERENCE.md#2-extension-to-host-messages) also cover
presentation, artifacts, composer, policy, secrets, terminal handoff,
autocomplete, native resources, bulk data and composition.

A `parent_request_id` names a host-issued request, never an arbitrary session.
A `resource_owner` is the exact host-issued `{session_id,
extension_instance_id, process_generation}` triple. Settlement, cancellation,
reload and generation replacement revoke the relevant authority. Independently
retained callbacks are admitted only by services that explicitly support them;
0.3's unparented session-lifecycle exception does not apply to 0.4.

### `ToolDefinition`

`name`, nonempty `description` and JSON-Schema-object `parameters` define a tool.
An optional `output_schema` enables typed structured results. Additive operation,
resource and prompt metadata require their corresponding selected features. See
[tool calls](PROTOCOL-REFERENCE.md#12-toolcall) and
[composition](PROTOCOL-REFERENCE.md#225-compositioncontext-compositioncall-compositionstore-api-04-feature-tool_composition_v1).

### `ToolCallParams`

`{name, arguments, context}` includes the host's execution metadata and issued
owner when available. Dynamic catalogs also supply `catalog_revision` so the
handler is the one frozen for that model request. Arguments do not supply owner
authority or change a request's already selected schema.

### `ContentPart`

Typed content includes text, verified artifact-backed image and audio parts.
At least one explicit text part is required. Media must match the admitted
artifact's owner, generation, MIME and kind; see [media/result validation](PROTOCOL-REFERENCE.md#12-toolcall).

### `ToolCallResult`

`{content, is_error, metadata?, structured_content?}` retains ordered typed parts.
Successful schema-declared output requires `structured_content`; without an
output schema it is forbidden. Explicit null is a value validated against that
schema, not absence. Metadata remains non-model-visible. Structured data is
lowered to the model only under explicit host policy. Bounds and errors are not
silently converted into successful text.

### `CancelRequestParams`

`$/cancelRequest` is a notification with `{id, reason?}` for a pending request.
Before write it may suppress a queued frame; after write it is cooperative.
Parent settlement cancels correlated children; late replies are tombstoned.
See [cancellation](PROTOCOL-REFERENCE.md#19-cancelrequest-api-02).

### `CancelRequestResult`

Do not expect a response to the cancellation notification. The original request
settles once; cancellation cannot undo already committed external effects.

### `ShutdownParams`

Host request `shutdown` has `{}` params, after bounded draining/cancellation.

### `ShutdownResult`

Acknowledge with an ordinary result object (for example `{}`), then exit promptly.
The host has separate bounded acknowledgement and process-exit stages, followed
by process-group termination. Stdin EOF is transport loss/final teardown, not
the graceful shutdown signal. See [shutdown stages](PROTOCOL-REFERENCE.md#18-shutdown).

## Session hooks and control

### `SessionBinding`

`{session_id, extension_instance_id, process_generation}` contains opaque owner
and instance identifiers (at most 256 bytes each) and a portable unsigned
generation. It is not a session path or mutable session object. The host further
screens hook identifiers before constructing this record.

### `SessionStart`

`hook/run` with `hook: "session_start"` carries `payload.binding` plus the 0.4
start `payload.reason` (`startup` or `reload`) and an ordinary execution `context`
with its resource owner and offered session mirror. Declare the hook and select
its offered `lifecycle_events` feature. Unlike 0.3, 0.4 can declare start/end
independently and does not select a canonical `hook/run` method set.

### `SessionEnd`

`hook/run` with `hook: "session_end"` carries `payload.binding`, `outcome`,
`reason` (`shutdown`, `reload`, `crash` or `cancelled`) and `duration_ms`, plus
0.4 context. Outcomes are `completed`, `failed`, `cancelled`, `interrupted`,
`frontend_disconnected`, `shutdown` or `limit_reached`. Starts use the ordinary
request deadline; end dispatch is capped at 250 ms. Failures are diagnostic and
cannot veto lifecycle ownership. Replacement/crash fencing preserves the old
binding generation before starting the replacement.

### `SessionHookParams`

The common typed prefix is `{hook, payload}`. The host validates that prefix
before adding 0.4 reason/context/mirror fields; it is not the complete 0.4 wire
record by itself.

### `SessionHookResult`

Use the ordinary 0.4 hook result (an empty object is a valid observation). Do not
require 0.3's `disposition` wrapper. See [hook/run](PROTOCOL-REFERENCE.md#14-hookrun).

### `SessionCreateParams`

### `SessionForkParams`

### `SessionSwitchParams`

### `SessionReloadParams`

The `session_control_v1` service uses 0.4 closed requests with required
`parent_request_id`, optional/null issued `resource_owner`, and operation-specific
fields. `session/switch` requires a session ID of at most 256 UTF-8 bytes, not an
arbitrary path. Create/fork/reload/wait-for-idle are distinct operations; current
create setup/parent and fork position/entry options are defined by the
session-control implementation (`crates/octet-agent/src/extension_process/session_control.rs`)
and replacement driver (`crates/octet-agent/src/extension_process/session_control/replacement.rs`).
See [active-session control](PROTOCOL-REFERENCE.md#221b-active-session-control-api-04-feature-session_control_v1)
and the separate [compaction service](PROTOCOL-REFERENCE.md#session-compaction-v1).
Do not copy the canonical empty 0.3 create/fork/reload request models.

### `SessionLifecycleResult`

Ordinary lifecycle success identifies the active/durable session with
`{session_id}`. Setup/replacement services may attach their own snapshot facts.
Queue admission is not success: responses follow actual consumer completion.
Idle-boundary mutations must not be awaited by a handler whose settlement is
needed to reach that boundary. Cancellation does not roll back a commit.

## Provider records

These records are independently maintained in the host's
`extension_api_v04` (`crates/octet-agent/src/extension_api_v04.rs`) module.
The common data validators retain their existing portable-integer, depth,
closed-field and absent/null rules. **0.4 registration additionally uses the
proxy/metadata path below**, not the canonical 0.3 registration parser.

### `ProviderAuthRequirement`

`{kind, subject?, scopes?}`: kind is `none`, `oauth` or `host_credential`; subject
is at most 64 bytes and scopes at most 32 strings. Optional fields omit absence
and reject explicit null at validation. Proxy registrations require `kind: "none"`.

### `ProviderDefinition`

`{id, label, auth}`: ID at most 64 bytes, label at most 128 UTF-8 bytes. Registry
admission additionally validates identifiers, uniqueness and owning generation.

### `ProviderModelCapabilities`

`{tools, parallel_tool_calls, structured_output, reasoning}` are required booleans.

### `ProviderModelDefinition`

`{id, api_name, protocol, context_window, max_output_tokens, capabilities,
display_name?}`: IDs/API names at most 64 bytes; display name at most 128 UTF-8
bytes, absent allowed, explicit null refused. Counts are portable unsigned
integers. The common model parser accepts `openai_chat`, `openai_responses` and
`anthropic_messages`; the **0.4 proxy path** also handles `extension_stream` and
feature-gated `google_generative_ai`, with its existing additional checks.

### `ProviderRegisterParams`

### `ProviderUpdateParams`

`providers/register` and `providers/update` carry `{provider, models}`. For 0.4,
each model must also carry validated `pi_metadata`; metadata is removed before
validating the common model record. That closed metadata includes `base_url`,
`input`, `pricing`, optional inheritance/custom-chat/reasoning facts, and refuses
caller-supplied `native_model_id`. URL, modality, pricing and protocol/inheritance
combinations are checked by the 0.4 registration path (`crates/octet-agent/src/extension_process/model_control.rs`).
Inheritance requires `provider_inheritance_v1`; Google additionally requires
`provider_google_generative_ai_v1`. Neither registration nor an extension-owned
stream grants credential/OAuth authority. Updates replace the owned catalog and
invalidate stale routes; they do not mutate a frozen in-flight model request.

### `ProviderUnregisterParams`

`providers/unregister` takes `{provider_id}` (at most 64 bytes), checked against
the owning registration generation. It does not unregister another process's provider.

### `ProviderCatalogCompleteParams`

`providers/complete` is an `{}` notification marking initial catalog completion;
it does not silently change the first already admitted model request.

### `ProviderCatalogResult`

`{revision, provider_ids, model_ids}` is the authoritative admitted catalog
result. Revision is portable unsigned; lists and IDs have the bounds below.

### `ProviderStreamRequest`

Host request `provider/stream` carries `{stream_id, provider_id, model_id,
request, authorization_lease?}`. The optional lease is opaque and request-scoped,
not a credential. Omission is not explicit null. Request JSON retains the
service's portable-number/depth checks.

### `ProviderStreamAccepted`

`{stream_id, accepted, reason?}` acknowledges stream admission, not successful
completion or permission to retry an ambiguous effect.

### `ProviderStreamEvent`

`provider/event` notifications carry `{stream_id, sequence, kind, payload}`.
Sequence starts at zero and must be contiguous. At most 100,000 events are
accepted; an encoded payload is at most 65,536 bytes. Kind/payload are decoded by
the stream implementation (`crates/octet-agent/src/extension_process/provider_stream.rs`).
Terminal state, owner retirement and bounded ingress pressure settle/cancel the
route; missing finish is not a successful completion.

### `ProviderStreamCancelParams`

`provider/cancel` takes `{stream_id, reason?}`. Reasons are at most 4,096 UTF-8
bytes. Cancellation is not rollback and does not make unsafe replay safe.

### `ProviderAuthorizationRequest`

`provider/auth/request` / `provider/auth/revoke` use `{provider_id, action,
interactive, scopes?}`. Request actions are `authorize`/`refresh`; revoke requires
`revoke`. The host policy owns UI, credentials and storage; unavailable policy
cannot be replaced by an extension assertion.

### `ProviderAuthorizationResult`

`{status, lease?}`: status is `ready`, `pending`, `denied`, `unavailable` or
`revoked`. Optional leases are at most 256 bytes and never retained in registry
snapshots. Explicit null is refused by the common validator.

## Errors

### `ErrorObject`

JSON-RPC errors carry `{code, message, data?}`. Absent `data` is omitted; explicit
null remains null. There is **no single fixed code/message table for every 0.4
service**. SDK standard errors, domain failures, typed host refusals and provider
errors retain their own existing vocabularies. Do not validate all 0.4 errors
against the canonical 0.3 table.

Shared provider and legacy session-response helpers use these exact pairs:

| Semantic | Code | Exact message |
| --- | ---: | --- |
| `parse_error` | -32700 | `parse error` |
| `invalid_request` | -32600 | `invalid request` |
| `unknown_method` | -32601 | `unknown or unnegotiated method` |
| `invalid_params` | -32602 | `invalid params` |
| `internal_error` | -32603 | `internal error` |
| `version_mismatch` | -32010 | `extension API version mismatch` |
| `capability_mismatch` | -32011 | `extension capability mismatch` |
| `resource_exhausted` | -32012 | `extension resource exhausted` |
| `request_cancelled` | -32800 | `request cancelled` |

Provider host replies map invalid/conflict to `invalid_params`, unavailable to
`capability_mismatch`, and exhausted to `resource_exhausted`. Method-specific
0.4 typed refusals instead include `unsupported_feature` (-32601),
`invalid_request` / `bounds_exceeded` (-32602) and `not_foreground_owner` (-32002),
with bounded detail. See [standard and typed errors](PROTOCOL-REFERENCE.md#3-standard-json-rpc-errors).
Historical diagnostic text in the independently owned validators is preserved;
its mention of 0.3 is not a change to 0.4 wire selection.

<a id="protocollimits"></a>

## Bounds

These are existing defaults/data bounds, not a canonical negotiated 0.3 table.
Configured hosts and selected service profiles may impose tighter limits. String
bounds below count UTF-8 bytes; encoded JSON bounds count serialized bytes.

| Item | Bound/default |
| --- | ---: |
| Ordinary physical message including LF | 1,048,576 bytes |
| Host concurrent requests / complete writer queue | 64 / 128 |
| Ordinary request / tool-call timeout | 30 s / 10 min |
| Cancellation grace / cancelled-ID retention | 2 s / 30 s (512 IDs) |
| Child requests / child workers | 128 / 8 |
| Tool catalog / command catalog / CLI flags | 256 / 256 / 64 |
| CLI string value | 4,096 bytes |
| Reverse string ID / session-control ID / hook identifier | 256 bytes |
| Provider registry / model registry or batch | 32 / 256 |
| Provider/model/API name / display label | 64 / 128 bytes |
| Provider auth scopes / lease | 32 strings / 256 bytes |
| Catalog provider IDs / model IDs | 32 × 64 / 256 × 128 bytes |
| Stream ID / stream reason | 256 / 4,096 bytes |
| Encoded stream event payload / events | 65,536 bytes / 100,000 |
| Stream ingress buffer / idle timeout / overall deadline | 32 events / 30 s / 5 min |
| Portable service integer magnitude / nesting depth | 9,007,199,254,740,991 / 32 |
| Result content parts / referenced media | 256 / 64 MiB |
| Structured result / metadata | 256 KiB / 64 KiB |

Service-specific resource, UI, schema, artifact and snapshot profiles are in the
[protocol reference](PROTOCOL-REFERENCE.md); do not replace those checks with
these common data limits.

## Optional API 0.4 cache-warming advice

Declare `cache_warming_decision` and select its offered feature. `hook/run`
carries `{decision, model}` and owner-fenced context, never prompt/credentials.
Return `cache_warming_decision: "warm" | "stop" | null` or omit it. Hooks run in
registration order; the last returned action wins. Malformed/failed/stale/late
advice preserves the preceding decision. Each wait is capped at 200 ms and an
aggregate deadline still applies. Advice cannot change eligibility, spending or
attempt budgets, cancellation or replay safety. See [full advice fields and
rules](../extensions.md#cache-warming-decision-advice-api-04).

<a id="generated-artifacts-and-conformance"></a>

## Ownership and conformance

The hand-maintained sources are
`extension_api_v04.rs` (`crates/octet-agent/src/extension_api_v04.rs`)
(common flag/provider/session/error data),
`extension_api_policy.rs` (`crates/octet-agent/src/extension_api_policy.rs`)
(current run/install eligibility), and the native process
initialize records (`crates/octet-agent/src/extension_process/process_state.rs`),
feature records (`crates/octet-agent/src/extension_process/protocol.rs`),
negotiation (`crates/octet-agent/src/extension_process/negotiation.rs`),
framing (`crates/octet-agent/src/extension_process/connection.rs`) and
dispatch (`crates/octet-agent/src/extension_process/protocol_line.rs`).
The current [Python](../../sdk/python/README.md) and
[TypeScript process SDK](../../sdk/typescript/process/README.md) implement this
feature-negotiated wire; they are not the generated canonical bindings.

`extension_api_v04_owned_contract` (`crates/octet-agent/tests/extension_api_v04_owned_contract.rs`)
compares pre-split serialization, validation, absent/null, bounds, bytes and
exact errors against the still-live old definitions, with independent literal
0.4 assertions to retain after the old oracle is removed.
The generator-ownership tests (`scripts/tests/test_extension_api_v04_reference.py`)
exercise generation and drift-checking without touching this file. Continue to
check live 0.3 outputs with `python3 scripts/generate-extension-api-v03.py --check`;
that command no longer owns this reference.
