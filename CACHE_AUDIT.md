# Prompt-cache audit

## Outcome and qualification

Audited the persisted-session → model request → provider JSON → transport path
against source revision `cfe997b3` (0.9.5), with the cache changes in this working
tree. Two native annotation defects were demonstrated and fixed, without
removing context:

1. Anthropic's previous conversation breakpoint became unreachable after large
   parallel tool batches exceeded its 20-content-block lookup window.
2. Bedrock Converse emitted no native prompt-cache points, even for supported
   Claude models with retention enabled.

A third fix recognizes Kimi's top-level `cached_tokens` usage alias. It corrects
accounting, not the provider's actual cache behavior.

**The ≥98% target passes the local cache-policy simulation, not a live-provider
acceptance test.** The persisted agent benchmark also retains ≥98% diagnostic
prefix bytes per steady-state request. Neither serialized byte estimate is
provider tokenization, billing, or proof of a cache hit. No live inference,
credential exchange, or paid benchmark was performed. Conventional provider
credential environment variables were unset; saved DeepSeek and Codex credential
files exist but were not opened or validated for this audit. There is no claim
that all possible credential sources are unavailable.

The Responses follow-up also fixed a **diagnostic-only** defect: native
`prompt_cache_breakpoint` annotations were invisible to breakpoint reporting and
moving them looked like rewritten history. This changes neither provider payloads
nor cache policy. The shared GPT-6 Luna/Flex-path checks below found no additional
provider-cache defect; their byte-preservation results are not measured cache hits.

## Evidence: before and after

### Native cache-policy simulation

Command:

```sh
cargo test -p octet-ai --lib --locked cache_audit -- --nocapture
```

Fixture: ~150K input tokens **estimated as serialized UTF-8 bytes / 4**, generated
coding context, fixed system and four tool schemas (`read`, `edit`, `bash`, an
extension probe), nine requests, first cold request excluded. Eight successive
requests append either one or 32 paired tool calls/results. Real Anthropic and
Bedrock codecs produce the JSON used by the simulator.

| Codec | Calls per turn | Before reuse | After reuse | Before fresh estimated tokens | After fresh estimated tokens |
| --- | ---: | ---: | ---: | ---: | ---: |
| Anthropic Messages | 1 | 99.964361% | 99.964361% | 426 | 426 |
| Anthropic Messages | 32 | 0.426815% | **98.903569%** | 1,249,988 | 13,764 |
| Bedrock Converse | 1 | 0% | **99.967532%** | 1,195,034 | 388 |
| Bedrock Converse | 32 | 0% | **98.995962%** | 1,249,754 | 12,548 |

Post-fix aggregate counters, excluding the cold request:

| Codec / batch | Input content bytes | Reused content bytes | Estimated input tokens | Maximum points |
| --- | ---: | ---: | ---: | ---: |
| Anthropic / 1 | 4,781,300 | 4,779,596 | 1,195,325 | 3 |
| Anthropic / 32 | 5,021,384 | 4,966,328 | 1,255,346 | 4 |
| Bedrock / 1 | 4,780,136 | 4,778,584 | 1,195,034 | 2 |
| Bedrock / 32 | 4,999,016 | 4,948,824 | 1,249,754 | 3 |

Before changing the Anthropic codec,
`large_tool_batch_keeps_the_previous_conversation_breakpoint_reachable` failed
with `cache model: read=2679 total=603586`. The old history had not changed:
32 assistant tool-use blocks plus 32 user tool-result blocks moved the new point
64 boundaries away from the prior cached user boundary. Only the small static
prefix remained reachable. The repair explicitly retains that preceding user
boundary using the fourth available slot. Bedrock now emits system and last-user
points and the same preceding-user bridge for large batches. Retention None
still emits no points; Long adds `1h` only when cache compatibility allows it.

The reproducible report prints both `policy=before_annotations` and `policy=after`.
The former reconstructs the pre-fix policy by removing only the added
annotations from current codec output (no Bedrock points; no Anthropic bridge).
It is a test-only reconstruction, not a second production implementation. The
pre-edit failing regression above was also observed against the old codec.

**Simulation limits:** hashes serialized provider content-block prefixes; stores
explicit breakpoints; searches 20 candidate boundaries per breakpoint. It does
not implement provider tokenization, minimum cacheable sizes, expiration,
eviction, routing, account affinity, concurrency, rounding, reasoning-specific
rules, or actual billing. In particular, the small static-prefix hit in the old
Anthropic batch-32 case is optimistic: a provider minimum could make it zero.
The Bedrock lookback calculation models Claude behavior, not every Bedrock model.

### Real persisted-agent / loopback-HTTP benchmark

```sh
cargo test -p octet-agent --test agent_run --locked \
  persisted_large_coding_session_prefix_audit -- --nocapture
```

`crates/octet-agent/tests/cache_audit/agent.rs` creates a temporary workspace and
persisted journal, loads real `CoreTools` and a native extension tool, and runs
16 scripted tool turns: read, edit, shell compilation/assertion, extension probe.
The shell actually runs `rustc` and checks the edited value with `grep`. All tool
results must succeed; the final file must contain the fourth edit. The agent is
dropped and recreated from the journal after request 12, then an explicit
compaction is persisted before request 18. This is a persisted-session reopen
inside one process, not a CLI/process-restart or live-model test.

Observed sample:

```text
estimated_initial_tokens=150096
requests=19
steady_requests=17
ordered_input_bytes=10272861
reusable_bytes=10265600
fresh_est_tokens=1816
efficiency=99.929319%
live_provider_usage=NOT_RUN
```

The initial ordered payload is 600,384 bytes. All 17 steady requests retain the
entire previous ordered prefix, with identical tools/system and no changed
historical message. Each independently exceeds the 98% diagnostic byte threshold (sample minimum
99.882532%). Journal identity and serialized canonical history are equal before/after reopen.
The ordered view is tools → system → messages with native annotations removed;
it is not raw JSON field order or a provider tokenizer.

Selected first-change evidence (zero-based request indices):

| Request | Raw JSON LCP bytes | Ordered LCP bytes | Previous → current ordered bytes | First changed historical message |
| --- | ---: | ---: | --- | --- |
| 1, first tool result | 594,269 | 600,384 | 600,384 → 600,769 | none; append only |
| 13, session reopen | 599,654 | 605,769 | 605,769 → 605,912 | none; append only |
| 17, last steady tool result | 601,283 | 607,398 | 607,398 → 607,645 | none; append only |
| 18, after compaction | 87 | 6,170 | 607,645 → 8,212 | **0** |

Compaction invalidates a 601,475-byte old diagnostic suffix (~150,369 estimated
tokens). It is intentionally outside the steady-state aggregate, not hidden as
a cache success. Raw JSON diverges early because `messages` is serialized before
system/tools and annotations move; that is not automatically a model-token
prefix loss. Fixture response usage is synthetic and deliberately not used for
these efficiency numbers. Timing and tool-output formatting can vary sample
sizes slightly.

### Repeated encoding across all conversation codecs

The same `cache_audit` command also re-encodes an unchanged large request twice,
checks historical append stability, unchanged catalogs/instructions and canonical
request nonmutation. Cache annotations are disabled for this isolation test.

| Codec | Repeated serialized body bytes | Raw LCP bytes, sample |
| --- | ---: | ---: |
| Anthropic Messages | 597,192 | 597,192 |
| Bedrock Converse | 597,111 | 597,111 |
| OpenAI Chat | 597,153 | 597,153 |
| OpenAI Responses | 597,137 | 597,137 |
| Google generateContent | 596,971 | 596,971 |
| Mistral Conversations | 597,151 | 597,151 |
| Pi Messages | 597,315 | 594,100 (clock-dependent) |

Pi `encode_context` calls `timestamp_ms()` once per encode and uses the new time
for historical messages. Removing only that timestamp metadata makes the two
Pi fixtures identical. This is demonstrated wire volatility, **not demonstrated
upstream model-token cache invalidation**: a Pi gateway may discard timestamps.
The adapter is not changed speculatively; preserving historical times would
require an explicit canonical/persistence contract or evidence from the gateway.

### Shared Responses follow-up: Luna, Flex, and all builtin models

The follow-up covers every builtin model on the Responses codec: `gpt-6-astra`,
`gpt-6-sol`, `gpt-6.1-sol`, `gpt-6-luna`, and `gpt-5.4-mini`. Flex is a service tier,
not a separate model. The default Responses runtime profile currently rejects
selected service tiers; Flex fixtures explicitly declare the existing `Codex`
profile, which permits them. They do **not** enable public-API Flex, prove a live
Codex/model combination is available, or grant endpoint authority from a model
name. Undeclared tiers remain rejected.

Before the trace fix, two observer regressions failed: five documented native
breakpoints were reported as zero, and moving only those annotations reduced the
reported common prefix from 285 to 14 bytes. The observer now reports their
positions and removes them only from its comparison projection. This is restricted
to `input_text`, `input_image`, and `input_file` blocks in user/system/developer
messages or function/custom-tool outputs. Schemas, call arguments, unknown blocks,
assistant output, and opaque reasoning fields retain their bytes. Raw digests still
change when annotations move; outgoing bodies are untouched. Four new unit tests
cover these boundaries and the 256-position diagnostic output bound. A loopback
HTTP test additionally moves a marker in a payload hook, distinguishes encoded
from final-wire changes, and correlates synthetic Flex usage without leaking the
prompt, tool data, session, credential, or endpoint.

Two independent local prefix checks now cover the shared path (sample results):

| Check | Cases | Steady-state serialized-byte result |
| --- | --- | ---: |
| Codec, one tool per batch | All five models, nine requests each | 99.944500%–99.962152% aggregate |
| Codec, 32 parallel tools per batch | All five models, nine requests each | 98.337703%–98.839653% aggregate |
| Persisted coding session | All five models × default/declared-Codex-Flex | 99.871392%–99.911763% aggregate; minimum individual comparison 99.803496% |

Reproduce with:

```sh
cargo test -p octet-ai --lib --locked cache_audit_responses -- --nocapture
cargo test -p octet-agent --test agent_run --locked \
  persisted_responses_coding_session_prefix_audit -- --nocapture
```

The codec check requires exact append-only input items, stable tools/reasoning/cache
controls, repeated-build determinism, and request nonmutation; its denominator is
serialized input-item bytes, not the whole request or a tokenizer. Cold requests
are excluded. The persisted fixture uses the earlier real read/edit/bash and
extension-tool workflow: each case completes 16 tool calls, verifies `value = 4`
with actual commands, makes 19 requests with 17 steady-state comparisons, reopens
the journal within the same process, and measures final compaction separately.
GPT-6 initial contexts estimate 150,082 tokens (default) / 150,101 (Flex); mini
uses a smaller fixture (about 102,600) for its 128K route. Opaque reasoning IDs,
encrypted payloads, and unknown fields survive exact replay after reopen;
`store:false` and full input replay remain intact, without `previous_response_id`.
As in the earlier benchmark, timing/tool-output formatting can vary sample sizes.

Additional codec fixtures preserve authoritative replay and declared cache controls
across auto/default/Flex/priority tiers and None/Short/Long retention. Synthetic
`response.completed` and `response.incomplete` events produce identical normalized
cache usage with whole, one-byte, and 17-byte chunks for every model and tested
default/Flex/priority tier. The usage fixture verifies inclusive input 150,000 =
fresh 1 + writes 2,999 + reads 147,000; output 19 includes reasoning 7, which must
not be subtracted again. These are parser assertions, **not** provider-hit
observations. No Luna-specific or provider-side Flex cache failure was reproduced,
and no speculative encoder or model-name fix was applied.

The public [prompt-caching guide](https://developers.openai.com/api/docs/guides/prompt-caching)
and [reasoning guide](https://developers.openai.com/api/docs/guides/reasoning)
were also read for this follow-up, alongside the checked-in Responses API snapshot.
Stable bytes alone do not establish that a matching implicit breakpoint remains
eligible when an existing user/tool-result group is extended. Actual hits still
depend on the route's eligible boundaries, minimum length, TTL, routing, and
billing policy. Opaque-reasoning replay and model-dependent `all_turns` defaults
must be preserved; neither was changed to inflate cache reuse. These append-only
fixtures do not qualify arbitrary history rewrites, live tiers, or provider billing.

## End-to-end source map and mutation analysis

| Boundary / source | Finding |
| --- | --- |
| `crates/octet-coding-agent/src/resources.rs`: `compose_instructions_at`, `compose_instructions` | Composes global/project/explicit instructions; file/config changes legitimately change the system prefix. No ordinary per-turn timestamp or counter was found. |
| Same file: `render_skills_for_prompt`, `format_skills_for_prompt`; `app/bootstrap.rs` | Selected skill descriptions are appended at bootstrap/resource reload. They are not re-read and re-ordered for each inference in the audited ordinary path. |
| `crates/octet-agent/src/extension.rs`: `policed_tools`, `model_tool_snapshot`; `tool_composition.rs`: `advertised_surface` | Stable registration/replacement slots and deterministic advertised ordering (cost class/name). Dynamic registration/replacement and owner-scoped selection are real prefix changes, not safe to ignore. |
| `extension_process/contributions.rs`, `extension_process/tools.rs` | Executable extension tool/instruction contributions join the same host snapshots. Explicit system/prompt prefixes and suffixes retain their requested placement; moving them could change semantics. |
| `crates/octet-agent/src/session/store.rs`: `cache_key`, `resource_owner_key`; `agent.rs`: constructor | Durable owner/session identity derives from the session descriptor; ordinary reopen does not regenerate a random cache identity. Fork/checkout/model changes are separate cases. |
| `session/context.rs`; `agent/images.rs` | Journal projection honors compaction, tool-result coalescing and the newest-four tool-image policy. These can rewrite earlier model-visible messages intentionally; the journal keeps original media. |
| `agent/tool_results.rs` | Tool output bounds/truncation are applied when results are inserted. Not a justification to remove more historical output on later requests. |
| `agent.rs`: `model_visible_system`; `agent/turn_loop.rs`: request assembly | Combines configured system and current tool guidelines; uses current ordered tools and projected history. Context hooks may deliberately replace messages/system before the canonical AI-client boundary. |
| `crates/octet-ai/src/transform.rs` | Target-model normalization preserves same-model replay and deterministically handles incompatible reasoning, media, IDs and missing tool-result pairs. A model switch can legitimately change old history. |
| `client.rs`: `stream_once`; `protocol/*::build_request` | Captures canonical before normalization and codec output before host/extension payload hooks. Real provider encodings differ; JSON byte order is not provider token order. |
| `client/stream.rs`: `stream_http` | Captures final uncompressed JSON after payload hooks, before compression/signing. Auth/headers are never included in prefix diagnostics. |
| `responses_ws.rs`: final `response.create` frame | Shape and prefix diagnostics see the frame after `previous_response_id`/suffix rewriting, immediately before serialization/send. |
| Protocol usage decoders; `agent/cache.rs`; `telemetry/schema.rs` | Cache reads/writes and uncached input are disjoint. Existing footer/telemetry formula was not the demonstrated cause of misses. |

### Adapter, routing and subagent caveats

- **Anthropic:** tools → system → conversation is the relevant logical hierarchy.
  Explicit points have a four-point budget and a 20-block search window. The fix
  changes annotations only. Signed/redacted thinking remains provider-affine;
  unsupported thinking blocks do not acquire invented cache controls. Exact
  warming replay still changes the output cap, not history or reasoning budget.
- **Bedrock:** cache support is gated by Claude foundation-model API identities,
  including regional/system inference profiles and model ARNs: 3.5 Haiku, 3.7
  Sonnet, 4.x and the declared 5-family identities. Nova keeps automatic caching;
  unsupported models and opaque application-profile routes receive no points.
  Display names and a force-cache environment override are not used as proof of
  support. Deployment/model support and allowed one-hour retention need live
  qualification; capability metadata is not a successful cache write.
- **Chat / DeepSeek / Kimi:** deterministic serialization is not evidence that a
  particular compatible server caches reasoning/tool history. DeepSeek's dedicated
  `reasoning_content` replay remains model-specific and is required during tool
  continuations; stripping it to improve a cache metric would be incorrect.
  Kimi alias precedence is nested `prompt_tokens_details.cached_tokens`, then
  `prompt_cache_hit_tokens`, then top-level `cached_tokens`. They are alternatives,
  never additions. Duplicate fields and impossible usage underflow still fail.
- **Responses / Codex:** `openai_responses/input.rs` preserves authoritative
  provider replay/IDs; `request.rs` supplies cache/affinity controls. A live
  WebSocket may send only a continuation suffix, so low raw-frame overlap can be
  correct. Resume falls back to authoritative local replay, not a fabricated
  durable `previous_response_id`. Changing route, model, tools, fixed controls,
  reasoning configuration or account can change cache eligibility.
- **Google:** the inclusive prompt count is normalized by subtracting
  `cachedContentTokenCount` once; do not add the cached count to that inclusive
  count again. Automatic cache availability is not proved by repeat JSON.
- **Mistral Conversations:** deterministic supplied `inputs` are sent to a new
  nonstored conversation; there is no canonical durable conversation continuation
  field to invent as an affinity fix.
- **Subagents:** `delegation/manager_workers.rs::build_child_agent` constructs a
  scoped child tool surface, system instructions, session and owner. A child is
  not the parent's identical cache lane. Pool metrics per stable child/model,
  not across parent/child prefixes; adding parent instructions or sharing a
  continuation identity would cross ownership/semantic boundaries. No change was
  made to scheduling, tool scopes or child histories.
- **Intentional invalidation:** screenshots aging out of the four-image window,
  compaction, resource reloads, tool schema changes, prompt/context hooks, model
  changes and reasoning-profile changes need separately labeled cold intervals.
  A 98% promise for all such requests would be misleading.

Local reference inspection used installed Pi 1.0.2 under `@earendil-works/pi-ai`:
`dist/api/bedrock-converse-stream.js` (supported families, native points),
`dist/api/openai-completions.js` (usage/reasoning conventions), and the relevant
Messages/Responses adapters. This is source comparison, not a Pi live benchmark.
No DSH implementation was available; no relative DSH performance claim is made.

## Privacy-safe diagnostics

Enable for an ordinary conversational run with a fresh, private file:

```sh
OCTET_PROVIDER_REQUEST_TRACE=/tmp/octet-cache-audit-one-process.jsonl octet
```

Leave unset to disable. `cache_prefix` records share an `id` with existing
request/rejection shape records and terminal `cache_usage`. Stages:

1. `canonical`: after the agent's context hooks, before target normalization;
2. `encoded`: exact codec body, before encoded payload hooks;
3. `wire_http` or `wire_websocket`: final uncompressed JSON / actual final frame.

Opaque host transports expose only canonical plus terminal usage. Standalone
compact, prewarm and native multi-response steering are not instrumented with
this three-stage/terminal-usage contract (existing shape traces remain where
available). The next ordinary request after compaction is observed normally.

Each client has an unexported random salt; cloned clients share observation
state. Endpoint identity/base URL, model identity/API name, protocol and session
identity select a salted lane. Authorization is not resolved or hashed for this
purpose; operators must hold account/auth configuration constant themselves.
Comparisons do not survive a process restart, and numeric request IDs are only
process-local: do not concatenate unrelated processes into one trace file.

The observer retains at most 12 raw/projection snapshots and 16 MiB of payload
bytes in memory, compares bodies up to 2 MiB and emits at most 256 segment entries.
An oversized/non-JSON body clears that lane/stage's previous comparison, rather
than reporting a match across an unobserved gap. Eviction or a new client likewise
means no comparison, **not** a cache miss. Debug output redacts snapshot content.
New Unix trace files use `0600`; existing modes are not silently changed.

Only salted SHA-256 digests, counts, known segment paths, lengths, offsets,
annotation positions, a controls digest and decoded usage leave the observer.
No raw prompt, tool schema/arguments/output, credential/header, reasoning/media
content, endpoint URL or session/response identity is written. Existing shape
records still name the API model and sanitized provider rejection request ID.
Salt is never exported, preventing cross-client/run content-digest correlation.

Interpretation:

- `raw_common_prefix_bytes` is exact serialized JSON LCP, including metadata and
  moving native annotations; it is not the cacheable token prefix.
- The diagnostic `prefix_*` view concatenates tools, system, then ordered history,
  removing only native annotations at known boundaries. It never recursively
  deletes user/schema fields named `cache_control`. It is not tokenization.
- `previous_suffix_estimated_tokens = ceil((previous_prefix_bytes - LCP)/4)`;
  current suffix is analogous. Appends have zero invalidated old suffix even
  though `first_changed_segment` points to the first newly appended segment.
- Stable canonical/encoded bodies with a changed final wire body isolate a payload
  hook. The loopback hook regression finds `/messages/0`, while earlier stages
  remain identical. HTTP raw LCP is checked against captured request bytes;
  WebSocket final-frame size/continuation shape is checked against received frames.
- Stable content with a changed controls digest suggests a cache key, model
  parameter, continuation or other non-content change; inspect configuration
  locally, never dump values into diagnostics. Stable bytes with poor real reads
  instead require checking TTL, routing, minima, account and provider policy.

## Accounting and live acceptance still required

Use final canonical provider usage, not streamed deltas, output tokens or the
provider's potentially different `total_tokens` convention:

```text
fresh_input = input_tokens + cache_write_tokens
total_input = fresh_input + cache_read_tokens
cache_efficiency = cache_read_tokens / total_input
```

`cache_write_1h_tokens` is a subset of writes, never added again. Zero/unreported
input yields no ratio, not 100%. Missing terminal usage remains unmeasured.
The trace integration uses a clearly synthetic fixture: 100 inclusive prompt
input, 98 cached, 2 fresh, ratio 0.98. It verifies decode/correlation, not caching.

To qualify a provider, use a credential and cost-approved model/route with a
sufficient context window. Load representative ~100K–200K-token coding context
through normal bounded tools, then run at least eight short tool continuations
with fixed tools/system/model/reasoning/account and a stable session. Repeat a
large batch, reopen the persisted session, and test compaction separately. Measure
actual input sizes from provider usage; bytes/4 is only a fixture sizing estimate.
Finish each cached write before its dependent request and stay within the TTL.
Record cold, steady, resumed, changed-tool and compacted intervals separately.
Compare the same provider/model/task before and after; this audit has no live
before/after numbers.

For one process's trace, aggregate selected steady request IDs with weighted
counts (not the average of per-request percentages). For example, list known
cold/compaction IDs to exclude as the second argument:

```sh
python3 - /tmp/octet-cache-audit-one-process.jsonl 1,19 <<'PY'
import json, sys
excluded = {int(x) for x in sys.argv[2].split(',') if x}
reads = fresh = requests = unavailable = 0
observed, finished = set(), set()
with open(sys.argv[1]) as trace:
    for line in trace:
        row = json.loads(line)
        if row['id'] in excluded:
            continue
        if row.get('event') in ('request', 'cache_prefix'):
            observed.add(row['id'])
        if row.get('event') != 'cache_usage':
            continue
        finished.add(row['id'])
        if not row['total_input_tokens']:
            unavailable += 1
            continue
        reads += row['cache_read_tokens']
        fresh += row['fresh_input_tokens']
        requests += 1
total = reads + fresh
print(dict(requests=requests, zero_or_unreported_usage=unavailable,
           observed_without_terminal=len(observed - finished), cache_read=reads,
           fresh_input=fresh, total_input=total,
           efficiency=reads / total if total else None))
PY
```

A live acceptance pass requires measured steady `reads / total >= 0.98`, with
unmeasured/failed attempts and exclusions disclosed. This remains **NOT RUN**.

## Executed verification

The initial audit ran in the shared checkout, preserving its pre-existing dirty
metadata/goldens. The isolated PR-branch verification below excludes those edits.

- `cargo test -p octet-ai --lib --locked cache_audit -- --nocapture`: six tests
  passed (codec replay, policy report, large-batch regression and the three
  shared Responses matrix tests).
- Focused Kimi alias, Bedrock gating/TTL/nonmutation, request-trace privacy/bounds,
  HTTP hook/usage and WebSocket continuation checks passed.
- `cargo test -p octet-ai --lib --locked cache_audit_responses -- --nocapture`:
  three shared-codec matrix tests passed. The persisted Responses test covers ten
  model/profile cases and is also included in the full agent target below.
- `cargo test -p octet-ai --lib --locked request_trace::cache::tests`: 11 passed,
  including native Responses boundaries, marker movement, opaque fields and bounds.
- `cargo test -p octet-ai --test provider_request_trace --locked`: three passed,
  including the Responses/Flex final-HTTP hook and terminal-usage regression.
- `cargo test -p octet-agent --test agent_run --locked`: **186 passed**, including
  both persisted benchmarks, tool/runtime/hook/recovery tests.
- `cargo test -p octet-ai --locked --no-fail-fast` after the Responses additions:
  **587/588 library tests passed**; all **192 integration tests** (20 targets) and
  the doc test passed. The sole failure is untouched
  `model_metadata::tests::generated_pricing_is_provider_scoped_and_integer_based`:
  generated input rate `347826` versus the existing dirty golden `388368`.
  The same test fails when run alone. Existing dirty snapshot/golden files were
  not changed to make it pass.
- `cargo check -p octet-ai -p octet-agent --all-targets --locked`: passed.
- `cargo check -p octet-coding-agent --bin octet --locked --offline`: passed.
- The documented aggregation example passed a synthetic fixture checking cold
  exclusion, weighted usage, missing terminals and zero/unreported usage;
  local links in this audit and `docs/providers.md` resolve.
- `cargo clippy -p octet-ai -p octet-agent --all-targets --locked -- -D warnings`:
  passed.
- Scoped Rust formatting and `git diff --check`: passed. No repository-wide
  formatter, metadata refresh or provider call was run.

### Isolated PR-branch verification

Before publication, only the 20 cache-audit source/test/documentation paths were
carried into an isolated worktree based on `origin/main` at `261783aa` (0.9.5).
No unrelated metadata/golden, application, extension or release-archive work was
included. Repeated verification in that tree observed:

- `cargo test -p octet-ai --lib --locked cache_audit -- --nocapture`: six passed.
- `cargo test -p octet-ai --locked --no-fail-fast`: **588 library tests**, all
  **192 integration tests** across 20 targets, and the doc test passed.
- `cargo test -p octet-agent --test agent_run --locked`: **186 passed**.
- `cargo check -p octet-ai -p octet-agent --all-targets --locked`: passed.
- `cargo check -p octet-coding-agent --bin octet --locked --offline`: passed.
- `cargo clippy -p octet-ai -p octet-agent --all-targets --locked -- -D warnings`:
  passed.
- Scoped Rust formatting, whitespace checks and relative documentation links:
  passed.

The earlier pricing-snapshot failure does not reproduce without the unrelated
dirty metadata/goldens; none were overwritten to obtain this pass. The original
checkout's HEAD, index and source-file hashes remained unchanged during packaging.
This is scoped PR verification, not the full workspace/all-features release gate.
Live cache-hit, billing and tier-availability qualification remains **NOT RUN**.

Implementation is confined to cache annotations, the Chat usage alias, opt-in
tracing, regressions/benchmarks and documentation. Existing metadata edits,
untracked application/extension work and release archives remain untouched.
