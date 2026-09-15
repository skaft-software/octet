# Parity detail — telemetry (ledger rows 3.1–3.6)

Reference (read-only): `earendil-works/pi` @
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`. No upstream file is edited and no
TypeScript is vendored. Upstream anchors are the telemetry package
(`packages/telemetry/src/{index,noop,memory,testing/conformance}.ts`), the agent
harness telemetry (`packages/agent/src/harness/telemetry.ts`) and the generated
schema (`packages/agent/docs/telemetry-schema.md`).

## Summary

| Row | Required behavior | State |
| --- | --- | --- |
| 3.1 | Explicit callback-based vendor-neutral TelemetryContext/TelemetrySpan, no global/exporter | Landed |
| 3.2 | NOOP and InMemory implementations | Landed |
| 3.3 | Serializable typed span/schema definitions | Landed |
| 3.4 | Span assertion harness | Landed |
| 3.5 | Provider/stream/tool/turn/compaction/summary/delegation span boundaries | Not landed |
| 3.6 | Tool and summary usage in totals, cache-hit rate, distinct cacheWrite1h; preserve uncertainty | Landed |

## 3.1 Explicit callback-based vendor-neutral substrate

Upstream `TelemetryContext.startSpan(options, callback)` admits the callback
once and owns its promise. Octet ports this to
`crates/octet-agent/src/telemetry/spans.rs`:

- `TelemetryContext::start_span` / `start_span_sync` invoke the callback exactly
  once and hold a `SpanGuard` until settlement.
- `TelemetrySpan::{set_attributes, add_event, set_status}` record passively.
- There is no `AsyncLocalStorage` equivalent: no task-local, thread-local or
  global current span. Parentage is derived only from the explicit context a
  callback receives; concurrent siblings cannot overwrite each other.
- There is no exporter. The only bundled adapter is in-memory.

## 3.2 NOOP and InMemory

- `NOOP_TELEMETRY_CONTEXT` (and `TelemetryContext::default()`) is the shared
  inert context. Callbacks still run exactly once and errors are preserved.
- `InMemoryTelemetryContext` is a bounded, process-local recorder with detached
  snapshots, deterministic ids and end ordering.

**Hard gate.** These are observer no-ops only. Durable `Session` usage/cost
accounting, the `--telemetry` JSONL observer and fail-closed
`has_uncertain_usage()` are untouched. Tests prove identical business outcomes
under both contexts.

Tests: `crates/octet-agent/tests/telemetry_conformance.rs`
(`inert_context_runs_callbacks_once_without_recording`,
`recording_adapter_enforces_bounds_without_dropping_callbacks`,
`inert_and_in_memory_spans_never_change_accounting_outcomes`,
`jsonl_observer_path_is_untouched_and_records_usage_uncertainty`).

## 3.3 Serializable typed span/schema definitions

Upstream `TelemetrySchemaDefinition`, `TelemetrySpanDefinition`,
`TelemetryAttributeDefinition` and `ParentDefinition` are ported to
`crates/octet-agent/src/telemetry/schema.rs` as `serde` types plus a compile-time
`SpanSchema` bridge. `agent_telemetry_schema()` returns the serializable
definition for every span name. `TelemetryContext::start_typed` runs a callback
inside a typed span.

Tests: `serializable_schema_and_completion_usage_preserve_disjoint_buckets`,
`typed_instrumentation_nests_children_under_the_typed_span`.

## 3.4 Span assertion harness

Upstream `createTelemetryAdapterConformance` is ported as a runner-independent
suite in `crates/octet-agent/src/telemetry/testing.rs`
(`conformance_cases()`, `TelemetryAdapterFixture`, `SpanAssertions`). Cases cover
callback-once/result preservation, synchronous and asynchronous error
preservation, explicit-status precedence, attribute merge and ordered events,
post-settlement inertness, nested/concurrent parentage with end ordering, and
bound enforcement without suppressing callbacks.

Test: `every_conformance_case_passes_for_the_recording_adapter`.

## 3.5 Span boundaries — landed

All seven named boundaries (run, turn, provider request, provider stream, tool,
compaction/summary, delegation) are wired into the `octet-agent` run generator
through `TelemetryContext::begin_typed` / `SpanGuard::context`, and behavioral
boundary tests exist:

- `crates/octet-agent/src/agent.rs:6389` `begin_typed::<RunSpan>`,
  `agent.rs:6462` `TurnSpan`, `agent.rs:6863` `ProviderRequestSpan`,
  `agent.rs:7006` `ProviderStreamSpan`, `agent.rs:7770`/`agent.rs:7870`
  `ToolSpan`, `agent.rs:4784`/`agent.rs:4946` `CompactionSpan`,
  `agent.rs:4540`/`agent.rs:4541` `SummarySpan` plus its provider request.
- `crates/octet-agent/src/delegation.rs:3214` `begin_typed::<DelegationSpan>`,
  with the child run re-parented through `agent.set_telemetry_context(...)`.
- Completion attributes are recorded at `agent.rs:4659`, `agent.rs:7006`, and
  `agent.rs:7195` through `CompletionAttributes::record`.
- Tests: `typed_spans_nest_run_turn_provider_and_tool_boundaries`
  (`crates/octet-agent/tests/agent_run.rs:9411`),
  `typed_spans_label_failed_runs_without_changing_accounting`
  (`agent_run.rs:9513`), `typed_spans_cover_compaction_and_summary_boundaries`
  (`agent_run.rs:9598`), `delegation_span_owns_the_child_run_and_nests_child_spans`
  (`crates/octet-agent/src/delegation.rs:7343`), and
  `typed_instrumentation_nests_children_under_the_typed_span`
  (`crates/octet-agent/tests/telemetry_conformance.rs:113`).

`SpanGuard` settles on drop as an error, so every successful `continue 'run` /
`break 'run` inside a generator scope settles the guard explicitly; a bare guard
around the turn loop would mislabel tool-continuation turns.

## 3.6 Tool and summary usage in totals, cache-hit rate, distinct cacheWrite1h, uncertainty

- `CompletionAttributes::usage` carries every reported bucket with
  `cache_write_1h_tokens` distinct, a derived `cache_hit_rate`, and an explicit
  `has_uncertain_usage` flag.
- `UsageTotals::from_records` folds durable `UsageRecord`s. Tool-driving
  assistant turns, compaction summaries, terminal gates and rejected Responses
  turns contribute to one total; mirrored delegated-child usage is a separate
  total (`total_tokens`) and is excluded from `own_context_total_tokens`.
- `cache_hit_rate` returns `None` without reported prompt traffic.

Tests:
`usage_totals_include_tool_turns_and_summaries_and_keep_1h_distinct`
(integration) and
`session::tests::usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty`
(unit, real `Session`; proves uncertainty leaves known totals unchanged and
survives reopen).

## Evidence

Observed runs and commands are recorded in
`docs/swarm-audit/EXECUTION-parity-telemetry.md`.
