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

STEP 2026-09-15T19:20Z agent8: launchable child-session handle (openall4's missing primitive) LANDED host-side.

New host API (crates/octet-agent/src/delegation.rs, crates/octet-agent/src/agent.rs):
- `pub fn octet_agent::resolve_launchable_child_session(session_directory: &Path, reference: &str)
   -> Result<LaunchableChildSession, DelegationError>` — resolves `agent-session:<sha256>` from the
  session-owned durable roster (`<session-dir>/delegation/fleet.json`) with NO live agent, so a
  separate process can launch a worker pane. Fails closed with a bounded reason for: unknown handle,
  malformed handle (not exactly 64 lowercase hex), a worker parked at the approval boundary, a
  vanished transcript, an unreadable/foreign roster.
- `Agent::session_delegation() -> Option<SessionDelegationHandle>` and
  `SessionDelegationHandle::launchable_child_session(reference)` — the in-process resolver, which
  additionally knows process-local liveness (`AgentRecord.live_task`, cleared by a Drop guard on every
  `run_worker` exit path) and refuses a worker a live task still owns.
- `LaunchableChildSession { reference, session_path, agent_id, agent_path, status }` — the token is
  opaque/path-free/argv-safe (`agent-session:` + 64 lowercase hex, no `/`, no space, no secret);
  `session_path` is host-only and must never be published to an extension, a notice, or a command line.
- Every `list_agents` / extension `agent/list` row now carries `handle`, `launchable`, `launch_blocked`
  and `live_task`, so the extension can render a pane vs an explicit blocked reason without new RPC.
- Parked (`awaiting_approval`) workers are NOT launchable; the host refusal is the well-founded basis
  for the extension's existing `worker_awaiting_approval` refusal.

REMAINING PRIMITIVE (not mine, does not exist yet — named blocker, not a silent gap):
CLI-side wiring in `crates/octet-coding-agent/src/session_store.rs::path_by_id` (accept
`agent-session:<sha256>` and call `resolve_launchable_child_session`, then hand the resolved host-only
child path to the launcher). Until that lands, `/subagents open-all` worker panes stay BLOCKED with
that exact wiring named. Documented in docs/subagents.md (that paragraph previously claimed no
host-side resolver exists; corrected).

Observed output (run by me):
```
$ cargo test -p octet-agent --test delegation              -> 11 passed; 0 failed
$ cargo test -p octet-agent --lib                          -> 539 passed; 0 failed; 1 ignored
$ cargo test -p octet-agent --test agent_run -- --skip websocket --skip qualified_codex_ws --skip native_compaction_calls_bound_reopening
                                                           -> 136 passed; 0 failed; 3 filtered out
$ cargo test -p octet-agent --test telemetry_conformance    -> 9 passed; 0 failed
$ cargo test -p octet-agent --test extension_api_v03_conformance -> 5 passed
$ cargo test -p octet-agent --test extension_api_0_1_conformance -> 4 passed
$ cargo check --workspace --all-targets --locked           -> Finished (green)
```
Pre-existing red, NOT caused by my change and NOT mine to touch:
- `crates/octet-agent/tests/agent_run.rs::websocket_connection_limit_is_retried_by_agent` fails with
  `StreamProtocol(ResponseNotResumable { detail: "websocket_connection_limit_reached" })` — ai8's
  in-flight `crates/octet-ai/src/responses_ws.rs` work (host said do not chase).
- `crates/octet-agent/tests/api_v03_runnable.rs` fails with
  `-32011 extension capability mismatch: theme_selection` (extension/theme capability negotiation,
  not delegation).

TASK 2 status (honest): NOT started — no code written for parity 1e.2 durability, §4 consumers
4.7/4.8/4.10/4.13, #264, #265, #267. The P0 (TASK 1) plus the corrected telemetry doc and the
launchable handle consumed the whole budget; nothing half-applied was left in the tree.

CHANGELOG-ready bullets:
- feat(octet-agent): session-scoped delegation lifetime. A spawned worker's record is owned by the
  session, not the run: the end of the owning run (completed, aborted, or dropped) journals an explicit
  `run_detached` boundary, keeps the worker discoverable, and persists id, name, task, child-session
  reference, status, and consumed budget to a durable roster (`fleet.json`) that survives a process
  restart. A later turn reattaches the fleet (`wait_agent`, `send_message`, `followup_task`,
  `interrupt_agent` all work on it) and never spawns a duplicate for the same extension idempotency
  key. Retirement stays explicit and diagnosable.
- fix(octet-agent): a reattached worker no longer keeps a stale `detached` marker (the run-scoped
  marker is cleared for a live in-process worker) and reattachment rebuilds the command channel when a
  settled/parked record no longer owns a receiver, instead of attaching work to a closed queue.
- fix(octet-agent): execution caps hold across the turn boundary. Idle workers release their execution
  slot, reattachment acquires one slot per record and leaves the excess visibly `detached`, and a new
  owning run never hands back capacity that a surviving worker still holds.
- fix(octet-agent): a root owning run reactivates after an explicit stop and sweeps only *retired*
  (shut-down, non-resumable) records, so an explicit stop can neither brick the session nor burn a
  worker name forever.
- feat(octet-agent): unattended mutation fails closed. A detached worker that needs an approval
  authority it no longer has parks in a durable `awaiting_approval` state with a bounded reason
  (never proceeds, never blocks forever); it is not launchable, and a later turn supplies the decision.
- feat(octet-agent): session-owned launchable child-session handle. `resolve_launchable_child_session`
  / `Agent::session_delegation()` resolve the opaque, path-free, argv-safe `agent-session:<sha256>`
  reference to a host-only launchable transcript; `agent/list` rows carry `handle`, `launchable`,
  `launch_blocked`. A live in-process worker, a parked worker, and a vanished transcript refuse with a
  bounded reason. Remaining primitive: CLI `path_by_id` wiring for `octet --resume <reference>`.
- docs(parity/telemetry): row 3.5 corrected from "NOT landed" to landed, with the live generator
  locations and the five boundary tests; the stale `#[allow(dead_code)]` on
  `TelemetryContext::begin_typed`, `CompletionAttributes::record`, and `SpanGuard::context` removed.

CONTRACT FOR THE EXTENSION (openall4) — exact host surface to call:
1. `agent/list` row fields: `status.state` ∈ {pending, running, completed, interrupted, timed_out,
   failed, detached, awaiting_approval, shutdown}; plus `detached` (bool), `live_task` (bool),
   `handle` (string|null, `agent-session:<sha256>`), `launchable` (bool), `launch_blocked` (string|null,
   bounded reason), `diagnostic` (string|null, e.g. "detached worker could not be reattached: ...").
2. `agent/wait` returns when no record is pending/running; a detached record with no live task is not
   running, so wait settles instead of stalling. `agent/spawn` with a previously used idempotency key
   returns the SAME `agent_id`/`agent_path` for the session-owned worker (no duplicate), and fails with
   `-32002` only when the key is reused with different input. A reused public `task_name` that is held
   by an existing worker is refused with a bounded message naming the holder and `followup_task`.
3. `agent/follow_up` on a settled reattached worker returns `delivery: "new_run"`; on a running worker
   `"follow_up"`. `agent/message` returns `steering`/`queued`. `agent/interrupt` sets `interrupted`
   (explicit stop; `shutdown` only for owner/team teardown).
4. Parked workers: render `awaiting approval` with the bounded reason; refuse `subagent_continue`
   (`worker_awaiting_approval`), keep `subagent_stop`. After the owning session reattaches, the record
   is `pending` again (idle, no work retried): the parent must re-issue the work through
   `agent/follow_up`, which is what supplies the missing authority.
5. Launchable panes: use `handle` + `launchable`; when `launchable` is false read `launch_blocked`
   verbatim (it distinguishes a live in-process worker from an approval park from a vanished
   transcript). Do not fabricate a resume; the launcher still needs the CLI `path_by_id` wiring.

agent6: session-scoped delegation lifetime landed

START 2026-09-15T17:42:57Z agent9 alive

START 2026-09-15T17:53:01Z agent10 alive
START 2026-09-15T17:55:32Z agent11 alive

START-of-directed-work 2026-09-15T17:56:07Z agent11 — MAINTAINER DIRECTIVE: remove ls/find/grep tools; priority above #175
Ownership grant received: crates/octet-agent/src/tools/**, src/tool.rs, tests/parity_tools.rs, tests/read_concurrency_current.rs


## agent11 MAINTAINER DIRECTIVE work — step 1 (tool surface)

- Deleted `crates/octet-agent/src/tools/{ls,find,grep}.rs` (git rm).
- `src/tools/mod.rs`: removed `mod ls/find/grep`, their `pub use`, and the three
  `host.tool(...)` registrations. `register` is now exactly v0.7.6 parity:
  ReadTool, EditTool, WriteTool, BashTool, SearchTool (+ `#[cfg(windows)]` opt-in
  PowerShellTool). Module doc states the narrow surface + rg default.
- `src/tools/bash.rs:173` snippet: "Execute bash commands (ls, grep, find, etc.)"
  -> "Execute bash commands (prefer rg/ripgrep for file and content search)".
- `src/tools/shell_environment.rs:85`: the non-PowerShell snippet now delegates to
  `BashTool.prompt_snippet()` so the two cannot drift.
- `tests/parity_tools.rs`: deleted the 4.1/4.2/4.3 test block (7 tests) plus the
  now-unused `binary_available`/`run_with_path` helpers; 4.13 test updated for the
  new surface and bash snippet; NEW regression guard
  `core_tools_register_exactly_the_narrow_maintainer_surface` asserts the
  registered `CoreTools` surface is exactly {bash, edit, read, search, write}
  (+ powershell on Windows) and that ls/find/grep are absent.

Build blocker (NOT mine, in flight): `cargo check -p octet-ai --lib --locked`
currently fails at `crates/octet-ai/src/protocol/openai_responses.rs:863` with
`E0425: cannot find function canonical_computer_action` — another worker is
mid-edit in `crates/octet-ai`. `cargo check --workspace --all-targets --locked`
was green before that edit; I will re-run after it settles and report both ways.

### agent11 tool-surface removal — observed output ($(date -u +%Y-%m-%dT%H:%M:%SZ))

```
$ cargo check -p octet-agent --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 00s

$ cargo test -p octet-agent --test parity_tools --locked
running 17 tests
test bash_checkpoint_publisher_is_interval_bounded_and_dedupes_identical_snapshots ... ok
test deferred_suspension_requires_a_valid_handle_and_rejects_every_mismatch ... ok
test deferred_polls_need_one_permit_per_pass_and_fail_closed_on_stale_duplicate_or_foreign_handles ... ok
test batch_termination_requires_unanimous_finalized_results ... ok
test core_tools_register_exactly_the_narrow_maintainer_surface ... ok
test invocation_memos_survive_replay_until_the_outcome_is_known ... ok
test preview_coalescer_paces_both_interval_and_encoded_bytes ... ok
test powershell_is_opt_in_and_never_a_bash_fallback ... ok
test summarization_retries_are_distinct_from_compaction_failures_without_duplicate_durable_state ... ok
test tool_prompt_contributions_match_pi_snippets_and_guidelines ... ok
test bash_untruncated_output_leaks_no_spill_path ... ok
test edit_applies_multiple_edits_against_the_original_file ... ok
test bash_truncated_output_spills_the_full_stream_to_a_readable_path ... ok
test edit_legacy_shapes_are_normalized_into_one_batch ... ok
test bash_checkpoints_land_at_interval_boundaries_and_final_output_is_complete ... ok
test session_shell_clears_inherited_metadata_and_rereads_the_resolver ... ok
test session_shell_exposes_live_identity_metadata_and_host_command_prefix ... ok

test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s
```

Workspace check at the time of writing: `cargo check --workspace --all-targets --locked`
currently fails **outside my paths** in another worker's in-flight edit
(`crates/octet-coding-agent/src/tui/view/reasoning_render.rs:1345` and `:1383`,
E0061 `activity_shimmer_color` takes 8 args, 7 supplied). Before that edit the
same command was reported green by this worker's predecessor; `octet-agent` and
`octet-ai` both compile now. I will re-run the workspace check at the end and
report the result verbatim.

CHANGELOG-ready bullet (for root to land in CHANGELOG.md):
- **feat(octet-agent): the built-in tool surface is four tools plus ripgrep.**
  `ls`, `find`, and `grep` are withdrawn from `CoreTools` on the maintainer's
  decision: octet offers `read`, `write`, `edit`, `bash`, and the pre-existing
  ripgrep-backed `search` tool (registered for embedders and explicit allowlists
  exactly as in v0.7.6; the coding product leaves `search` out of its default
  allowlist). File discovery and content search go through `rg` — `search`, or
  `bash` — and bash's tool-prompt snippet now says so. A regression guard
  (`core_tools_register_exactly_the_narrow_maintainer_surface`) fails if any of
  the three tools is re-registered, and the parity ledger records rows 4.1–4.3 as
  **withdrawn by maintainer decision** rather than landed, including the two
  behaviours `search` genuinely does not cover (directory listing, filename-only
  discovery).

## agent11 TASK 1 — roadmap #175 `/fast`: `service_tier` plumbed into the live run path

agent10: service_tier plumbed into the live run path

### What landed (crates/octet-agent/src/agent.rs)
- `Agent::set_service_tier(Option<ServiceTier>) -> Result<(), AgentError>` and
  `Agent::service_tier() -> Option<ServiceTier>`; the field defaults to `None`
  (no tier), so default behavior is unchanged.
- `resolve_service_tier(&Model, Option<ServiceTier>)` gates the selection on the
  route's declared capability: a tier is accepted only when the protocol is
  `OpenAiResponses` **and** `endpoint.runtime.responses_profile.accepts_service_tier()`
  (Codex today). Every other route returns the codec's typed
  `AiError::Unsupported(UnsupportedError::ServiceTier)` — never a silent drop,
  and never a provider-name branch.
- Both live-run builders now take the tier: `durable_responses_options(session,
  model, system, tier) -> Result<Option<ResponsesOptions>, AgentError>` and
  `native_responses_options(..., tier)`. Called from the run loop's request
  construction (`agent.rs` ~6.9k) and from `responses_prewarm_request`, so the
  prewarmed websocket carries the same tier as the following live request.
- The gate is re-applied inside the builders, so a route change after
  `set_service_tier` still cannot leak the field.
- A requested tier is **never** dropped even when the session has no route-affine
  replay window: the builder then returns tier-only Responses options, which the
  codec replays canonically exactly as it would with `None` (asserted in the unit
  test below). With no tier the historical `None` is preserved untouched.

### Consumer contract for tui11 (crates/octet-coding-agent/src/modes/interactive.rs)
`apply_fast_command` can now drop its "inert" branch and call, on the agent it
already owns:
```rust
match agent.set_service_tier(Some(ServiceTier::Priority)) { Ok(()) => notice("`/fast on` ..."), Err(e) => error(...) }
agent.set_service_tier(None)   // `/fast off`
agent.service_tier()           // render current state
```
`Some(bool)`/`None` semantics stay as they are: `off` -> `None`, `on` ->
`Some(Priority)`. The existing `commands::codex_fast_tier_endpoint(model)` gate is
the same declared-capability check (`set_service_tier` re-checks it and returns
the typed error, so a UI that skips the pre-check still fails closed).
Not persisted across processes: a resumed session starts with no tier until the
frontend re-applies it (recorded as a remaining primitive, not claimed).

### Behavioral evidence (observed)
```
$ cargo test -p octet-agent --test agent_run --locked service_tier
running 1 test
test service_tier_reaches_the_request_only_on_a_route_that_declares_it ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s

$ cargo test -p octet-agent --lib --locked service_tier
running 1 test
test agent::tests::a_requested_service_tier_is_gated_by_the_route_and_never_silently_dropped ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 543 filtered out; finished in 0.02s
```
What the integration test asserts against the real HTTP/SSE path (wiremock
captures the actual request bodies):
1. Codex route + `set_service_tier(Some(Priority))` -> the request body carries
   `"service_tier":"priority"`, and the run completes.
2. `set_service_tier(None)` -> the next request has no `service_tier` key at all.
3. Plain OpenAI Responses route -> `set_service_tier(Some(Priority))` returns
   `ai error: Unsupported error: Responses service tier is unsupported on this
   route`, `service_tier()` stays `None`, and the run's request has no
   `service_tier` key.
4. Unit test additionally pins the `OpenAiChat` protocol gate (a Codex profile bit
   on a non-Responses protocol still refuses) and the no-replay-window path.

CHANGELOG-ready bullet:
- **feat(octet-agent): `/fast` now reaches the wire (roadmap #175).**
  `Agent::set_service_tier`/`service_tier` select the Responses `service_tier`
  for live runs, and both live-run `ResponsesOptions` builders (durable replay and
  native compact, plus the websocket prewarm) emit it. The field is sent only to
  a route whose declared runtime profile accepts it (Codex); every other route
  fails closed with the codec's typed unsupported error, so the switch can never
  report success while changing nothing. A requested tier survives a session with
  no route-affine replay window, and clearing it removes the field again.

## agent11 TASK 2 — parity 1e.2 durability half: verified end-to-end (already landed; test added)

The encoder/reducer and the durable journal are already at HEAD
(`crates/octet-ai/src/assistant_frame.rs`, `Session::begin_assistant_frame_journal`
/ `take_partial_assistant` at `session.rs:3010/3037`, the republish block at
`agent.rs:6400`, the per-attempt encode at `agent.rs:7130`). The only gap was
evidence: the sole test was a session-level unit test. New end-to-end test in
`crates/octet-agent/tests/agent_run.rs`:

`a_killed_stream_republishes_its_partial_assistant_prefix_once`
1. A scripted Responses turn streams `"KILLED-PREFIX "` then `"KILLED-TAIL"`; the
   test polls the real `Run` stream, stops at the first text delta and drops the
   run (a mid-stream kill). It then asserts the durable journal file
   `session.jsonl.partial-assistant-frames` exists and the session log does **not**
   contain the tail (an unsettled attempt is never committed).
2. A second `Agent` over `Session::open` republishes the frame prefix first: the
   concatenated text starts with `"KILLED-PREFIX "`, never contains
   `"KILLED-TAIL"`, and still contains its own `"SECOND-TURN"`; the journal is
   gone afterwards (consumed exactly once).
3. A third start after the second turn settled terminally observes exactly
   `"THIRD-TURN"` — a terminal turn is never republished as progress.

Observed:
```
$ cargo test -p octet-agent --test agent_run --locked a_killed_stream_republishes
running 1 test
test a_killed_stream_republishes_its_partial_assistant_prefix_once ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
```

CHANGELOG-ready bullet:
- **test(octet-agent): parity 1e.2 durability is proven end-to-end.** A run killed
  between assistant deltas leaves a durable frame journal; the next start
  republishes exactly the frame prefix once (never the whole killed turn, never a
  terminally settled turn) and removes the journal.

## agent11 TASK 3 (partial) — row 4.10 loop consumer landed

The tool-layer primitive `batch_requests_termination` had no consumer. It now has
one in the run loop (`crates/octet-agent/src/agent.rs`):
- per assistant batch, `termination_requests: Vec<bool>` records
  `ToolOutput::terminates_run()` for every finalized result, pushed exactly at the
  commit path (before the durable append, so a skipped/failed/aborted call — which
  has no `ToolOutput` — is recorded as `false` and can never sponsor termination);
- immediately after the abort check and *before* `needs_continuation` (so a
  unanimous batch never appends a continuation prompt), the loop calls
  `batch_requests_termination(termination_requests.iter().copied())` and breaks
  with `FinishReason::Completed`.

Behavioral test (`crates/octet-agent/tests/agent_run.rs`,
`unanimous_tool_termination_ends_the_run_and_a_lone_request_does_not`) uses a real
tool (`terminate_probe`) through the scripted HTTP/SSE path:
- unanimous single-call batch -> exactly **1** model request (the second scripted
  body `SHOULD-NOT-BE-REQUESTED` is never fetched), run reasons `Completed`, and
  the finalized result is durable in the session context before the run ends;
- two-call batch where only one sibling requests termination -> **2** requests,
  the run continues, and the follow-up request carries BOTH sibling results
  (`batch complete` and `batch continues`), proving a lone stop request cannot
  discard a sibling result.

Observed:
```
$ cargo test -p octet-agent --test agent_run --locked unanimous_tool_termination
running 1 test
test unanimous_tool_termination_ends_the_run_and_a_lone_request_does_not ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
```

CHANGELOG-ready bullet:
- **feat(octet-agent): a tool batch can end the run, unanimously.** The run loop
  now consumes `ToolOutput::requesting_termination()` through
  `batch_requests_termination`: the run finishes as soon as every finalized result
  of the assistant batch requested termination (never before its results are
  durable, never from a failed/skipped call, and never on a lone request that
  would discard a sibling result).

### Still-open §4 consumers — exact required changes (recorded, not done)

- **4.7 bash checkpoint publisher.** `CheckpointedBashTool` /
  `BashCheckpointPublisher` (`src/tools/bash.rs`) and `DurableInvocationStore`
  (`src/tools/durability.rs`) need a `PartialOutputCheckpointSink` in the live
  tool path. The blocker is the seam, not the logic: `ToolContext`
  (`src/tool.rs`) has no sink field, and adding one breaks every `ToolContext`
  literal — including `crates/octet-agent/src/delegation.rs` (out of bounds for
  this worker) and the coding product. Required change: add
  `PartialOutputCheckpointSink`/handle to `ToolContext` (or construct
  `CheckpointedBashTool` in `Agent`'s extension host) as one coordinated edit
  across `tool.rs`, `agent.rs`, `delegation.rs`, and the product's literals, then
  route the snapshot into `Session`'s durable value family (4.11's keyed
  replace/scan API, still missing).
- **4.8 live preview coalescer.** No built-in tool publishes a *replaceable*
  snapshot: the live panel is fed append-only `ToolProgress` chunks
  (`ToolProgressSink::live`), which must stay verbatim, so
  `AdaptivePreviewCoalescer` has no producer. Required change: a
  `ToolProgressSink::replaceable(...)` publication channel (tool-layer, mine) plus
  the panel consumer in `crates/octet-coding-agent`'s live tool panel (not mine).
- **4.13 prompt consumer.** `collect_tool_prompt_contributions` (`src/tool.rs`)
  must be called by the model-visible tool-section assembly, which lives in the
  coding product (`crates/octet-coding-agent/src/resources.rs`, prompt assembly),
  not in `crates/octet-agent`. Required change there: build the section from
  `collect_tool_prompt_contributions(host.tools())` in registration order, keeping
  `prompt_snippet == None` tools absent (so the list can never widen an
  allowlist).

## agent11 TASK 4 — #265 and #264 verified (real tests already exist; no new code needed)

**#265 namespaced pre-persistence turn-metadata enrichment: VERIFIED LANDED.**
The enrichment runs through the real agent path: `ExtensionHost::persistence_metadata_hook`
(`src/extension.rs:836`) registers a typed namespace, `collect_persistence_metadata`
(`src/agent.rs:~3250`) aggregates before the atomic append, and the metadata rides
the same `Session::append_with_metadata` boundary as the assistant turn
(`src/session.rs` entry metadata), so it is set *before* persistence and can
never be rewritten afterwards (append-only log). The end-to-end test is
`agent_run.rs`'s `#[path = "support/extension_hooks.rs"] mod extension_hooks`
test `before_persistence_metadata_is_namespaced_durable_and_never_model_context`
(`crates/octet-agent/tests/support/extension_hooks.rs:30`): it drives two real
`agent.complete(...)` turns with three hooks (private, public, invalid), asserts
the durable entry carries exactly the two valid namespaces with correct
provenance, that neither sentinel reaches any provider request, and that a
reopened `Session` shows the same metadata on both turns.

Observed:
```
$ cargo test -p octet-agent --test agent_run --locked before_persistence_metadata
running 2 tests
test extension_hooks::before_persistence_metadata_timeout_is_non_veto_and_uses_one_aggregate_budget ... ok
test extension_hooks::before_persistence_metadata_is_namespaced_durable_and_never_model_context ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
```
Remaining nit (not a gap in this row): the roadmap row is not restated in
`docs/parity/README.md`, which root owns.

**#264 bounded tool-progress presentation enrichment: VERIFIED LANDED.**
`ExtensionProgressEvent::Decoration` -> `ToolProgressDecoration::new` ->
`ToolProgress::Decoration` (`src/extension_process.rs:14449`),
with bounds in `src/tool.rs` (`MAX_PROGRESS_DECORATION_LABEL_BYTES` = 256,
`MAX_PROGRESS_DECORATION_DETAIL_BYTES` = 4 KiB, control characters and empty
labels rejected). The test
`extension_process::tests::progress_decoration_dispatch_requires_feature_active_parent_and_safe_bounded_fields`
proves feature negotiation (undecorated protocol -> error), the byte-truncation
bound (`"é" * 128` -> 256-byte label), duplicate-sequence and foreign-request
suppression, and the exact refusals (129 chars, ESC control char, empty label).

Observed:
```
$ cargo test -p octet-agent --lib --locked progress_decoration_dispatch
running 1 test
test extension_process::tests::progress_decoration_dispatch_requires_feature_active_parent_and_safe_bounded_fields ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 543 filtered out; finished in 0.00s
```

## agent11 FINAL STATUS (honest)

Landed with code + run test in this session:
1. **Maintainer directive — `ls`/`find`/`grep` withdrawn.** 3 modules deleted, 3
   registrations removed, bash snippet now names rg, `search` kept (v0.7.6
   parity), 7 obsolete tests removed, 1 new registration-surface guard added,
   `docs/parity/tools.md` rows 4.1–4.3 marked withdrawn with an honest list of
   what `search` does *not* cover.
2. **#175 `service_tier` plumbed** (see the exact `agent10: ...` line above).
3. **Parity 1e.2 durability** — verified end-to-end, new kill/restart test.
4. **Row 4.10 loop consumer** — code + test.
5. **Row 4.13 / 4.8 / 4.7 consumers** — NOT landed; exact required changes
   recorded above (all three need the coding-product prompt/panel seam or a
   `ToolContext` widening that would break `delegation.rs`, which is out of
   bounds for this worker).

Test state observed at the end of this session:
```
$ cargo test -p octet-agent --lib --locked                      -> 543 passed; 0 failed; 1 ignored
$ cargo test -p octet-agent --test agent_run --locked -- --skip websocket --skip qualified_codex_ws --skip native_compaction_calls_bound_reopening
                                                                -> 139 passed; 0 failed; 3 filtered out
$ cargo test -p octet-agent --test parity_tools --locked        -> 17 passed; 0 failed
$ cargo check -p octet-agent --all-targets --locked             -> Finished (green)
$ cargo check -p octet-ai --lib --locked                        -> Finished (green)
```
`cargo check --workspace --all-targets --locked` is **red outside my paths**:
`crates/sexy-tui-rs/src/rich_text/markdown.rs` is mid-edit by another worker
(`E0061 Builder::build` arity, `E0271 OffsetIter` item type, `E0063/E0027
Frame::Code` missing `info`) and `crates/sexy-tui-rs` is modified in the shared
tree by that worker. I did not touch it. Earlier in this session the same command
was also red in `crates/octet-coding-agent/src/tui/view/reasoning_render.rs`
(E0061 `activity_shimmer_color`) — also another worker's in-flight edit, since
resolved. Nothing in my paths is implicated.

Telemetry gates untouched: no change to the `--telemetry` JSONL path, to
`has_uncertain_usage`, or to any NOOP/InMemory accounting behavior (my edits touch
tool registration, the Responses request builder, the tool-batch termination
check, tests, and docs only). Bounded-by-default and no-provider-name-branching
hold: the tier gate is the endpoint's declared `accepts_service_tier()`
capability, not a provider identity.

### agent11 addendum (final)

- `src/tools/shell_environment.rs:84` now delegates the non-PowerShell snippet to
  `BashTool.prompt_snippet()` instead of carrying its own copy of the old
  "ls, grep, find" text, so the session shell and bash cannot drift. Re-ran after
  the change: `cargo test -p octet-agent --test parity_tools --locked` -> 17
  passed; `cargo test -p octet-agent --lib --locked` -> 543 passed, 1 ignored.
- `cargo check --workspace --all-targets --locked` is still red **only** in
  `crates/octet-coding-agent` (lib test) from another worker's in-flight edits
  (`app/bootstrap.rs`, `auth/codex/*`, `session_store.rs`, `tui/*`; 8 errors,
  E0063 among them). `crates/sexy-tui-rs` was red earlier in the same session and
  is no longer in the error list. No error in my paths.
START 2026-09-15T18:21:34Z agent12 alive
START 2026-09-15T18:29:29Z agent12b alive
START 2026-09-15T19:02:43Z agent12c alive

