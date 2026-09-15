# Mistral Conversations current-candidate status

**Source candidate, not build-qualified or accepted. #245 remains incomplete.**
The native request/SSE codec and deterministic fixtures now exist; this is no
longer a reserved rejection-only protocol. Native presets, discovery and host
auth integration still require implementation/qualification before exposure.
No live endpoint, installed-runtime or full provider-parity claim is made.

## Pinned evidence

The preserved complete official `mistralai/client-python` source snapshot is
commit `3653cd9a5169fc151a0787232aed70eeea88e52a`. The earlier partial reference
packet remains preserved, but no longer describes the available evidence. No
SDK installation or execution was needed for this source review. Paths below
are relative to the complete pinned SDK snapshot, not the Octet repository.

| Evidence | Contract used |
| --- | --- |
| `src/mistralai/client/conversations.py` | Streaming POST `/v1/conversations#stream`, `Accept: text/event-stream`. The fragment is not sent in the HTTP target. |
| `src/mistralai/client/models/conversationstreamrequest.py` | Native `inputs`, `model`, `instructions`, `tools`, `completion_args`, `store` and `handoff_execution`; not a Chat `messages` envelope. |
| `src/mistralai/client/models/completionargs.py` | Sampling, stop, output-format and enum tool-choice fields. The SDK also exposes `reasoning_effort`; this candidate does not implement it. |
| `src/mistralai/client/models/conversationevents.py` | Ten discriminated native event variants. SDK-local `function.result` is not a server SSE event. |
| `src/mistralai/client/models/messageoutputevent.py` and `functioncallevent.py` in the same directory | Message/content indices, function entry/call identity and argument fragments. |
| `src/mistralai/client/models/responsedoneevent.py` and `conversationusageinfo.py` in the same directory | Required terminal usage object, default-zero model counters and nullable connector usage fields. |
| `src/mistralai/extra/run/result.py` | SDK event accumulation reference, not execution authority for Octet tools. |

## Candidate behavior

- Requests use a new non-stored conversation with `handoff_execution: "client"`.
  History is replayed as native message/function entries. Cache session IDs are
  never reinterpreted as provider conversation/continuation IDs.
- Function definitions, enum tool choices, sampling controls and JSON output
  formats use native fields. Tool-call IDs and argument fragments remain exact;
  the shared history repair supplies interrupted calls' missing error results.
- Only native started/delta/done/error events drive settlement. Interleaved text
  and function entries have stable identities; text chunks within one native
  message remain together in response/history despite intervening function deltas.
  All completed argument buffers
  must be JSON objects before any ToolCallEnd is exposed; schema mismatches
  remain canonical argument errors, not permission to execute.
- Only native `conversation.response.done` finishes successfully. EOF, `[DONE]`,
  malformed/foreign events, changed or duplicate function identities and native
  errors cannot manufacture a successful response or completed tool call.
- Native SSE errors use fixed safe prose; HTTP errors retain the shared bounded,
  credential-redacted status/request-ID path. The client does not replay a POST.
  Stream drop uses the shared response-body lifetime; its native fixture is unrun.
- Native model counters remain separate from cache/reasoning/connector usage.
  Missing model counters default to zero; malformed or explicit null model
  counters fail rather than being represented as missing usage. Nullable
  connector fields do not manufacture model/cache counters.
- Telemetry/RPC preserve native protocol identity. OpenRouter Batch's default
  route selection rejects this protocol. Canonical API 0.3 extension-provider
  declarations still permit only their existing three generic protocols; this
  native host codec does not enlarge that schema's authority.

## Unsupported and intentionally lossy behavior

These are explicit candidate limitations, not claims that the upstream API
lacks these features. They do not close the parent issue or narrow the roadmap.

- Non-Off reasoning controls, non-Standard reasoning mode and forced named
  function choice are rejected before credentials/dispatch in both modes.
  Catalog/bootstrap discovery must not advertise unsupported reasoning controls.
- Input/assistant media, tool-result media, replayed reasoning/provider state
  and audio output reject in Strict mode; Lossy omits them with diagnostics,
  without Chat-style placeholder text or inferred reasoning state.
- Server tool execution, agent handoffs and non-text output reject in Strict
  mode. Lossy omits them with diagnostics; they never become local function
  calls, reasoning text, media delivery or host execution authority.
- New native entries must first appear in increasing `output_index` order.
  Deltas for already-known entries may interleave. An out-of-order new entry
  fails in both modes before any ToolCallEnd, rather than silently changing
  native tool-effect ordering to the canonical stream's first-observation order.
- Non-null function confirmation status remains unsupported in both modes,
  including `allowed`: provider confirmation is not host permission.
- Connector usage breakdown rejects in Strict mode. Lossy reports its omission
  while retaining the native total and model input/output counts, without
  relabeling connector tokens as model output or cache/reasoning usage.

## Tests and independent remaining gates

`crates/octet-ai/tests/mistral_current.rs` now contains native request, text,
interleaved function/replay, usage, malformed/terminal/error, credential-redaction,
destination, Strict/Lossy and drop-cancellation fixtures. Caller regressions also
cover API 0.3 native-protocol rejection and bootstrap reasoning metadata.
Auth in these fixtures is a loopback-only test credential, not qualification of
`MISTRAL_API_KEY`, stored credentials, discovery or a real provider account.

**All candidate Rust formatting, compilation and test execution are UNRUN.**
The sole Rust producer is blocked by Temper access. Source inspection/import,
authored fixtures and historical checks are not acceptance of this candidate.
After access returns, admit immutable inputs through the shared build slot,
using the existing locked dependencies/target and bounded receipt-producing
runner. Required checks include:

```text
cargo test --locked -p octet-ai --test mistral_current
cargo test --locked -p octet-ai --lib
cargo test --locked -p octet-agent --lib extension_provider_protocols_reject_native_unmodeled_routes
cargo test --locked -p octet-coding-agent --lib mistral_conversations_discovery_does_not_invent_reasoning_controls
cargo check --locked --workspace --all-targets
```

Formatting also requires explicit slot admission. Native model presets,
discovery/picker availability and environment/credential-store host wiring must
be reconciled and tested before enabling native models. Live provider, broader
workspace/feature, final review and release acceptance remain separate gates.
