# octet AI design

## Canonical model

`octet-ai` exposes provider-independent `Request`, `Message`, `AssistantPart`, `Usage`, `Response`, and `StreamEvent` types. Protocol codecs translate these values to and from OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, Amazon Bedrock Converse, and Google `generateContent`. Provider DTOs do not cross the crate boundary.

## Capability and reasoning model

`Capabilities` keeps transport and model facts explicit. `responses_lite` selects
a Responses wire contract, while `agent_delegation` records a collaboration
protocol the model advertises; it does not imply that `octet-ai` owns or can run
an agent team. `ReasoningEffort::Ultra` is ordered above `Max`. OpenAI Responses
requests backed by V2 delegation map Ultra to the model effort `"max"`; the
coding product supplies the delegation half only through its observing
`octet-subagents` extension. Explicit Ultra requests without V2 are rejected;
product selection may normalize persisted choices to a supported ordinary tier.

Optional `ReasoningCapability.options` preserves exact endpoint values and an
advertised default, including gaps and whether Off exists. Discovery distinguishes
absent, unknown and explicit metadata; explicit false suppresses route fallbacks.
Malformed values/defaults are not interpreted as a broader range. Typed wire
profiles remain endpoint-specific. See [provider thinking](../provider-thinking.md).

`ReasoningMode::Pro` remains deserializable only for older callers and persisted
sessions. Protocol validation rejects it in strict mode (or reports
`ignored_reasoning_mode` in lossy mode), and no codec serializes a
`reasoning.mode` field. The product layer must migrate legacy Pro state only
after it has both model metadata and a live, trusted `octet-subagents` observer.

## Google generateContent

Gemini Developer API, Vertex AI, and declared Google-compatible routes select the
same native codec. It constructs the fixed `models/<safe-api-name>:streamGenerateContent`
path under a trusted endpoint, uses Google content/function declarations and native
JSON-schema fields, and decodes SSE candidate snapshots into canonical deltas.
Google's opaque `thoughtSignature` is retained as position-bound
`ProviderMetadata` immediately before its text, reasoning, or function-call part.
It is replayed only to the same Google model; transformations to any other model
or protocol drop it.

The codec accepts only inline image bytes, never fetches a user-provided image
URL, and rejects unsupported tool-result media. Cumulative function-call argument
snapshots are merged before terminal assembly rather than concatenated as invalid
JSON. Google thought text is canonical reasoning; a thought signature alone does
not make visible content reasoning.

## Responses Lite

A model with `responses_lite = true` uses the same capability-driven contract for
ordinary Responses, opaque replay, and `POST /responses/compact`, regardless of
endpoint identity or session-affinity format:

- add `x-openai-internal-codex-responses-lite: true`;
- carry function schemas in a developer `additional_tools` input item, wrapped
  by the `functions` namespace, rather than top-level `tools`;
- carry nonempty instructions as a developer message input item rather than
  top-level `instructions`;
- emit `parallel_tool_calls: false` explicitly even when model metadata advertises
  parallel support, as required by the internal Lite route;
- include `reasoning.context: "all_turns"` alongside any advertised effort; and
- remove only `detail` from `input_image` parts in messages and function/custom
  tool outputs while retaining every other opaque field.

Public/non-Lite compact routes retain their narrower schema. Lite is never
inferred from a model name, endpoint label, or authentication plan.

## Native compact opening boundary

`AiClient::open_compact_responses` resolves credentials and sends exactly one
request, returning `PendingResponsesCompact` only after actual HTTP headers
arrive, including non-2xx headers. Its consuming `complete()` method reads the
body or optional bounded error snippet under independent body timeouts; the
absolute body deadline starts at header arrival. The existing
`compact_responses` composes these operations. Dropping either phase cancels
local work; neither phase replays a POST or rotates credentials autonomously.
A host outage deadline may therefore cover opening without cancelling a healthy
body. Header arrival ends that opening episode, not remote execution or usage.

For streaming and native compact calls, `track_request_dispatch()` creates an
attempt-local client clone with a fresh sticky dispatch marker. The marker is
set after credential resolution/signing and before HTTP send, WebSocket dispatch,
or entry into an opaque host transport. `request_may_have_been_sent()` is
conservative evidence of possible dispatch, not proof of transmission or billing.
A deadline cancelling a tracked opening must retain usage uncertainty when set;
credential-only waiting leaves it unset. A fresh clone is required per attempt;
the original client and other attempts retain independent tracking.

## Stream contract

A successful guarded stream has exactly one `Started`, balanced start/delta/end events for every indexed part, at most one usage event, and exactly one terminal `Finished`. Premature EOF, events after finish, and unbalanced parts are errors. Completed parseable tool arguments are normalized and checked against the immutable request schema snapshot before their `ToolCallEnd`: an ordinary schema mismatch remains a canonical call marked for a bounded paired error, while malformed schemas, malformed arguments, and validation-limit failures are errors. An authoritative max-token terminal is the sole malformed-argument exception: it retains only the call envelope with empty arguments so the agent can pair a non-executing error result and continue safely.

### Responses failure provenance

`AiError::ResponsesFailed(ProviderError)` identifies a native `response.failed`
terminal; arbitrary top-level provider errors remain `AiError::Provider`.
Both retain sanitized code/kind/message metadata and stream progress. Qualified
host policy can treat unknown failed terminals separately without broadening
arbitrary provider-error retry. `ProviderError::is_permanent()` shares typed
code/kind vetoes with HTTP classification, including policy/quota/context/auth
errors and pinned Codex `server_is_overloaded` / `slow_down` denials (not generic
`overloaded_error`). `HttpError::is_transient_server_error()` covers all 5xx,
including 520, with the same veto; it is solely a hint for host-qualified finite
replacement and does not broaden the legacy `is_safe_to_retry()` whitelist.

Within the Responses codec, only an unknown `response.incomplete` reason creates
`StopReason::Other`; this is sufficient provenance for qualified hosts to reject
partial completion before commit. `max_output_tokens` and `content_filter`
retain successful `MaxTokens` / `Refusal` terminals. Unqualified consumers keep
the existing unknown-incomplete terminal contract. WebSocket failed terminals
and unknown incomplete outcomes retire/disable pooled state before publication,
including text and binary frames. None of this grants automatic replay or
establishes nonacceptance, cancellation, or zero usage.

### Opt-in endpoint lifecycle feedback

`StreamEvent::ProviderLifecycle` is bounded advisory transport telemetry, not an
assistant part and never response-builder input. It is enabled only for a
streaming HTTP OpenAI Chat request whose `RequestRuntime::lifecycle_feedback`
flag is true. The client sends `x-octet-lifecycle: 1` and accepts the same
response header and SSE comments in the `octet-lifecycle:` namespace. Values are
`queued`, `loading`, or `ready`, optionally followed by `; detail`; malformed,
unknown, and ordinary comments are ignored. Details are credential-redacted and
terminal-safe before a 160-byte cap, and a stream emits at most 64 lifecycle
updates.

A header or lifecycle comment that precedes provider data causes a synthetic
`Started` first, preserving the ordinary stream invariant. Lifecycle feedback
cannot produce a `Finished`, affect assembled response content or usage, extend
the response-header timeout, or make a request replay-safe. Non-streaming
requests, WebSocket transports, and all endpoints without explicit opt-in keep
the ordinary protocol path.

The response builder enforces absolute limits before appending:

- 16 MiB per tool argument object;
- 64 MiB aggregate text, reasoning, tool identifiers/arguments, and media;
- 100,000 events;
- 1,024 indexed parts;
- protocol SSE event/body and timeout limits in the transport layer.

Bedrock ConverseStream is decoded as incremental AWS EventStream frames rather
than SSE. Both frame CRCs are verified before its bounded JSON payload is
interpreted.

Transport timeouts are phase-specific rather than one short request timer:
connection establishment remains bounded separately, `Endpoint::timeout` covers
request send and response headers, and the client defaults to a fifteen-minute
first-body allowance, a five-minute inter-chunk idle allowance, and a one-hour
overall body deadline. Optional error snippets use tighter two-second idle and
five-second overall ceilings after the HTTP status is known. A preferred
WebSocket falls back to HTTP when connection establishment fails before a
generation frame could have been sent. During an active OpenAI Responses
WebSocket generation, Ping/Pong probes run at most every fifteen seconds with
at most a ten-second acknowledgement deadline (both shorten with a configured
response-idle bound). A Pong proves only control-path liveness and never
extends the provider-event idle deadline. A missed probe retires and disables
the pooled socket before reporting a post-send body timeout, so it cannot
silently replay the generation. Fatal WebSocket failures retire/disable pooled
state before publishing the error, so an immediately authorized subsequent
request cannot reuse the poisoned socket and preferred transport can use HTTP.
This ordering does not itself authorize a replacement or prove remote
cancellation. The AI client reports post-send header/body failures rather than
silently retrying them; zero visible output does not establish nonacceptance.
The agent retains this conservative default for unqualified requests, but may
replace host-qualified Codex local-function inference before assistant commit,
even after provisional generation. The developing candidate separates finite
streamed-inference replacement and HTTP-admission budgets; neither authorizes
blanket retry or establishes the number of accepted generations or charges.
Transport fallback does not reset these logical-turn budgets. See the
[recovery boundary](../tools.md#recovery-and-security) and
[candidate qualification](../qualification/v0.7.4-recovery.md).
The coding product uses a
fifteen-minute response-header default for built-in and custom routes; custom
providers can override that startup allowance for their own cold-start profile.
Mid-stream failures retain bounded progress counters plus elapsed and
last-provider-event timing for operational diagnosis. All of these are
cancellable bounds, not a requirement to wait before cancelling a stalled request.

Observed indices use a hash set and are sorted only during final assembly, keeping hostile many-part processing near-linear.

## Validation and compatibility

Strict mode rejects unsupported modalities, reasoning state, tools, malformed schemas, missing/orphan tool results, invalid sampling parameters, and model-limit violations before network I/O. Lossy conversion emits bounded diagnostics and visible placeholders rather than silently changing semantic data. Explicit generation reasoning selections are validated without clamping, even in Lossy mode: silently omitting a rejected Off could enable provider-default thinking. Token budgets must leave answer room within the effective output allowance. Anthropic/Bedrock thinking also rejects incompatible sampling and forced tool choices. Product-level normalization is separate from core wire validation.

`AiError::NetworkUnavailable` preserves positively classified transient pre-send
connection failures, including connect timeouts, separately from generic
`Transport` errors. Unclassified DNS/TLS/certificate/configuration failures do
not authorize sustained network waiting. The agent, not the transport, owns
that cancellable recovery policy.

## Authentication and secrets

Endpoints resolve static, environment, or dynamic credentials immediately before requests. Secret values redact `Debug` and `Display`; authorization headers are marked sensitive; redirects are disabled. Transport errors and bounded response snippets are sanitized before crossing the API.

`AuthError::Unavailable` preserves positively identified transient pre-send
credential-service connection failure for host-owned network waiting. It does
not classify accepted or ambiguous OAuth token rotation as safe to repeat;
response-body or HTTP-status failures after acceptance are not autonomously
replayed. Like inference opening, this requires positive connect-timeout or
transient I/O evidence, not a generic DNS/TLS error.
The Codex resolver keeps those failures, permanent auth errors, and unclassified
resolution failures conservative; credential details do not enter diagnostics.
Recovery does not add an independent OAuth retry loop.

## Deterministic catalog

Normal builds generate display-name aliases and trusted provider pricing only
from the checked-in `models/models-dev-names.json` and
`models/models-dev-pricing.json` snapshots. They never contact the network. The
explicit maintainer script `scripts/refresh-models-dev-pricing.py` refreshes both
snapshots together and excludes provider-known dead aliases before writing them.
Pricing is provider-scoped, represented as integer microdollars per million
tokens, and is used as a fallback for discovered built-in routes; explicit
`CatalogConfig` pricing remains authoritative. Runtime discovery is a
coding-product concern and can be disabled with `--offline`/`OCTET_OFFLINE=true`.

## OpenRouter Batch API

`AiClient` also exposes OpenRouter's asynchronous Batch API as a provider-specific
operation rather than extending the canonical interactive `Request` or
`ResponseStream` contract. `OpenRouterBatchRequest` preserves the provider's
stream-parser field order (`endpoint`, `model`, `requests`), validates unique
`custom_id` values and JSON object bodies, and supports the Chat, Responses,
Anthropic Messages, and embeddings endpoint paths. `submit_openrouter_batch`,
`get_openrouter_batch`, and `list_openrouter_batches` reuse endpoint
authentication, no-redirect transport, bounded response reads, timeout phases,
and diagnostic redaction. Batch results are inline only after asynchronous
completion and remain mapped to caller ids; the client does not retry a
submission or run octet's tool loop. The coding-agent `batch` command owns file
input, model/catalog selection, polling, and JSON output. See
[`../openrouter-batches.md`](../openrouter-batches.md) for the user contract.

## Cost accounting

Usage buckets remain disjoint and pricing uses integer picodollar arithmetic. A response carries exact provider usage and optional cost; the agent decides when that completed operation becomes durable session accounting.

Usage lost through interruption or an ambiguous HTTP 5xx (including gateway
504) is not inferred as zero. Even a status response need not prove whether
upstream generation was accepted or charged. The
agent persists separate `usage_uncertainty` evidence before replacement; known
usage/cost remains an independent subtotal. Success, resume, and checkout do not
clear uncertainty. `Session::has_uncertain_usage()` exposes this durable state;
hard cumulative ceilings fail closed while it is set. Fork accounting starts
independently. This additive session record evolves the record contract without
a schema-version bump; see [sessions](../sessions.md).
