# Telemetry

Octet has two independent telemetry surfaces. They never share a lifecycle and
neither is an accounting authority.

1. The optional **`--telemetry` JSONL observer** (`TelemetryObserver`) records
   operational facts alongside, but independently of, durable usage/pricing
   accounting. `octet-agent`'s `Session`, not the observer, is the authoritative
   record of tokens, cost and usage uncertainty.
2. **Vendor-neutral observer spans** in `octet_agent::telemetry::{spans, schema}`.
   These are explicit, callback-owned, and purely observational.

## Durable accounting (authoritative)

`Session` persists one JSONL record per accepted provider result. Usage buckets
are disjoint and provider-reported:

- `input_tokens` is uncached prompt traffic;
- `cache_read_tokens` and `cache_write_tokens` are separate additions;
- `cache_write_1h_tokens` is a **subset of** `cache_write_tokens` (never added
  again);
- `reasoning_tokens` is a **subset of** `output_tokens`.

Unknown usage is recorded separately as `usage_uncertainty` records that carry
only bounded host-selected identifiers. `Session::has_uncertain_usage()` is
fail-closed: while it is true, known totals are only subtotals and cumulative
token/cost ceilings refuse to proceed. Uncertainty survives success
checkpoints, checkout, compaction and reopen; it is never rewritten as
fabricated zero usage.

`TelemetryObserver` (`octet_agent::telemetry::TelemetryObserver`) appends one
bounded JSON object per line to an owner-only (`0o600`) file when a caller
explicitly installs it. It records operational facts and hashes, never prompts,
tool arguments, tool output, credentials or provider payloads. Streaming deltas
are aggregated in memory and are not written.

Writes run on an ordered worker with a **256-record / 1 MiB** admission budget,
including in-flight writes. Saturation rejects observations, not agent work or
authoritative accounting. `status()` exposes rejected records, failures and
pending work; `flush()` and `shutdown(Duration)` report incomplete delivery.
Hosts must surface those diagnostics at a lifecycle boundary. Shutdown is
bounded, but cannot cancel an uninterruptible OS write; a timed-out worker may
outlive its caller, and pending records are not claimed delivered.

## Observer spans

`octet_agent::telemetry::spans` is a small, dependency-light substrate:

- `TelemetryContext` is an explicit span factory. It is created by a caller and
  passed down; there is no process-global current span, no thread-local, and no
  exporter.
- `TelemetryContext::start_span(options, callback)` invokes the callback exactly
  once, immediately, and owns the callback's future until settlement. The Rust
  `Result` and its error value are preserved; a dropped future settles the span
  as an error.
- `TelemetrySpan` records attributes, ordered events and a terminal status. All
  calls are inert after settlement and inert entirely under the no-op context.
- Recording is passive. Every adapter call is wrapped so that a panicking or
  unreadable payload can never change business behavior.

`NOOP_TELEMETRY_CONTEXT` (also `TelemetryContext::default()`) is the shared inert
context. `InMemoryTelemetryContext` is the bounded, process-local reference
recorder used by tests.

> **Hard guarantee.** No-op or in-memory *observer* spans never replace durable
> usage, cost or uncertainty accounting. Dropping to
> `NOOP_TELEMETRY_CONTEXT` loses observations only. The `--telemetry` JSONL path
> and `has_uncertain_usage()` are unchanged by the observer substrate.

## Serializable schema

`octet_agent::telemetry::schema` provides:

- `TelemetrySchema` / `SpanDefinition` / `AttributeDefinition` /
  `ParentDefinition`: plain `serde` data describing span names, parents, start
  attributes, completion attributes and events.
- `SpanSchema` marker types (`RunSpan`, `TurnSpan`, `ProviderRequestSpan`,
  `ProviderStreamSpan`, `ToolSpan`, `CompactionSpan`, `SummarySpan`,
  `DelegationSpan`) plus `TelemetryContext::start_typed`.
- `CompletionAttributes`, which carries the reported usage buckets with
  `cache_write_1h_tokens` distinct, a derived `cache_hit_rate`, and an explicit
  `has_uncertain_usage` flag.
- `UsageTotals`, which folds `UsageRecord`s into one total. Tool-driving
  assistant turns, compaction summaries, terminal gates and rejected Responses
  turns all contribute. `own_context_total_tokens` excludes mirrored
  delegated-child records, which are reported in `total_tokens` instead.

`cache_hit_rate` (on both `octet_ai::Usage` and `UsageTotals`) is `None` when
there is no reported prompt traffic: missing data is unavailable, not a
fabricated zero.

## Span assertion harness

`octet_agent::telemetry::testing` exports a runner-independent conformance suite
(`conformance_cases()`), a `TelemetryAdapterFixture` trait, and
`SpanAssertions`. The suite covers callback-once semantics, error preservation,
explicit-status precedence, attribute merge + ordered events, post-settlement
inertness, nested/concurrent parentage with end-ordering, and bound enforcement
without dropping callbacks. `crates/octet-agent/tests/telemetry_conformance.rs`
runs it against `InMemoryTelemetryContext` and additionally asserts the inert
context and the untouched JSONL path.

## Runtime instrumentation

The agent installs explicit run, turn, provider-request, provider-stream, tool,
compaction/summary, and delegation spans at their owning execution boundaries.
`TelemetryContext::begin_typed` and `SpanGuard::context` carry parentage through
the generator; a dropped guard settles as an error. An embedding host supplies
its observer context explicitly. These measurements do not replace durable
usage or uncertainty accounting, and an inert observer changes no run behavior.
