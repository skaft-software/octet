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
