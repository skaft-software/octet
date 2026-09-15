# EXECUTION — agent3 (crates/octet-agent, explore/implement profile)

Base commit: df5a7e809715961b9344af6b52e43a6ca48f56b3
Exclusive paths: `crates/octet-agent/**` except `src/artifact.rs` (roadmap3),
`src/extension_api_v03.rs` (ext3), `src/telemetry/**` (read-only unless a task says otherwise).

---

## PRIORITY 0 — mechanical repair of the 17 mangled `ToolDef` sites (BLOCKER)

### 2026-02-14 (KST) — root cause found, repair applied

Root cause (differs slightly from the hand-off description): `constrained_sampling` is a
**newly added field** on `octet_ai::ToolDef`, landed in the shared dirty worktree by the
providers worker (`crates/octet-ai/src/types.rs:1311`,
`pub constrained_sampling: Option<ConstrainedSampling>`; absent at `HEAD`). Every
`ToolDef { .. }` struct literal therefore needs the field. A killed worker tried to add it at
17 sites and inserted two 4-space-indented stray lines per site — one *before*
`octet_ai::ToolDef {` and one directly after it — never inside the literal. Exactly two sites
(`IdentityReadGate` → `ReadTool.definition()`, `DurableBashProbe` →
`octet_agent::BashTool.definition()`) got the single stray line and need no field (no literal).

Repair command (deterministic, re-derived the pristine `HEAD` content via `git show` and
re-applied only the intended change; no `git checkout` used):

```bash
python3 /tmp/repair3.py    # deletes every 4-space-indented stray `constrained_sampling: None,`
                           # line, then inserts `            constrained_sampling: None,`
                           # as the last field of each ToolDef literal that lacks it
```

Observed output:

```
crates/octet-agent/tests/read_concurrency_current.rs: inserted 5 fields
crates/octet-agent/tests/agent_run.rs: inserted 10 fields
```

Resulting diff is exactly 15 inserted field lines + the preserved `mod extension_hooks;`
lines at the tail of `agent_run.rs` (owned by ext3; untouched):

```
 crates/octet-agent/tests/agent_run.rs                | 13 +++++++++++++
 crates/octet-agent/tests/read_concurrency_current.rs |  5 +++++
 2 files changed, 18 insertions(+)
```

Sites repaired (post-repair line numbers, `constrained_sampling: None,` as last literal field):
`read_concurrency_current.rs` 314, 490, 545, 753, 981; `agent_run.rs` 3322, 4175, 4213, 4271,
4301, 4343, 4438, 4467, 5568, 5613. Single-stray deletions: `read_concurrency_current.rs:864`
(`IdentityReadGate`), `agent_run.rs:6793` (`DurableBashProbe`).

### Observed verification (verbatim)

```bash
$ cargo check -p octet-agent --all-targets 2>&1 | tail -40
warning: missing documentation for the crate
   --> crates/octet-agent/tests/windows_process_current.rs:1:1
...
warning: unused import: `futures_util::StreamExt`
  --> crates/octet-agent/tests/read_concurrency_current.rs:14:5
   |
14 | use futures_util::StreamExt;
   |     ^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: `octet-agent` (test "read_concurrency_current") generated 1 warning
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 37.38s
```

```bash
$ cargo test -p octet-agent --lib 2>&1 | tail -10
test tools::bash::tests::cancellation_kills_the_child_process_tree ... ok
test agent::tests::provider_usage_entry_work_is_bounded_by_the_unmeasured_suffix ... ok
test effect::tests::file_tool_payload_contract_fits_streaming_intent_boundary ... ok

test result: ok. 533 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 11.98s
```

**REMAINING ERROR SET: EMPTY.** `cargo check -p octet-agent --all-targets` is clean (warnings
only); the 17 `expected identifier` / 17 `unexpected token` / 24 cascading E0277 are gone, and
no E0063 (missing field) survives in this crate.

Behavioural confirmation of the repaired suites:

```bash
$ cargo test -p octet-agent --test read_concurrency_current --test agent_run 2>&1 | tail -20
     Running tests/agent_run.rs (target/debug/deps/agent_run-...)
test result: ok. 136 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 29.95s

     Running tests/read_concurrency_current.rs (target/debug/deps/read_concurrency_current-...)
running 5 tests
test abort_during_a_read_wave_keeps_pairing_and_stops_future_turns ... ok
test text_image_and_audio_reads_retain_identity_in_ordered_slots ... ok
test read_waves_stop_at_mutation_barriers_and_serialize_hooks ... ok
test host_read_waves_are_bounded_and_keep_effect_and_result_order ... ok
test non_read_effects_are_ordered_barriers_without_parallel_admission ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
```

Note for the parent: the unused `futures_util::StreamExt` import at
`read_concurrency_current.rs:14` is pre-existing at `HEAD` (the repair diff contains zero
deletions), so it was left untouched.

### CHANGELOG-ready bullet

- fix(octet-agent): repair `ToolDef.constrained_sampling` initialisation in the two
  concurrency/agent-run test suites (17 sites were left with stray field lines that broke
  every downstream `cargo check`).
START 2026-09-15T15:43:44Z agent5 alive
STEP 2026-09-15T15:48:20Z agent5: wired run/turn/provider-request/provider-stream/tool/compaction/summary spans in agent.rs; cargo check hit a TRANSIENT error from another worker in src/tools/durability.rs (not my path); retrying.
STEP 2026-09-15T15:49:12Z agent5: agent.rs + delegation.rs boundary wiring compiles GREEN (`cargo check -p octet-agent --lib` finished, 0 errors). Transient durability.rs error from another worker resolved itself.
START 2026-09-15T16:28:32Z agent6 alive
START 2026-09-15T16:50:12Z agent7 alive
STEP 2026-09-15T16:55:55Z agent7: P0 core landed in delegation.rs (Detached/AwaitingApproval states, durable fleet roster fleet.json, detach_run at run boundary, reattach_detached on run start, delta usage mirroring, approval parking) + agent.rs run-end/Run::drop detach. cargo check blocked by another worker mid-edit in crates/octet-ai (E0027/E0061).

START 2026-09-15T17:13:24Z agent8 alive

STEP 2026-09-15T18:05Z agent8: C1 applied. docs/parity/telemetry.md §3.5 rewritten from
"NOT landed" to "landed" with the live generator locations (agent.rs 6389/6462/6863/7006/
7770/7870 ToolSpan/4784/4946 CompactionSpan/4540/4541 SummarySpan+request, delegation.rs 3214
DelegationSpan, CompletionAttributes::record at agent.rs 4659/7006/7195) and the five boundary
tests. Stale `#[allow(dead_code)]` removed from telemetry/schema.rs:156/261 and
telemetry/spans.rs:244 (replaced by doc comments stating the real contract);
`cargo check -p octet-agent --all-targets` = 0 errors, 0 warnings from those items.

Observed boundary test output (run by me, not claimed):
```
$ cargo test -p octet-agent --test agent_run --test telemetry_conformance typed_spans
running 3 tests
test typed_spans_nest_run_turn_provider_and_tool_boundaries ... ok
test typed_spans_label_failed_runs_without_changing_accounting ... ok
test typed_spans_cover_compaction_and_summary_boundaries ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 136 filtered out

$ cargo test -p octet-agent --test telemetry_conformance typed_instrumentation_nests_children_under_the_typed_span
test typed_instrumentation_nests_children_under_the_typed_span ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out

$ cargo test -p octet-agent --lib delegation_span_owns_the_child_run_and_nests_child_spans
test delegation::tests::delegation_span_owns_the_child_run_and_nests_child_spans ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 534 filtered out
```
That last test was RED before this step for a reason unrelated to my task (`ai error:
Unsupported error: Reasoning is unsupported`): its fixture requested the Astra model with no
explicit effort and without `reasoning.max_effort = Ultra`, while `octet-ai`'s
`validate_reasoning_selection` (crates/octet-ai/src/validate.rs:64-80) rejects exactly that.
Test-only fix in my file: fixture now mirrors the sibling wire-contract test (max_effort=Ultra,
template reasoning = Effort(Ultra)).

STEP 2026-09-15T18:35Z agent8: P0 session-scoped delegation lifetime — verified + repaired the gaps.
Fixes in crates/octet-agent/src/delegation.rs (my path):
1. `reattach_detached` now also clears the run-scoped `detached` marker on a live worker
   (previously only `Detached`/`AwaitingApproval` records were visited, so a live in-process
   worker stayed `detached: true` forever after its first run boundary — a later turn's
   `list_agents` reported it as detached).
2. Reattachment rebuilds the worker command channel when the record no longer owns a receiver
   (a worker that parked at the approval boundary, or a live task that already settled, has no
   receiver; the old code attached a task to a closed queue).
3. `prepare_owning_run` (root) reactivates `root_active` for the next owning run and sweeps only
   *retired* records (status `Shutdown` with no buffered work), so an explicit stop cannot brick
   the session and cannot permanently burn a worker name; execution capacity is NOT handed back
   (the cap cannot drift up across the boundary).
4. Idle workers release their execution slot (`initial_permit.take()` when no work is queued);
   a reattached worker re-acquires through `acquire_follow_up_permit` when work arrives.
5. A spawn that reuses a session-scoped worker name now fails with a bounded, actionable
   diagnostic naming the worker and the tool that resumes/stops it.
Test contract updated (same file) to the new lifetime: `owning_run_restart_reactivates_root_without_recycling_session_capacity`,
`extension_spawn_idempotency_survives_the_owning_run_without_a_duplicate_worker` (same key after the
boundary returns the SAME agent id, one worker; a different key spawns its own), plus new tests
`reattachment_is_bounded_by_the_remaining_execution_slots` and
`reattachment_fails_closed_when_the_child_session_is_gone`.
NOTE: `cargo test -p octet-agent` is currently blocked by another worker's in-flight
crates/octet-ai/src/responses_ws.rs edit (E0596); retrying.
