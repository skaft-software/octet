# Parity telemetry execution evidence

Scope: parity ledger §3. Read-only reference: `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`.

Inspected upstream telemetry README, public schema/types, NOOP, memory adapter,
runner-independent conformance cases and package tests; agent telemetry design
and generated schema. No upstream files changed or TypeScript vendored.
Inspected Octet agent design, session/context contracts and telemetry measurement
methodology, existing JSONL observer, execution/compaction/delegation accounting.

Implementation and verification in progress. This file records observed checks,
not an advance compatibility claim. Existing unrelated work is retained.

## 2026-09-15 row 6.4 — CHANGELOG release extraction and link repair

Adopted telemetry2's `crates/octet-agent/src/telemetry/{spans.rs,schema.rs}`
(unwired) for rows 3.1-3.3. Started the independent repo-tooling row 6.4.

- Added `scripts/changelog.py`: faithful port of upstream
  `packages/coding-agent/src/utils/changelog.ts` (`parse_changelog`,
  `normalize_changelog_links`, `compare_versions`, `get_new_entries`) with an
  argparse CLI (`extract --version`, `since --last`). Repo root base path and
  `skaft-software/ygg` legacy-URL canonicalization; no network access.
- Added `scripts/tests/test_changelog.py` (unittest + importlib + tempfile,
  matching `scripts/tests/test_source_archive.py`).
- Command: `python3 -m unittest scripts.tests.test_changelog -v`
  Observed: `Ran 9 tests in 0.138s / OK` (all 9 pass), including CLI
  extraction, tag-pinned links, directory `tree/` links, legacy canonicalization,
  external/anchor passthrough, and absent-release exit 1.
- Command: `python3 scripts/changelog.py --changelog CHANGELOG.md since --last 0.7.4`
  Observed: prints `## [0.7.6] ...`; `extract --version 9.9.9` exits 1 with
  `release 9.9.9 not found in CHANGELOG.md`.
- Row 6.5 verified present: `scripts/create-source-archive.py` +
  `scripts/tests/test_source_archive.py` (not modified).
- CHANGELOG bullet: "Add `scripts/changelog.py` to extract `## [x.y.z]`
  CHANGELOG releases and repair their relative/legacy Markdown links to
  tag-pinned GitHub source links, with behavioral tests."

## 2026-09-15 rows 3.1-3.4 — vendor-neutral callback telemetry substrate

Adopted telemetry2's `telemetry/{spans.rs,schema.rs}` and finished the rows.
Wired them into the module tree and added docs for `#![deny(missing_docs)]`.

- Files:
  - `crates/octet-agent/src/telemetry.rs` (shared wiring: added
    `pub mod schema; pub mod spans; pub mod testing;` and a module-level note
    that spans observe while `session`/JSONL own accounting).
  - `crates/octet-agent/src/telemetry/spans.rs` (3.1/3.2: explicit callback
    `TelemetryContext`/`TelemetrySpan`, `NOOP_TELEMETRY_CONTEXT`,
    `InMemoryTelemetryContext`, passive `catch_unwind` recording, bounded).
  - `crates/octet-agent/src/telemetry/schema.rs` (3.3/3.6: serializable
    `TelemetrySchema` + `SpanSchema` markers; `CompletionAttributes` keeps
    disjoint buckets, distinct `cache_write_1h_tokens`, `has_uncertain_usage`).
  - `crates/octet-agent/src/telemetry/testing.rs` (3.4: runner-independent
    `conformance_cases()`, `TelemetryAdapterFixture`, `SpanAssertions`).
  - `crates/octet-agent/tests/telemetry_conformance.rs` (behavioral tests).
- Command: `cargo test -p octet-agent --test telemetry_conformance`
  Observed: `test result: ok. 8 passed; 0 failed` (2026-09-15), covering
  callback-once + result/error preservation, explicit status without overwrite,
  attribute merge + ordered events, post-settlement inertness, nested/concurrent
  parentage + end ordering, bound enforcement without dropping callbacks, typed
  nesting, serializable schema round-trip, 1h-cache distinctness, uncertainty
  flag, and the untouched JSONL observer header.
- Hard gate note: no global, no exporter; observer-layer only. Accounting stays
  in `session.rs` (`has_uncertain_usage`, `--telemetry` JSONL). A test in the
  file proves identical business outcomes under NOOP and InMemory.
- CHANGELOG bullet: "Add a vendor-neutral callback telemetry substrate
  (TelemetryContext/TelemetrySpan, NOOP + InMemory, serializable schema and a
  span-assertion harness) that observes without altering usage, cost or
  uncertainty accounting."
- Compile note: `cargo check -p octet-agent` (lib) is green; other test targets
  (`agent_run.rs`, `parity_tools.rs`, `read_concurrency_current.rs`) are red
  from concurrent workers, not from these paths.

## 2026-09-15 row 3.4 conformance suite — expanded and green

- Added to `crates/octet-agent/src/telemetry/testing.rs`: `conformance_cases()`
  (6 cases) + `SpanAssertions` + `TelemetryAdapterFixture` (`Send + Sync` so the
  boxed case futures stay `Send`).
- Added `crates/octet-agent/tests/telemetry_conformance.rs`:
  `cargo test -p octet-agent --test telemetry_conformance`
  Observed 2026-09-15: `test result: ok. 9 passed; 0 failed`, including
  `every_conformance_case_passes_for_the_recording_adapter`,
  `recording_adapter_enforces_bounds_without_dropping_callbacks`,
  `inert_context_runs_callbacks_once_without_recording`, and
  `usage_totals_include_tool_turns_and_summaries_and_keep_1h_distinct`.

## 2026-09-15 row 3.6 — usage totals, cache-hit rate, 1h distinct, uncertainty

- Added `UsageTotals` to `crates/octet-agent/src/telemetry/schema.rs`
  (`from_records`, `cache_hit_rate`, `own_context_total_tokens`). Tool-driving
  assistant turns, compaction summaries, terminal gates and rejected Responses
  turns all fold into one total; mirrored delegated-child usage is a separate
  total so it is not double-counted in the root's own context.
- Added unit test `session::tests::usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty`
  in `crates/octet-agent/src/session.rs`.
  Command: `cargo test -p octet-agent --lib usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty`
  Observed: `test ... ok` (1 passed), proving assistant+summary totals
  (`200+85`), delegated excluded from own context, `cache_write_1h_tokens`
  distinct, `cache_hit_rate() == 50/255`, and that recording uncertainty leaves
  known totals byte-identical while `has_uncertain_usage()` stays true across
  reopen.
- Hard gate honored: uncertainty is preserved, never zero-filled; the JSONL
  observer path and `has_uncertain_usage()` are untouched.
- CHANGELOG bullet: "Report tool-driving and compaction-summary usage in one
  folded total with cache-hit rate and distinct one-hour cache writes, while
  preserving fail-closed usage uncertainty."

## 2026-09-15 row 3.5 — NOT landed (named design gap, no partial edit left)

Deliberately did **not** start a partial edit to the shared 11k-line
`crates/octet-agent/src/agent.rs` generator. Wiring the seven named boundaries
(run, turn, provider request, provider stream, tool, compaction/summary,
delegation) needs generator-owned `begin_typed`/`SpanGuard` scopes across many
`continue 'run`/`break 'run` exits plus a scripted-provider (`wiremock`)
integration test. Nothing is blocked by a missing primitive: the hooks exist and
are exercised by tests.

- Hook tests added in `crates/octet-agent/src/telemetry/schema.rs`
  (`generator_scope_guard_nests_typed_spans_and_records_completion_usage`,
  `dropped_scope_guard_settles_as_error`).
  Command: `cargo test -p octet-agent --lib telemetry::`
  Observed: `test result: ok. 10 passed; 0 failed` (2 schema + 8 observer).
- Dead-code hooks (`begin_typed`, `CompletionAttributes::record`,
  `SpanGuard::context`) carry `#[allow(dead_code)]` + row-3.5 comments;
  `cargo check -p octet-agent` emits no telemetry warnings.
- Exact remaining work recorded in `docs/parity/telemetry.md` §3.5, including the
  `SpanGuard`-settles-on-drop pitfall for `continue 'run` continuations.

## 2026-09-15 docs

- Added `docs/telemetry.md` (two-surface model, accounting guarantees, the
  observer substrate, schema, harness, and boundary status).
- Added `docs/parity/telemetry.md` (per-row 3.1-3.6 outcomes, upstream anchors,
  tests, and the exact 3.5 gap).
- `docs/parity/README.md` left to the integrator: other owners edit their rows in
  the same table.

## 2026-09-15 final verification (observed)

- `cargo test -p octet-agent --test telemetry_conformance` → `9 passed; 0 failed`
- `cargo test -p octet-agent --lib telemetry::` → `10 passed; 0 failed`
- `cargo test -p octet-agent --lib usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty` → `1 passed`
- `cargo check -p octet-agent` → green, no telemetry warnings
- `python3 -m unittest scripts.tests.test_changelog` → `Ran 9 tests ... OK`
- Note: other octet-agent test targets (`agent_run.rs`, `parity_tools.rs`,
  `read_concurrency_current.rs`) and `octet-ai` were intermittently red from
  concurrent workers during this session; none of those failures are in these
  paths.

## CHANGELOG-ready bullets

- Add a vendor-neutral, callback-based telemetry substrate
  (`TelemetryContext`/`TelemetrySpan` with NOOP and in-memory implementations,
  serializable span schema and a span-assertion harness) that observes without
  ever replacing durable usage, cost or uncertainty accounting.
- Report tool-driving and compaction-summary usage in one folded total with
  cache-hit rate and one-hour cache writes kept distinct, preserving fail-closed
  usage uncertainty.
- Add `scripts/changelog.py` to extract `## [x.y.z]` CHANGELOG releases and
  repair their relative/legacy Markdown links to tag-pinned GitHub source links,
  with behavioral tests.

## Rows landed / blocked (final)

- Landed: 3.1, 3.2, 3.3, 3.4, 3.6, 6.4.
- Not landed: 3.5 (design gap; substrate + hooks + tests present, generator
  wiring + scripted-provider test remaining). 6.5 verified already present.

## 2026-09-15 CHANGELOG policy doc

- Added `docs/CHANGELOG-POLICY.md`: the release-heading and link format that
  `scripts/changelog.py` relies on, plus the `extract`/`since` CLI contract.
