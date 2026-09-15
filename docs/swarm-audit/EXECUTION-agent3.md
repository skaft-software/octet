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
