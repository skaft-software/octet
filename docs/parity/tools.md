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

## Status matrix

| Row | Behavior | State | Evidence |
| --- | --- | --- | --- |
| 4.1 | `ls` directories/dotfiles/limit | Landed | `src/tools/ls.rs`; `ls_lists_directories_and_dotfiles_without_recursing`, `ls_enforces_limit_and_rejects_zero`, `ls_reports_an_empty_directory_explicitly` |
| 4.2 | `find` glob/gitignore/limit | Landed | `src/tools/find.rs`; `find_glob_includes_hidden_paths_and_respects_gitignore_with_limit`, `find_reports_a_missing_fd_as_an_error_without_downloading` |
| 4.3 | Default `grep` ignoreCase/context/limit/hidden | Landed | `src/tools/grep.rs` + `src/tools/search.rs`; `grep_defaults_include_hidden_files_and_honor_ignore_case_and_context`, `grep_limit_defaults_to_one_hundred_and_rejects_bad_arguments` |
| 4.4 | Bash spilled output path | Landed | `src/tools/bash.rs` (`Capture::{spill,spill_path}`); `bash_truncated_output_spills_the_full_stream_to_a_readable_path`, `bash_untruncated_output_leaks_no_spill_path` |
| 4.5 | Bash session identity/provider/model/reasoning env + `commandPrefix` | Landed | `src/tools/shell_environment.rs`; `session_shell_exposes_live_identity_metadata_and_host_command_prefix`, `session_shell_clears_inherited_metadata_and_rereads_the_resolver` |
| 4.6 | Opt-in PowerShell (+ Windows CI evidence) | Opt-in gating landed; Windows execution evidence blocked | `src/tools/powershell.rs`, `src/tools/mod.rs`; `powershell_is_opt_in_and_never_a_bash_fallback` proves the gate, the non-Windows refusal, and the never-a-fallback contract. The Windows execution path is `#[cfg(windows)]` and is **not compiled here**: real Windows CI evidence is blocked on a Windows runner (human/hardware-gated, no primitive available in this environment) |
| 4.7 | Interval durable partial bash output checkpoints | Blocked | Missing durable invocation-scoped value store; see below |
| 4.8 | Adaptive preview coalescing (interval/rate/single trailing timer) | Landed (tool layer) | `AdaptivePreviewCoalescer` in `src/tool.rs`; `preview_coalescer_paces_both_interval_and_encoded_bytes`. Live-panel consumer pending |
| 4.9 | Original-file nonoverlapping multi-edit + legacy normalization | Landed | `src/tools/edit.rs`; `edit_applies_multiple_edits_against_the_original_file`, `edit_legacy_shapes_are_normalized_into_one_batch` |
| 4.10 | Unanimous finalized-result batch termination | Landed (tool layer) | `ToolOutput::requesting_termination`/`terminates_run` + `batch_requests_termination` in `src/tool.rs`; `batch_termination_requires_unanimous_finalized_results`. Loop consumer pending |
| 4.11 | Durable invocation memos through replay until outcome known | Blocked | Missing operation/invocation-scoped memo store; see below |
| 4.12 | Deferred provider suspend/resume/handles/poll permits | Blocked | Missing harness suspended-run lifecycle; see below |
| 4.13 | Tool `promptSnippet`/`promptGuidelines` | Landed (tool layer) | `Tool::prompt_snippet`/`prompt_guidelines` + `collect_tool_prompt_contributions`; `tool_prompt_contributions_match_pi_snippets_and_guidelines`. Prompt consumer pending |
| 4.14 | Summarization retry distinct from compaction failure | Blocked | Missing distinct summarization retry policy; see below |

## Contract notes for the landed rows

- **4.1** `ls` lists one directory level, includes dotfiles, sorts
  case-insensitively, suffixes directories with `/`, and appends an explicit
  `[entries or byte limit reached; limit=N; truncated=true]` marker. Default
  limit 500. Effect classification stays `WorkspaceRead` on Unix; on Windows it
  degrades to `HostRead` because no descriptor-relative enumeration primitive is
  exposed, and no platform-specific escape hatch is claimed.
- **4.2** `find` shells out to an already-installed `fd` (never downloads one),
  passes the model's glob and path after `--`, includes hidden paths, keeps
  `.gitignore` semantics, defaults to 1000 results, and reports a clear error
  when `fd` is absent.
- **4.3** default `grep` is the Pi-shaped surface over the bounded native
  search: `pattern`/`path`/`glob`/`ignoreCase`/`literal`/`context`/`limit`/
  `hidden`, hidden search on by default, limit 100, context lines rendered with
  `-` separators and not counted against the limit, and the search-only
  vocabulary (`query`, `mode`, `max_results`) rejected.
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
- **4.13** `prompt_snippet`/`prompt_guidelines` default to nothing, so no
  existing tool changes behavior, and `collect_tool_prompt_contributions` skips
  tools without a snippet — the list is presentation intent, never a tool
  inventory, allowlist, or authority input. The PI_* guideline is advertised
  only by the variant that actually injects the metadata.

## Recorded gaps in the landed rows

These are consumers that live outside this worker's exclusive paths; each is a
one-line wiring change, not a missing primitive.

1. **4.10 loop wiring.** `crates/octet-agent/src/agent.rs` must read
   `ToolOutput::terminates_run()` for each finalized result and call
   `batch_requests_termination` once the batch is fully placed.
2. **4.13 prompt assembly.** The model-visible tool section is assembled by the
   coding product (`crates/octet-coding-agent`), which does not yet call
   `collect_tool_prompt_contributions`.
3. **4.8 live consumer.** No built-in tool publishes a replaceable preview
   snapshot through the coalescer yet; the live tool panel is fed by
   append-only progress chunks, which must stay verbatim.
4. **lib.rs re-exports.** `ToolPromptContribution`, `PreviewPublication`,
   `AdaptivePreviewCoalescer`, `collect_tool_prompt_contributions`,
   `batch_requests_termination`, and `ToolOutput::requesting_termination` are
   reachable through the public `octet_agent::tool` module but are not
   re-exported at the crate root, because `crates/octet-agent/src/lib.rs` is not
   an owned path for this work.
5. **4.6 Windows evidence.** `src/tools/powershell.rs`'s `resolve_shell` and
   `configure_command` are `#[cfg(windows)]`, and `bash.rs`'s
   `execute_windows` is too, so the PowerShell command construction is not even
   compiled on this host (darwin/arm64). The executable opt-in gate, the
   non-Windows refusal, the shared `HostProcess` classification, and the "never
   a bash fallback" contract are proven by test; **real Windows CI evidence is
   unattainable in this environment and requires a Windows runner or Windows
   hardware.**

## Blocked rows and their exact missing primitives

- **4.7 — interval durable partial bash output checkpoints.**
  Missing primitive: a durable, invocation-scoped *replaceable* value store in
  `crates/octet-agent/src/session.rs` (Pi's `setValue(pendingToolOutput(operationId,
  invocationId), snapshot)` with a mutation that verifies the call is still
  `effect_pending`, plus prefix `scanValues` cleanup) and a `checkpoint: true`
  option on the tool progress callback (`ToolProgress`) that requests it. Both
  are outside `crates/octet-agent/src/tools/**` and `tool.rs`. The tool side is
  ready: bash already produces the bounded snapshot (head/tail plus
  `full_output_path`), and Pi's rule (`BASH_CHECKPOINT_INTERVAL_MS = 2000`,
  only when the bounded snapshot differs) can be added to `bash.rs` as soon as
  the channel exists. Crash recovery of a checkpoint additionally needs the
  agent loop (`agent.rs`) to rehydrate it as auxiliary observation data.
- **4.11 — durable invocation memos through replay until outcome known.**
  Missing primitive: an operation/invocation-scoped memo store
  (`pi.op.tool_memo` equivalent: non-empty name validation, replace/delete,
  prefix scan for atomic invocation cleanup when the outcome becomes ready) plus
  the invocation capability that fences late writes after settlement. octet's
  session is an append-only JSONL log with no keyed replace/scan API
  (`crates/octet-agent/src/session.rs`) and tools receive no invocation-scoped
  capability (`crates/octet-agent/src/tool.rs` `ToolContext`), so this cannot be
  implemented inside the owned tool paths.
- **4.12 — deferred provider suspend/resume/handles/poll permits.**
  Missing primitive: the harness suspended-run lifecycle — a durable
  `deferred.suspended` run state carrying a `DeferredHandle`, `run_suspend`/
  `run_resume` events, and "one deferred poll permit per pass" with unknown-poll
  replacement under fresh ids (`packages/agent/docs/work-packages/05-direct-durable-drive.md`).
  Owner paths are `crates/octet-agent/src/{agent,events,session}.rs` and the
  provider surface, none of which are owned here; no tool-local change can
  satisfy the row.
- **4.14 — summarization retry distinct from compaction failure.**
  Missing primitive: a summarization-call retry policy shared by
  compaction and branch summarization (Pi's `_summarizationRetryCallbacks`
  emitting `summarization_retry_scheduled` / `_attempt_start` / `_finished`),
  applied to `Agent::summarize`/`auxiliary_compact`. octet's
  `ProviderRetryKind` has no summarization kind, and `compact_boundary` awaits
  `self.summarize(...)?`, so a transient summarization error fails the
  compaction boundary. Owner paths: `crates/octet-agent/src/agent.rs` and
  `crates/octet-agent/src/compaction.rs`, outside this worker's paths.

## CHANGELOG-ready bullets

- Added built-in `ls`, `find`, and Pi-shaped `grep` tools (dotfile/directory
  listing with limits, installed-`fd` glob discovery that respects `.gitignore`,
  and hidden/ignoreCase/context/limit-aware content search) and registered `ls`,
  `find`, and `grep` in `CoreTools`.
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
