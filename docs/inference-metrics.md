# Inference measurements (unreleased)

[Providers](providers.md) · [Telemetry](telemetry.md) · [Performance contract](design/performance.md)

This checkout estimates **decode cadence**, rather than labeling round-trip
throughput as decode speed. It prefers **server-reported generation** when a
matching native pair exists; otherwise it uses a separately labeled robust
client-stream estimate. Neither authenticates pure GPU-active execution.
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

After the first poll, a cancellation-owned receive task drains ahead of the
consumer into a 256-event / 16-MiB admission queue. Larger guarded terminal/media
events own the whole byte budget alone; the canonical content caps still apply.
Dropping the stream aborts the reader. Queue saturation marks local backpressure
and suppresses the decode estimate, without dropping/reordering events or changing
native timing. Native steering also has a bounded outer receive task so successor
segments are not timed by terminal painting.

Client observations include preparation/opening, waiting before the first poll,
network/proxy buffering, local decode and scheduling, and caller backpressure
when the queue saturates. Already-ready terminal responses and EOF remain ready
in the same poll, preserving accepted-result settlement under cancellation.
These observations are not server execution time or terminal paint. They cannot separate
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

## Decode estimation without native timing

`InferenceMetrics.decode_estimate` is an estimate of visible-output decode cadence,
not E2E and not an asserted server measurement. The same implementation is used
by every conversation codec/transport; there is no provider-name speed heuristic.

- Capture cumulative answer-text and tool-argument UTF-8 byte progress on the
  receive side, excluding empty deltas, assembled calls and reasoning summaries.
  No response text, tokenizer files or provider payloads are retained.
- Coalesce arrivals within a fixed 2-ms burst window. Retain at most 256 samples
  with deterministic decimation, plus first/final boundaries. Events are never
  counted as tokens; the first chunk can contain many tokens.
- Calibrate progress using terminal `output_tokens - reasoning_tokens` across
  the whole observed visible output. This avoids a guessed characters/token
  constant, but **assumes approximately representative bytes/token over the
  fitted window**. Tool framing, unreported hidden output, rejected predictions
  and changing text/token density can bias that assumption.
- Fit the median of long-baseline pair slopes (at least one-quarter of observed
  progress and 62.5 ms apart). The intercept absorbs prefill/round-trip/first-chunk
  delay. Differencing byte progress removes the first chunk's proportional token
  mass instead of incorrectly dividing all tokens by first-to-last time. Final
  usage, agent persistence and completion-tail delays are not fitting samples.
- Require at least 32 visible-usage tokens, eight arrival bursts and a 250-ms
  observed window. Reject pair-slope 10th-to-90th percentile dispersion above
  50%, local reader backpressure, media, deferred retrieval, and reasoning whose
  usage cannot be separated. Short/unstreamed/fully buffered output is unavailable.

`relative_dispersion` describes fit stability, **not an accuracy probability or
confidence interval**. A stable stream can still have hidden server buffering or
a wrong token basis. Chunk jitter can be reduced statistically; arbitrary proxy
buffering, hidden reasoning timing, GPU scheduling and speculative execution
cannot be uniquely recovered from arrivals. This is the best available estimate
under its documented assumptions, not a guarantee of matching every provider's
server report. Native timing always wins the completion display; both independent
observations remain available for comparison in `/status` and telemetry.

`decode_unavailable` names missing visible usage, an unknown reasoning split,
insufficient output/samples, buffered output, unstable cadence, deferred operations,
media or local backpressure. It never falls back to the E2E average. Native
`server_unavailable` remains independent and is not overwritten by an estimate.

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
There is no client-derived fallback masquerading as a server rate. The separate
usage-calibrated estimate has its own provenance and unavailable reasons.

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

- Completion shows `tok/s generation (server-reported, last turn)` when native
  timing exists; otherwise `~tok/s decode (estimated, last turn)` or explicit
  `decode unavailable`. E2E is no longer on the completion line or copied outcome.
  `/status` retains E2E as a separately labeled request-throughput diagnostic.
- `/status` shows client output offsets/event count/gap/tail, estimate sample
  count/window/visible-token basis/dispersion/reasoning exclusion, and server source,
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
Presentation/telemetry tests pin distinct labels, native preference, no E2E
completion fallback, accounting independence and frozen timing despite delayed
persistence. Deterministic known-cadence fits test multi-token first batches,
prefill offsets, packet jitter, chunk bursts, hidden reasoning exclusion and
bounded long streams. Reader tests establish independent progress during a stalled
consumer, queue/byte admission, ordering, saturation feedback and drop cancellation.
They do not qualify a real provider's GPU execution.

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
