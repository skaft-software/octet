# Tool parity detail (Pi rows 4.1–4.14)

Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391` (v0.85.1+72), verified locally.
This document owns the per-row status of the tool rows in
[`README.md`](README.md). Upstream anchors are
`packages/coding-agent/src/core/tools/{ls,find,grep,bash,edit}.ts` and the design
documents `packages/agent/docs/tool-durability.md` and
`packages/agent/docs/mobile-handoff/01-harness/04-tool-output/rate-limiting.md`.
No upstream TypeScript is vendored.

Behavioral evidence is `crates/octet-agent/tests/parity_tools.rs` (17 tests) plus
the in-crate unit tests of the tool modules. A row is only "Landed" when a test
that exercises the real tool ran and passed; observed output for every run is
recorded in [`../swarm-audit/EXECUTION-parity-tools.md`](../swarm-audit/EXECUTION-parity-tools.md).

> **Maintainer decision (withdrawal of rows 4.1–4.3).** octet ships a four-tool
> surface — `read`, `write`, `edit`, `bash` — with ripgrep as the search engine
> (`search`, registered for embedders and explicit allowlists exactly as in the
> v0.7.6 release; the coding product leaves it out of its default allowlist).
> Dedicated `ls`, `find`, and `grep` tools were removed from `CoreTools` on the
> maintainer's direction ("there shouldn't be grep LS find tools, it should only
> be read write edit and bash … we use rg by default just like in v0.7.6"). Rows
> 4.1–4.3 are therefore recorded as **withdrawn**, not landed: the history and
> the reasoning stay below, and the withdrawn modules are gone from the tree.

Rows 4.7, 4.11, 4.12, and 4.14 are landed as **tool-layer primitives** in
`crates/octet-agent/src/tools/{durability,deferred,summarization}.rs`. Pi stores
their state in the session's bound-value family; octet's session is an
append-only JSONL log whose keyed replace/scan API does not exist yet, and
`session.rs`/`agent.rs` are outside this change's scope, so each module
implements the row's decision logic behind one owned type and documents the exact
consumer that still has to be wired in "Recorded gaps" below.

## Status matrix

| Row | Behavior | State | Evidence |
| --- | --- | --- | --- |
| 4.1 | `ls` directories/dotfiles/limit | **Withdrawn by maintainer decision** — the behaviour is served by `bash` + `ls`/`rg --files`, not a dedicated tool | Was `src/tools/ls.rs` (module deleted); tests `ls_lists_directories_and_dotfiles_without_recursing`, `ls_enforces_limit_and_rejects_zero`, `ls_reports_an_empty_directory_explicitly` were deleted with the tool |
| 4.2 | `find` glob/gitignore/limit | **Withdrawn by maintainer decision** — the behaviour is served by `bash` + `rg --files -g <glob>`, not a dedicated tool | Was `src/tools/find.rs` (module deleted); tests `find_glob_includes_hidden_paths_and_respects_gitignore_with_limit`, `find_reports_a_missing_fd_as_an_error_without_downloading` were deleted with the tool |
| 4.3 | Default `grep` ignoreCase/context/limit/hidden | **Withdrawn by maintainer decision** — the behaviour is served by the pre-existing ripgrep-backed `search` tool (`query`/`mode`/`ignoreCase`/`context`/`limit`/`hidden`/`glob`), not a Pi-shaped `grep` alias | Was `src/tools/grep.rs` (module deleted); `SearchTool` (`src/tools/search.rs`) keeps every behaviour and its tests; the `grep_defaults_*`/`grep_limit_*` alias tests were deleted with the alias |
| 4.4 | Bash spilled output path | Landed | `src/tools/bash.rs` (`Capture::{spill,spill_path}`); `bash_truncated_output_spills_the_full_stream_to_a_readable_path`, `bash_untruncated_output_leaks_no_spill_path` |
| 4.5 | Bash session identity/provider/model/reasoning env + `commandPrefix` | Landed | `src/tools/shell_environment.rs`; `session_shell_exposes_live_identity_metadata_and_host_command_prefix`, `session_shell_clears_inherited_metadata_and_rereads_the_resolver` |
| 4.6 | Opt-in PowerShell (+ Windows CI evidence) | Opt-in gating landed; Windows execution evidence blocked | `src/tools/powershell.rs`, `src/tools/mod.rs`; `powershell_is_opt_in_and_never_a_bash_fallback` proves the gate, the non-Windows refusal, and the never-a-fallback contract. The Windows execution path is `#[cfg(windows)]` and is **not compiled here**: real Windows CI evidence is blocked on a Windows runner (human/hardware-gated, no primitive available in this environment) |
| 4.7 | Interval durable partial bash output checkpoints | Landed (tool layer) | `src/tools/bash.rs` (`BashCheckpointPublisher`, `BashCheckpoints`, `CheckpointedBashTool`) + `src/tools/durability.rs` (`DurableInvocationStore`); `bash_checkpoint_publisher_is_interval_bounded_and_dedupes_identical_snapshots`, `bash_checkpoints_land_at_interval_boundaries_and_final_output_is_complete` |
| 4.8 | Adaptive preview coalescing (interval/rate/single trailing timer) | Landed (tool layer) | `AdaptivePreviewCoalescer` in `src/tool.rs`; `preview_coalescer_paces_both_interval_and_encoded_bytes`. Live-panel consumer pending |
| 4.9 | Original-file nonoverlapping multi-edit + legacy normalization | Landed | `src/tools/edit.rs`; `edit_applies_multiple_edits_against_the_original_file`, `edit_legacy_shapes_are_normalized_into_one_batch` |
| 4.10 | Unanimous finalized-result batch termination | Landed end-to-end | `ToolOutput::requesting_termination`/`terminates_run` + `batch_requests_termination` in `src/tool.rs`; consumer in the run loop (`src/agent.rs`: per-batch `termination_requests` recorded at the commit path, checked after the abort gate and before `needs_continuation`). Tests: `batch_termination_requires_unanimous_finalized_results` (tool layer) and `unanimous_tool_termination_ends_the_run_and_a_lone_request_does_not` (`tests/agent_run.rs`: unanimous batch -> 1 model request with durable results; one dissenting sibling -> 2 requests with both results carried forward) |
| 4.11 | Durable invocation memos through replay until outcome known | Landed (tool layer) | `src/tools/durability.rs`; `invocation_memos_survive_replay_until_the_outcome_is_known` |
| 4.12 | Deferred provider suspend/resume/handles/poll permits | Landed (tool layer) | `src/tools/deferred.rs`; `deferred_suspension_requires_a_valid_handle_and_rejects_every_mismatch`, `deferred_polls_need_one_permit_per_pass_and_fail_closed_on_stale_duplicate_or_foreign_handles` |
| 4.13 | Tool `promptSnippet`/`promptGuidelines` | Landed (tool layer) | `Tool::prompt_snippet`/`prompt_guidelines` + `collect_tool_prompt_contributions`; `tool_prompt_contributions_match_pi_snippets_and_guidelines`. Prompt consumer pending |
| 4.14 | Summarization retry distinct from compaction failure | Landed (tool layer) | `src/tools/summarization.rs`; `summarization_retries_are_distinct_from_compaction_failures_without_duplicate_durable_state` |

## Contract notes for the landed rows

- **4.4** truncated bash output keeps its bounded head/tail render and adds
  `full_output_path=<spill>` (the complete stream, written to a private temp
  file) or `partial_output_path=` plus `spill_error=true` when the spill itself
  failed. Untruncated output adds no path and deletes the spill.
- **4.5** `BashTool::with_session_environment`/`PowerShellTool::with_session_environment`
  return a `SessionShellTool` that resolves `PI_SESSION_ID`, `PI_SESSION_FILE`,
  `PI_PROVIDER`, `PI_MODEL`, `PI_REASONING_LEVEL` freshly before every call,
  removes any inherited value not supplied, and rejects NUL-bearing or oversized
  metadata before the child starts. `with_command_prefix` prepends trusted host
  configuration; it is never a model argument.
- **4.6** `powershell` is registered only under `#[cfg(windows)]` and is never a
  fallback for `bash`. On non-Windows it classifies as `HostProcess` (the same
  authority as bash) and fails explicitly instead of degrading.
- **4.8** `AdaptivePreviewCoalescer` implements
  `nextDelay = max(100 ms, encodedBytes * 1000 / 100 000)`: first update after
  idle publishes immediately, writes before the deadline collapse into the
  latest snapshot, at most one trailing timer exists, and a forced publication
  (completion/error/checkpoint) cancels it. Only replaceable snapshots may be
  coalesced; live append-only byte chunks stay verbatim.
- **4.9** every `edits[].oldText` is matched against the original file, regions
  must be non-overlapping and unique, ambiguity and overlap are rejected before
  any write, and legacy `old`/`new`, `oldText`/`newText`, JSON-string `edits`,
  and single-object `edits` all normalize into one batch guarded by
  `expected_hash`.
- **4.10** a batch ends the run only when every finalized result requests
  termination (`batch_requests_termination`); an empty batch or any dissent
  keeps the run going, so no sibling result is discarded. `ToolOutput` defaults
  to not requesting termination.
- **4.7** `BashCheckpointPublisher` implements Pi's bash policy: a checkpoint is
  requested at most once per interval (default `BASH_CHECKPOINT_INTERVAL = 2000
  ms`, floored at 10 ms) and only when the complete bounded snapshot differs from
  the last requested one, so output volume never accelerates checkpoint
  frequency. The snapshot is the bounded capture render, capped at
  `BASH_CHECKPOINT_MAX_BYTES = 50 KiB` (the two stream sections share the cap,
  over-long sections keep their newest bytes on a code-point boundary and are
  marked elided), and a checkpoint never emits `complete_<stream>=true`, so a
  recovery consumer cannot read it as proof the command finished. The durable
  value is `pi.pending.tool_output:`*operation*`:`*invocation — one replaceable
  value per invocation, deleted when the outcome becomes known, and a late
  checkpoint after settlement is refused instead of reviving state. A checkpoint
  write failure never changes the command's result; it is counted
  (`checkpoint_stats().failures`) for the host to report.
- **4.11** memos live at `pi.op.tool_memo:`*operation*`:`*invocation*`:`*name,
  where the name is non-empty and cannot contain `:`. A memo survives replay
  while the call is `effect_pending`: `replay_lookup` returns `Memoized(value)`
  or `NotYetRecorded`, and `replay_step` runs its effect only for
  `NotYetRecorded` and then commits the value (Pi's `step.do`). Settlement is one
  operation that fences every open handle and deletes memos and partial output
  together; afterwards `open` reports `OutcomeKnown` and a late memo read is an
  error — never `None` — because "no value" would mean "run the effect again".
  An orphaned `effect_pending` invocation is recovered as an interruption result
  whose outcome is explicitly `Unknown` with Pi's mandatory marker, and a second
  recovery is refused. Values, names, values per invocation, live invocations,
  and retained settled invocations are all hard-bounded; an over-limit write
  fails closed instead of truncating a memo.
- **4.12** a response suspends only with a valid handle: non-empty `id`, provider
  and model id equal to the durable run configuration, and `api` equal to the api
  of the response that carried it. Anything else is a terminal
  `DeferredSuspendFailure` (`MalformedHandle`) whose diagnostic starts with Pi's
  "Provider returned an invalid deferred handle" plus the specific rejection, so
  an untrustworthy handle can never park a run. Polling requires a permit:
  `DeferredPollPermit::none` leaves the run durably suspended with nothing
  written, while a granted permit admits exactly one poll, is consumed, and emits
  `run_resume`. Stale (minted for another durable generation), duplicate (the
  same permit twice), foreign (handle/configuration mismatch), and expired
  (provider-supplied `expires_at`) polls are refusals rather than silent waits. A
  poll from `deferred.suspended` increments the poll number and reserves fresh
  response/usage ids; a poll that replaces an unknown-outcome
  `deferred.effect_pending` keeps the same poll number, uses fresh ids, and
  reports the abandoned frame list to delete. A still-deferred poll returns to
  `deferred.suspended` at the same poll number with a bumped generation, and an
  invalid handle returned by a poll fails closed.
- **4.14** one `SummarizationRetryPolicy` (default three attempts, doubling
  backoff capped at 60 s) serves compaction and branch summarization.
  `SummarizationRetryScheduled` (diagnostic `summarization retry scheduled …`) is
  a distinct type and distinct wording from `CompactionFailure` (diagnostic
  `compaction boundary failed: …`), and `CompactionStepOutcome::is_compaction_failure()`
  is false for a scheduled retry, so a live boundary cannot be reported as
  failed. Deterministic failures and aborts are terminal on the first attempt.
  The driver calls the caller's commit closure only after a successful attempt,
  so durable summary records are exactly one on success and zero on failure — a
  retry can never duplicate durable state — and a commit that fails is reported
  as `CompactionFailureKind::DurableWrite` rather than as a summarization
  failure.
- **4.13** `prompt_snippet`/`prompt_guidelines` default to nothing, so no
  existing tool changes behavior, and `collect_tool_prompt_contributions` skips
  tools without a snippet — the list is presentation intent, never a tool
  inventory, allowlist, or authority input. The PI_* guideline is advertised
  only by the variant that actually injects the metadata.

## Withdrawn rows 4.1–4.3 — what the four-tool surface does and does not cover

The removed modules were real and tested; they were withdrawn because the
maintainer's decision is a narrower model-visible surface with ripgrep as the
search engine. Nothing in this section claims `search` covers what it does not.

- **Covered by `bash` + `rg`/coreutils (no dedicated tool, no gap in
  capability):** one-level directory listing including dotfiles (`ls -a`),
  filename discovery by glob (`rg --files -g '*.rs'`), and recursive content
  search (`rg`) are ordinary shell work. The removed tools were bounded,
  effect-classified wrappers over exactly these commands.
- **Covered by `search` (the pre-existing ripgrep-backed tool):** content search
  with `query`, `path`, `glob`, `mode=literal|regex`, `ignoreCase`, `context`,
  `limit`/`max_results`, and `hidden` (default true, ignore rules still apply),
  with structured `rg --json` parsing, `path:line  text` rendering, deterministic
  path ordering, explicit truncation metadata, and no shell interpolation. This is
  the whole of row 4.3's behaviour, minus the Pi-shaped argument *alias*.
- **Genuinely not covered by `search`:** there is no directory-listing mode and no
  filename-only discovery mode. `search` never lists a directory and never
  returns paths without matches; a caller that wants `ls`-style bounds (500-entry
  default, trailing `/` on directories, `[entries or byte limit reached; …]`
  marker) or `find`-style `fd`-backed globbing (1000-result default, `fd`'s
  `.gitignore` handling, the explicit "fd is absent" error) must use `bash`. That
  is a real reduction in built-in surface, recorded here rather than papered
  over. No `ls`/`find` tool returns under another name.
- **Regression guard.** `core_tools_register_exactly_the_narrow_maintainer_surface`
  (`crates/octet-agent/tests/parity_tools.rs`) asserts the registered `CoreTools`
  surface is exactly `bash`, `edit`, `read`, `search`, `write` (plus the
  Windows-only opt-in `powershell`) and that `ls`/`find`/`grep` are absent, so a
  later parity pass cannot re-add them silently.

## Recorded gaps in the landed rows

These are consumers that live outside this worker's exclusive paths; each is a
one-line wiring change, not a missing primitive.

1. **4.13 prompt assembly.** The model-visible tool section is assembled by the
   coding product (`crates/octet-coding-agent`), which does not yet call
   `collect_tool_prompt_contributions`.
2. **4.8 live consumer.** No built-in tool publishes a replaceable preview
   snapshot through the coalescer yet; the live tool panel is fed by
   append-only progress chunks, which must stay verbatim.
3. **lib.rs re-exports.** `ToolPromptContribution`, `PreviewPublication`,
   `AdaptivePreviewCoalescer`, `collect_tool_prompt_contributions`,
   `batch_requests_termination`, and `ToolOutput::requesting_termination` are
   reachable through the public `octet_agent::tool` module but are not
   re-exported at the crate root, because `crates/octet-agent/src/lib.rs` is not
   an owned path for this work.
4. **4.6 Windows evidence.** `src/tools/powershell.rs`'s `resolve_shell` and
   `configure_command` are `#[cfg(windows)]`, and `bash.rs`'s
   `execute_windows` is too, so the PowerShell command construction is not even
   compiled on this host (darwin/arm64). The executable opt-in gate, the
   non-Windows refusal, the shared `HostProcess` classification, and the "never
   a bash fallback" contract are proven by test; **real Windows CI evidence is
   unattainable in this environment and requires a Windows runner or Windows
   hardware.**

## Recorded gaps in the four durability rows

Each of these is a consumer or a cross-process storage binding, not missing tool
logic; the row behavior itself is implemented and covered by the tests named in
the matrix.

5. **4.7 cross-process durability + host wiring.**
   `DurableInvocationStore` is process-durable: it implements Pi's contract
   (replace one value, fence on `effect_pending`, delete on settlement, hard
   bounds) but keeps values in memory because `crates/octet-agent/src/session.rs`
   owns the durable log and has no keyed `setValue`/`scanValues` API yet. The
   exact session primitive needed is `setValue(pendingToolOutput(operationId,
   invocationId), snapshot)` as one scalar replacement whose mutation verifies
   the call is still `effect_pending`, plus prefix `scanValues` cleanup for
   operation-owned families. Wiring is also required for a host to construct
   `CheckpointedBashTool` (or pass a `PartialOutputCheckpointSink` through a
   future `ToolContext` field, which today would change every `ToolContext`
   literal in `agent.rs`); crash recovery must additionally rehydrate the stored
   snapshot as auxiliary observation data and call
   `DurableInvocationStore::recover_unsafe_orphan`.
6. **4.11 cross-process memos + capability injection.** Same storage gap as 4.7
   for `pi.op.tool_memo`, plus an invocation capability handed to tools
   (`AgentHarnessToolInvocation` in Pi). Until then, a tool that wants memos must
   be constructed with the handle it should use for its call.
7. **4.12 harness suspended-run lifecycle.** `crates/octet-agent/src/agent.rs`
   and `events.rs` must carry the durable leaves (`deferred.suspended`,
   `deferred.effect_pending`), emit `run_suspend`/`run_resume`, mint exactly one
   permit per `resume()`/poll-installed pass, and perform the poll through the
   provider's deferred-fetch stream. `crates/octet-ai` has no deferred stop
   reason or `DeferredHandle` yet, so `DeferredStopReason` and the provider call
   must be connected there; the decision core (`suspend_deferred_response`,
   `prepare_deferred_poll`, `DeferredSuspended::resume_after_poll`) is landed and
   tested against that lifecycle.
8. **4.14 compaction/branch-summary wiring.** `crates/octet-agent/src/compaction.rs`
   and `agent.rs` must call `run_summarization_with_retry` for both summary
   sources (with the session's durable write as the commit closure) and route
   `CompactionStepOutcome::Retrying` to the boundary without closing it;
   `ProviderRetryKind` (used by the ordinary assistant-turn retry hook) has no
   summarization kind, and `compact_boundary` awaits `self.summarize(...)?`, so a
   transient error still fails the boundary today.

## CHANGELOG-ready bullets

- **Removed the `ls`, `find`, and `grep` built-in tools (maintainer decision).**
  octet keeps a four-tool surface — `read`, `write`, `edit`, `bash` — with
  ripgrep as the search engine, matching the v0.7.6 release: the ripgrep-backed
  `search` tool stays registered for embedders and explicit allowlists, and
  directory listing plus filename discovery go through `bash` + `rg`
  (`rg --files -g <glob>`, `ls -a`). A dedicated entry (row 4.3) is not coming
  back as a `grep` alias, and the regression guard
  `core_tools_register_exactly_the_narrow_maintainer_surface` fails if any of the
  three is re-registered.
- Bash's tool-prompt snippet now names the search default the model should use:
  "Execute bash commands (prefer rg/ripgrep for file and content search)".
- Bash now writes truncated output to a private spill file and reports
  `full_output_path` so the complete stream stays reachable (`partial_output_path`
  plus `spill_error=true` when the spill cannot be retained).
- Added host-configurable shell session metadata: `PI_SESSION_ID`,
  `PI_SESSION_FILE`, `PI_PROVIDER`, `PI_MODEL`, and `PI_REASONING_LEVEL` are
  resolved freshly per call, inherited values are cleared, and
  `SessionShellTool::with_command_prefix` prepends trusted host setup commands.
- Added opt-in Windows PowerShell support: `powershell` is registered only on
  Windows, is never a bash fallback, and refuses to run elsewhere.
- `edit` accepts multiple non-overlapping replacements in one call, matched
  against the original file, with ambiguity/overlap rejection and legacy
  `old`/`new` normalization.
- Added the tool-layer primitives for unanimous batch termination
  (`ToolOutput::requesting_termination`, `batch_requests_termination`) and
  adaptive preview pacing (`AdaptivePreviewCoalescer`, 100 ms floor and 100 KB/s
  target with one trailing timer).
- Tools can now contribute a model-visible `prompt_snippet` and
  `prompt_guidelines` (Pi's `promptSnippet`/`promptGuidelines`), collected in
  registration order by `collect_tool_prompt_contributions`.
- Bash can now persist interval-bounded durable partial-output checkpoints
  (`CheckpointedBashTool`, `BashCheckpointPublisher`): at most one checkpoint per
  interval, only when the bounded snapshot changed, capped at 50 KiB, and never
  mistaken for a final result.
- Added the durable invocation store (`DurableInvocationStore`,
  `InvocationHandle`): one replaceable partial-output value plus named replay
  memos, fenced on the call still being `effect_pending`, hard-bounded, and
  deleted atomically when the outcome becomes known — a settled invocation fails
  closed instead of looking like an unrecorded memo.
- Added durable deferred provider suspend/resume: valid handles park a run,
  invalid handles fail terminally with Pi's diagnostic, and each pass needs its
  own poll permit, with stale, duplicate, foreign, and expired polls refusing
  instead of double-polling or parking forever.
- Added the shared summarization retry policy for compaction and branch
  summarization: a scheduled retry is a distinct typed outcome and diagnostic
  from a compaction failure, aborts and deterministic errors never retry, and a
  retry can never duplicate durable summary state.
