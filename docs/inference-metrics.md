# Inference measurements (unreleased)

[Providers](providers.md) · [Telemetry](telemetry.md) · [Performance contract](design/performance.md)

This checkout separates **client observations** from **server-reported generation
throughput**. It does not expose a universal GPU decode-speed measurement.
Deterministic fixtures establish parsing, units, scope and propagation—not live
provider accuracy, loaded-serving performance, or a released feature.

## Client observations

`Response.inference` carries `InferenceMetrics`. Ordinary `AiClient::stream*`
and `complete*` calls measure at the same guarded canonical-stream boundary,
regardless of selected provider, codec, credentials or transport. Measurements
freeze before the response reaches agent persistence, terminal gates, tool
admission and rendering. Timing uses a monotonic local clock; offsets and
normalized durations are integer nanoseconds, not claims of nanosecond precision.

The client records:

- request/segment/poll origin to availability of its guarded stream handle
  (`stream_ready_ns`), **not** raw HTTP headers;
- first canonical event (`first_event_ns`), which can be synthetic `Started`;
- first nonempty output, answer text, streamed reasoning, tool-argument delta,
  and completed generated media, separately;
- last output, UTF-8 delta byte counters, completed media count, nonempty output
  event count, and maximum observed inter-output gap;
- origin to canonical completion, and terminal provider-usage output tokens;
- derived first-to-last output interval and last-output-to-completion tail.

Empty deltas, part starts/ends, signatures, usage and final assembled calls are
not new output activity. Events and UTF-8 bytes are **not tokens**. Reasoning
summaries are not a timeline of hidden reasoning. One buffered chunk gives a
zero first-to-last interval, not an infinite decode rate. Completed audio/media
is an observation of completed media, not a timestamp for its individual tokens.

Polling is consumer-driven. Client observations include preparation/opening,
network/proxy buffering, local decode and scheduling, and caller backpressure.
They are not server execution time or terminal paint. They cannot separate
queueing from prefill, hidden reasoning, speculative acceptance/rejection,
preemption, batching or network latency.

| Client scope | Origin | E2E token rate |
| --- | --- | --- |
| `request` | Ordinary client call, including the initial native-steering request | Terminal reported output tokens / client elapsed time, when both are positive |
| `response_segment` | Successor native-steering `response.created` observation | Unavailable: no original request-submission clock is invented |
| `deferred_submit` | One deferred submission operation | Unavailable: parked generation is not submission latency |
| `deferred_poll` | One permitted deferred poll | Unavailable: a fast fetch is not fast generation |

`ClientInferenceMetrics::end_to_end_tokens_per_second()` only returns the first
scope's explicitly **E2E usage throughput**. Hidden/billed output may be included
in that numerator. Failed/cancelled attempts do not manufacture a completed
sample, and a retry or successor gets independent state. A direct codec result
can have server metrics without any client clock; that absence is not zero
latency. Retained WebSocket retrieval remains part of the same observed request,
not a replayed generation or a fresh decode window.

## Server-reported generation throughput

Recognized, terminal count/duration pairs retain their native definition:

| Source | Matched counter | Duration and original unit | Codec |
| --- | --- | --- | --- |
| `timings_predicted` | `timings.predicted_n` | `timings.predicted_ms`, milliseconds | Chat; completed/incomplete Responses |
| `time_info_completion` | Same-frame `usage.completion_tokens` | `time_info.completion_time`, seconds | Chat (including Cerebras-style envelopes) |
| `usage_completion` | `usage.completion_tokens` | Same-envelope `usage.completion_time`, seconds | Chat (including Groq-style completed responses) |
| `x_groq_usage_completion` | `x_groq.usage.completion_tokens` | Same-envelope `x_groq.usage.completion_time`, seconds | Chat streaming |

Optional valid prompt, queue and total duration fields remain separate. Their
inclusion/exclusion conventions stay source-specific. Client elapsed minus a
server total is **not network RTT**. Source and original unit accompany the
normalized pair. Recognizing an envelope does not authenticate a server's
assertion or imply that every endpoint/vendor/version emits it.

The rate is `native counter / matching native generation duration`. It is
labeled **server-reported generation**, not automatically post-first-token
decode, GPU-active throughput, ITL, TPOT, or an accelerator benchmark. No `N−1`
correction is invented: that requires a compatible first-token boundary and
counter definition. Billing counts can include hidden reasoning or rejected
prediction tokens; native counters never replace `Usage`, pricing or durable
usage uncertainty.

Chat timing must arrive on a finish-bearing frame or trailing usage-only frame.
Earlier snapshots stay provisional; their duration is never paired with a later
usage counter. Responses uses its completed/incomplete terminal object. Conflicting
terminal pairs (including within the same source) or observed response-ID
mismatches suppress the server rate. Invalid terminal pairs cannot subsequently
be resurrected by a different envelope. Recognized nested advisory numbers are
tolerant; malformed values do not invalidate otherwise valid assistant output.
Accounting fields and the existing JSON/framing/stream limits remain strict.
The parser retains only known numeric fields and at most one nested usage
object; unknown provider strings/trees are not copied into metrics.

`server_unavailable` explicitly distinguishes `not_reported`, `invalid`,
`provisional` and `conflicting`. It is absent when a valid server pair exists.
There is no visible-text retokenization, character-to-token estimate, timestamp
subtraction or client-derived fallback for a missing server rate.

## Coverage by route and transport

All built-in conversation routes share the client boundary. The declarations,
not a provider-name timing heuristic, select their codec:

| Codec | Declared endpoint IDs | Server timing |
| --- | --- | --- |
| Responses | `openai`, `xai`, `opencode`, `openai-codex`, `xai-subscription`, `meta-subscription`, `cloudflare-ai-gateway-openai`, `azure-openai`, `meta` | Optional recognized `timings` only; ordinary public schemas do not promise it |
| Chat | `deepseek`, `openrouter`, `groq`, `cerebras`, `together`, `fireworks`, `nvidia`, `huggingface`, `moonshotai`, `xiaomi`, `opencode`, `openrouter-oauth`, `mistral`, `cloudflare-workers-ai`, `cloudflare-ai-gateway-compat`, `moonshotai-cn`, `opencode-go`, `baseten`, `qwen-token-plan`, `qwen-token-plan-cn`, `qwen-token-plan-individual`, `zai-coding-cn` | Recognized envelopes only when supplied |
| Anthropic Messages | `anthropic`, `fireworks`, `minimax`, `opencode-anthropic`, `kimi-coding-subscription`, `cloudflare-ai-gateway-anthropic`, `kimi-coding`, `minimax-cn`, `vercel-ai-gateway`, `xiaomi-token-plan-ams`, `xiaomi-token-plan-cn`, `xiaomi-token-plan-sgp` | Explicitly unavailable without a recognized native count/duration contract |
| Google generateContent | `gemini`, `vertex`, `opencode-google` | Explicitly unavailable; usage/timestamps do not establish decode timing |
| Bedrock Converse | `bedrock` | Explicitly unavailable; invocation latency is not a generation interval |
| Native Mistral Conversations; Pi Messages | Explicitly configured experimental routes | Client observations; no recognized server generation contract |
| Custom compatible routes; host-owned transports (including faux) | Host-configured endpoints and supported Copilot routes | Client observations; recognized codec envelopes or host-supplied typed metrics, not guessed vendor speed |

HTTP SSE, completed Chat JSON/audio, completed Responses JSON normalization,
Bedrock binary event streams, ordinary WebSockets, HTTP fallback/proxy routes,
native-steering segments and host/deferred streams all retain their scopes.
Authentication, headers, replay policy and requests are unchanged; no extra
network call negotiates timing. Local llama.cpp-compatible responses can provide
`timings`; vLLM, SGLang, LM Studio and Ollama-compatible routes may omit it.
Ollama's **native** `eval_count`/nanosecond `eval_duration` is not a wire protocol
implemented here and is not guessed from its OpenAI-compatible responses.

Separate image-generation, native compaction and batch-job APIs retain their
own result/lifecycle contracts; this conversation-response measurement does not
reinterpret job wait, image counts or native compact latency as token decode
throughput. Agent compaction *summary* calls using ordinary conversation streams
are measured normally.

## Presentation and observability

- Completion retains `tok/s E2E (last turn)` and adds a separate
  `tok/s server-reported generation (last turn)` line only for a valid native pair.
- `/status` shows client output offsets/event count/gap/tail and server source,
  native count, normalized duration, original unit, or explicit unavailability.
- `AgentEvent::ProviderInference` is transient and carries the frozen metrics.
  It is not assistant content, durable session usage or a terminal-gate verdict.
- Optional telemetry JSONL writes a bounded `provider_inference` record with
  attempt/logical-turn identity and typed metrics; native-host NDJSON and RPC
  forward a `provider_inference` event without altering assistant messages.
  Print/plain response stdout and durable Serve item projections stay unchanged.
- Provider-request observer spans carry separate `client_*`/`server_*`
  completion attributes. Existing usage attributes remain accounting facts.
- Legacy `ttft_ms` remains agent-first-nonempty-delta latency. Legacy
  `generation_ms` is first-agent-delta-to-`TurnFinished`, including the completion
  and settlement tail; `first_delta_to_turn_finished_ms` names it honestly.
  Neither is used as server generation duration.

Attempt reset, model/branch hydration, rejection and deferred/native segment
scopes must not resurrect a previous successful rate. For legacy external event
producers without AI-client metrics, presentation's E2E fallback is explicitly
agent-observed request-to-`TurnFinished`, which can include persistence; it still
cannot stand in for server timing.

## Evidence and further qualification

Synthetic codec fixtures test matching envelopes/units, hidden-vs-native counts,
missing/invalid/provisional/conflicting states, duplicate advisory numbers,
response identity, byte-fragmented SSE and completed audio. Deterministic client
clock tests separate reasoning, answer, arguments, output gaps and completion
tail. Existing loopback transport regressions assert measurement propagation
through every codec, WebSocket transport selections, steering and deferred scopes.
Presentation/telemetry tests pin distinct labels, accounting independence and
frozen timing despite delayed persistence.

Live-provider timing availability and numerical accuracy remain **unqualified**.
A production-serving campaign should reuse [AIPerf](https://github.com/ai-dynamo/aiperf)
or [vLLM benchmarking](https://docs.vllm.ai/en/latest/cli/bench/serve/) with pinned
server/client versions, model, tokenizer, hardware, precision, cache policy,
reasoning/speculation controls, input/output distributions, concurrency/arrival
rate and service tier. Retain independent raw trials, failures and backpressure;
report latency percentiles, TTFT, chunk ITL, compatible TPOT, successful
requests/second, tokens/second and SLO-qualified goodput separately. Native
server observations and client observations must remain independently labeled.
A fixture pass is not that campaign, a speed ranking, or a publication receipt.
