# Prompt-cache warming

Cache warming is an extra **billable inference request**, not a guaranteed cache
hit. octet defaults to `streaming`: it can refresh an eligible prompt prefix
while an agent run is waiting on its provider or tools. `idle` also allows
refreshes while the live host waits for your next input. `off` disables both.

## Choose a policy

```toml
# ~/.octet/config.toml — user configuration only
cache_warming = "streaming"       # off | streaming | idle
show_cache_miss_notices = false    # default; accounting is always recorded
```

`OCTET_CACHE_WARMING` overrides the user file; `--cache-warming MODE` overrides
both. Trusted project configuration cannot change either user-level setting.
The mode is not restored from session history.

`/cache-warming` reports the current mode, scheduler state, next decision,
refresh estimate, avoided-miss estimate, expected savings, and known session
refresh spend. `/cache-warming off|streaming|idle` saves the user preference and
reconciles the live agent immediately, including during provider/tool waits.
Turning `off` cancels the existing prefix and pending refresh; re-enabling after
`off` waits for the next real request rather than reviving that prefix. Existing and
future delegated children share the live user-selected mode. `/session` and
`/cache` include the same diagnostics even when notices are off. `/status`
includes the mode.

With `show_cache_miss_notices = true`, successful refreshes and material cache
misses produce brief ordinary notices, not assistant messages or success
badges. An extension-forced refresh is labeled as an override. Resumed
interactive sessions mention prior refresh usage and recorded overrides.
Plain and print diagnostics go to stderr; **print stdout remains response-only**.
Active interactive reports and RPC `get_state` use live, payload-free scheduler
diagnostics rather than the state captured before the run started.

## Scheduling and eligibility

The core owns one cancellation-safe scheduler. Hosts poll its idle driver below
input and shutdown work; dropping and recreating that waiting future does not
lose an in-flight request or restart its timer. Interactive, chronological
plain, JSONL RPC, and the retained native-host session drive idle maintenance.
A one-shot print invocation does not wait after its run just to warm a cache.

A route must explicitly declare the selected retention's cache lifetime. Provider
names, an Anthropic-compatible protocol, or an arbitrary gateway URL alone do
not establish it. Refreshes require a safely replayable request whose effective
provider output limit is one token. Unknown lifetime, incompatible reasoning or
output limits, an unresolved historical refresh, exhausted session ceilings, or
changed context prevent warming. Usable pricing and prior prompt usage are
required for the default economic decision; a hook may override unavailable
economics, but never eligibility or a hard ceiling.

The timer runs at 90% of the declared TTL, keeping at least ten seconds before
expiry: a five-minute lifetime schedules a decision after four minutes thirty
seconds. The streaming profile has a one-hour horizon and assumes 100%
continuation probability; idle has a thirty-minute horizon and assumes 15%.
The economic decision compares the expected avoided miss penalty with refresh
cost and requires at least $0.05 expected savings. Stop reasons and unavailable
economics remain inspectable. Negotiated extension decision hooks can override
the economic choice, but not host authority, provider compatibility, or hard
session limits.

A refresh replays the **exact request snapshot** — system, messages, tools,
reasoning, retention, session affinity and other request controls — changing
only the output-token cap to one. There is **no synthetic suffix**, breakpoint
move, tool execution, or conversation mutation. Generated text, reasoning,
media and tool-call output are discarded. This replaces the older experimental
one-shot warm API and its synthetic-user-instruction design.

New prompts, model/reasoning changes, branch/session/context changes, tool
schema changes, reload, shutdown and `off` cancel stale work. A settled
`streaming` run stops warming; an `idle` run may retain its replayable prefix.
The native host retains only its latest settled app while waiting for another
frame, drops it before opening a new session writer, and never emits extra
protocol events for a request that already sent `final_result`.

## Accounting and failure behavior

`CacheWarmed { usage, cost, extension_override }` describes maintenance, not an
assistant turn. Provider-reported input, cache-read, cache-write, output and
exact cost count toward **session totals**, including ceilings, resume, export
and catalog accounting. They never change the selected head, model-visible
context, assistant output-token count, latest-turn cache-hit ratio, timing,
throughput, or context-size estimate.

The durable ledger stores payload-free `cache_warm` lifecycle records
(`started`, `completed`, `timed_out`, `failed`) and separate `usage` records with
`kind: cache_warm`. Lifecycle records include the request-prefix anchor and
whether an extension overrode the decision; older experimental records remain
readable. Prompt bodies, generated output, provider errors, URLs and credentials
are never stored in those maintenance records.

Refresh transport failures and deadlines are best-effort and do not fail the
user's main run. There are no immediate/provider retries; later scheduled
decisions remain subject to the original age horizon and session ceilings.
A refresh not yet dispatched when its cache deadline expires is cancelled.
An interrupted or failed operation after possible dispatch records usage
uncertainty, not an invented zero. An unsettled `started` record is uncertain on
reopen and disables further warming without stopping an otherwise eligible real
run. Known cost becomes a known subtotal, and unavailable exposure bounds fail
hard ceilings closed. Persistence errors are surfaced rather than silently
forgetting exposure.

Virtual-clock, fake-transport and frontend tests exercise these contracts.
Live provider cache-hit, prefix-reuse and pricing behavior has **not** been
qualified; no real-provider cost or latency savings are claimed.
