# octet-ai

Provider-independent inference for octet's agent loop.

`octet-ai` provides one canonical conversation model and one event stream across:

- OpenAI Chat Completions
- OpenAI Responses
- Anthropic Messages
- Amazon Bedrock Converse
- Google generateContent

The crate supports tools, reasoning continuation state, images, Chat conversational audio, structured output, strict/lossy cross-protocol conversion, dynamic authentication, integer usage pricing, custom endpoints, cancellation by stream drop, an embedded offline model catalog, and OpenRouter's asynchronous Batch API.

Feature support varies by protocol; this list does not imply uniform capabilities
or live-provider acceptance across all five.

See the [AI design](../../docs/design/octet-ai.md), the [OpenRouter Batch API guide](../../docs/openrouter-batches.md), and the crate-level Rust documentation for the public API.

## Model presets

`ModelConfig.preset` is admitted into `ModelSpec.preset` and consumed by the
Chat/Responses builders. Sampling defaults cannot replace structural, billing,
retention, tool, reasoning or token-limit controls. Explicit request temperature
wins a model default. Declared Chat thinking formats support effort mapping,
conditional template variables and reasoning-token budget fields; declared
Mistral Chat profiles emit either `reasoning_effort` or `prompt_mode` and retain
thinking replay. These are model/endpoint declarations, not model-name guesses.

Model headers override endpoint defaults, but not codec or authoritative auth
headers; request-aware signing runs over the final request. HTTP inference,
Responses compact and WebSocket prewarm share this composition. WebSocket
connection identity binds the final URL and headers so changed model headers or
credentials cannot reuse a stale handshake. Public `ModelSpec` serialization
omits preset headers; Debug output redacts them. Explicit private configuration
serialization retains headers and must not be used as public inventory.

The coding-agent xAI route follows Responses in Pi reference
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`; its older Chat inventory fixture stays
historical, with a named current-reference difference in the contract tests.
This does not qualify xAI OAuth or a newer static model catalog.

## Proxy environment

The default client snapshots `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and
`NO_PROXY` (lower-case names take precedence; empty values fall through).
`NO_PROXY` excludes a domain and its subdomains, including `.domain` and
`*.domain` spellings, with optional ports. Only HTTP/HTTPS proxies are accepted;
invalid settings fail before credential resolution or dispatch. Preferred
WebSocket endpoints use the HTTP/SSE route when a proxy is selected; the client
does not bypass that proxy to prewarm a socket. This is not native WebSocket
proxy tunnelling. `try_with_proxy_environment` accepts an explicit snapshot for
embedders; `with_http_client` leaves proxy policy to the supplied transport.

## Responses tier pricing

`responses_cost_of` is shared by terminal Responses pricing and host request-cost
estimation. The declaration-owned Codex profile qualifies flex ×0.5 and priority
×2 (×2.5 for the exact API model `gpt-5.5`). A terminal `default` echo on that
profile retains requested flex/priority. Other echoes win; absent echoes use the
request. Unknown tariffs, unresolved `auto`, missing terminal usage and totals
not representable in picodollars remain unpriced, not catalog-rate fiction.
Scaling occurs before flooring categories and preserves long-context tiers.

Hosts must use the same tier-aware estimator for reservations/hard budgets and
persist `Response.cost` without catalog-only repricing. Exact terminal settlement
alone does not make an ordinary-rate worst-case reservation safe. No tier changes
retention, retries, model limits or authority.

## Explicit grammar tools

Chat and Responses custom-call inputs are decoded into the grammar schema's
required string property, including fragmented/escaped Unicode input. Completed
calls remain subject to ordinary schema and buffer bounds. Responses terminal
snapshots cannot change a published call. Canonical history and authoritative
custom-call outputs replay with matching custom-call/result wire types; opaque
provider calls themselves are not rewritten based on a later tool definition.
Per-model grammar compatibility/default-strict declarations and tool-aware
`LocalAssistant` raw replay remain separate parity work.

The loopback/wire regressions for these paths are in `tests/provider_parity.rs`;
source presence is not a live-provider qualification claim.

## Host-owned request runtime hooks

`HostRequestOptions` carries host-owned, non-serializable material for exactly
one request attempt: a per-request `api_key`, provider `metadata`, and the
`transform_headers` / `on_payload` / `on_response` hooks plus a per-request
`fetch` transport. `stream_with_host_options` / `complete_with_host_options`
consume them on the built-in HTTP path. The header transform runs after
endpoint/model/codec headers and before authentication, so signers still cover
the final set, and it cannot add or change `authorization`, `host`,
`content-length`, or `x-amz-*` names. A credential override only replaces an
environment-backed scheme (never a fixed secret or signer). Payload hooks see
the exact encoded JSON and are bounded; response hooks observe status/headers
before the body is consumed. Hooks never retry, and host transports refuse wire
hooks so extension transports cannot observe or rewrite wire material they do
not own. Non-empty `metadata` is refused until a codec declares the wire field;
`RequestOverrides` still owns the data-only sampling/header/env/timeout/retry
surface, and nonzero retry controls stay refused as host-owned.

## Deferred responses and the faux provider

A provider may park a request instead of completing it. The transport half is
`DeferredHandle`, `StopReason::Deferred`, `Response.deferred`, and
`DeferredPollPermit`: a permit is one-shot and generation-bound, and
`AiClient::fetch_deferred` consumes it before any provider work, so a driving
pass can never admit two billable polls. `submit_deferred` /
`fetch_deferred` / `cancel_deferred` are optional `HostStreamTransport`
methods; a transport that cannot park fails closed with
`UnsupportedError::Deferred` rather than faking a suspension. The durable
suspend/resume leaf, poll numbering, crash recovery, and effect-pending
replacement remain host/kernel-owned. `FauxProvider` is an in-process test
double with pending/ready/failed/cancelled deferred states, scripted messages,
tool calls, and counters; it is deterministic, offline, and never registered
by default.

## Image generation

`ImageModelCatalog::builtin()` embeds the checked-in OpenRouter image-model
snapshot generated by `scripts/refresh-openrouter-image-models.py`; normal
builds and runtime never fetch the network. `AiClient::generate_images` posts
the chat-completions images surface with `modalities`, parses `data:` image
URLs, reports token usage, and computes cost from the pinned catalog rates only
when the route publishes one (dynamic-routing placeholders stay unpriced).
Every bound is enforced before dispatch or allocation: input count/prompt/image
bytes, generated-image count/size, and response body size. Failures return a
typed error, or an in-band `error` result through
`generate_images_reporting`. `ImageCancellation` gives an explicit aborted
result before dispatch and after response headers; an in-flight request is
cancelled by dropping the returned future. Nonzero retries and per-request fetch
transports are refused; request hooks reuse `HostRequestOptions`. Provider
credentials come from the declared environment variable (`OPENROUTER_API_KEY`);
no key is invented for an unknown image provider.

## Credential environment aliases

`anthropic_bearer_auth` applies the documented OAuth/bearer alias order
(`ANTHROPIC_AUTH_TOKEN` then `ANTHROPIC_OAUTH_TOKEN`) while leaving
`ANTHROPIC_API_KEY` on the ordinary `x-api-key` route. `select_vertex_credential`
selects API-key mode from a present `GOOGLE_CLOUD_API_KEY` and never falls
through to ambient application-default credentials once a key is selected; with
no key, an explicit `GOOGLE_APPLICATION_CREDENTIALS` selects ADC, and `None`
means the caller decides whether an ADC probe is available. These helpers read
only the supplied environment view: they perform no I/O, never mutate process
state, and never probe the metadata server.
