# CLI parity execution evidence

Upstream read-only checkout verified at `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391` (`git rev-parse HEAD`). No upstream/vendor edits. Shared dirty CLI config-diagnostics extraction is preserved.

## Behavioral source

Read octet README, CLI, sessions, media, themes, commands; upstream coding-agent README, JSON, sessions, themes and source `cli/{args,file-processor,initial-message,list-models}.ts`, `main.ts`, `modes/print-mode.ts`.

Upstream JSON is session-header-first, delta-only JSONL; print consumes first combined stdin/files/prompt then sequential remaining prompts. RPC exclusively owns stdin JSONL. `--session-id` exact project identity creates if missing; `--name` trims and rejects empty. Listing is credential-filtered, fuzzy and sorted. Media must retain octet's explicit admission and durable usage policy.

## Status (work in progress)

- JSON event mode, model listing, session identity/name, explicit file input, stdin, HTML export: implementation in progress.
- Ephemeral sessions: blocked pending durable accounting-only Session backend. Existing `Session` couples conversation and usage/uncertainty in one locked JSONL; temporary/deleted logs would defeat accounting. Must fail closed rather than imply ephemeral support.
- Model cycling scope: inspect catalog and TUI owner integration; no TUI writes by this worker.
- Incremental session search: inspect existing catalog/service integration.
- Catalog publish validation/immutable cache: inspect canonical publishing primitive; no authority to remote publish.
- Model-backed evaluation harness: inspect model boundary/artifact support; no unsolicited paid/live inference.
- Serve `--name`: startup owned elsewhere; report exact integration.

No checks claimed yet. Per-item final status and proposed changelog bullets will be appended after verification.

## Adoption round (cli3, continuing cli2)

Adopted cli2's partial work rather than restarting: `src/cli/config_diagnostics.rs` (#313 extraction) and
`src/cli/parity.rs` (options, input expansion, model listing/scoping, session identity) plus wiring in
`src/lib.rs`, `src/modes/print.rs`, `src/modes/rpc.rs` (`JsonEventStream`), `src/modes/export_html.rs`,
`src/session_commands.rs` (`sessions export --format html`).

P0 compile break: `src/cli/parity.rs:150` no longer exists as reported; the `read_regular_file_bounded`
call now lives at `parity.rs:291` and already performs the checked narrowing
(`MAX_FILE_BYTES as usize`, `MAX_FILE_BYTES = 5 * 1024 * 1024`, inside `usize` on every supported target).
No u64/usize mismatch remains in `src/cli/`.

Blocked compile observation (NOT my path, recorded not fixed):
`cargo check -p octet-coding-agent 2>&1 | tail -40` -> `cargo check` fails in the dependency
`crates/sexy-tui-rs`: `src/text_editor/prompt_zones.rs:151/161` `no method named next_back found for
struct TakeWhile` (E0599) and `src/text_editor.rs:563` non-exhaustive `TextEditAction` match (E0004).
`crates/octet-agent` (10 errors on tools/find+ls at 15:0x) was also broken and was repaired by another
worker while I worked. octet-coding-agent cannot be type-checked until sexy-tui-rs compiles; I retry
`cargo check -p octet-coding-agent` after every step and report the first successful run with real output.

### Roadmap #180 - `octet serve --name` (landed, pending compile)

- `crates/octet-coding-agent/src/cli.rs:150` Serve variant gains `--name <NAME>` (raw string; the
  Serve-owned `normalize_startup_session_name` trims, bounds to 120 chars, rejects control characters and
  treats blank input as "no name").
- `crates/octet-coding-agent/src/lib.rs:161` dispatch now calls
  `extensions::serve::run_with_session_name(config, port, no_open, web_root, name)`.
- `crates/octet-coding-agent/src/lib.rs:104` (build without the `serve` feature) fails closed when a name
  is requested instead of silently dropping it, because that path launches the installed extension
  package whose launch protocol has no name option.
- Tests added in `cli.rs`: `serve_command_accepts_a_startup_session_name`,
  `serve_command_rejects_a_name_without_a_value`; `serve_command_parses_forwarded_loopback_options`
  updated for the new field.

### Round 2 (cli3)

`cargo check -p octet-coding-agent --message-format short` now reaches our crate and reports exactly ONE error,
outside my paths: `crates/octet-coding-agent/src/modes/interactive.rs:4039:11 error[E0004]: non-exhaustive
patterns: commands::Command::Fast(_) not covered` (a `Command::Fast` variant added in `commands.rs` by the
TUI/commands worker is not handled in their `interactive.rs` match). Recorded, not fixed. Until that lands,
no test in `octet-coding-agent` can be built or run, so every row below is code-complete but unverified.

Fixes I made this round:
- `src/cli/parity.rs:6`: added the missing `ModelId` import (E0425 at `parity.rs:182`).
- `src/lib.rs:172`: `parity.resolve_models(&mut config)?` is now actually called before
  `parity.select_session`; previously `--models` validated patterns but never scoped/selected anything.
- `tests/parity_cli.rs`: NEW process-boundary suite (5.1-5.8 + #180) using an isolated HOME/workspace/
  session root and a loopback OpenAI-compatible fixture on 127.0.0.1:0 only.

### Round 3 (cli3) - real behavioral evidence from a scratch clone

The shared worktree still cannot build `octet-coding-agent` (other worker's
`src/modes/interactive.rs` match arms). To get real behavioral evidence without
touching their files I cloned the workspace to `/tmp/cli3-verify` (rsync, excluding
`target/`, `.git/`, `artifacts/`), added THREE compile-only arms to the CLONE's
`modes/interactive.rs` (focus gained/lost, `Command::Fast`) that are NOT in the
shared worktree, copied only my own edited files back into the clone, and ran:

```
cd /tmp/cli3-verify
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p octet-coding-agent --test parity_cli -- --test-threads=1
```

Observed (exact):

```
running 10 tests
test file_media_is_admitted_only_for_recognized_images_and_vision_models ... ok
test json_mode_streams_a_session_header_first_delta_only_event_sequence ... ok
test list_models_lists_the_credential_scoped_catalog_and_filters_by_search ... ok
test models_patterns_scope_the_catalog_and_warn_on_a_miss ... ok
test no_session_fails_closed_because_accounting_shares_the_session_ledger ... ok
test piped_stdin_and_files_join_the_first_prompt_before_the_remaining_prompts ... ok
test rpc_mode_rejects_positional_prompts_instead_of_sharing_stdin ... ok
test serve_name_fails_closed_without_the_embedded_serve_runtime ... ok
test session_id_creates_the_exact_session_and_name_trims_or_rejects_empty ... ok
test sessions_export_html_is_a_single_script_free_self_contained_file ... ok
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.87s
```

and

```
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p octet-coding-agent --test configuration_diagnostics_full -- --test-threads=1
running 6 tests ... test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.24s
```

and

```
cargo test -p octet-coding-agent --lib -- cli::
test result: ok. 90 passed; 0 failed; 0 ignored; 0 measured; 1222 filtered out; finished in 0.09s
cargo test -p octet-coding-agent --lib -- serve_command parity::
test cli::parity::tests::fuzzy_search_is_case_insensitive_and_token_conjunctive ... ok
test cli::parity::tests::missing_and_oversized_inputs_fail_before_submission ... ok
test cli::tests::serve_command_rejects_a_name_without_a_value ... ok
test cli::tests::serve_command_parses_forwarded_loopback_options ... ok
test cli::tests::serve_command_accepts_a_startup_session_name ... ok
test cli::parity::tests::expansion_combines_stdin_files_and_first_prompt_then_preserves_sequence ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 1306 filtered out; finished in 0.00s
```

Bugs found and fixed by that evidence (all in my paths):
1. `parity.rs select_session` created the transcript with `Session::create(store.dir()/…)` before the
   workspace-scoped store directory existed: `Error: session io error: No such file or directory (os
   error 2)`. Fixed by materializing the store with `SessionStore::write_workspace_marker()` (the same
   call bootstrap uses) when `store.dir()` is missing.
2. `parity.rs select_scoped_models` built `ModelId("{endpoint}/{id}")`, which is NOT a catalog key (custom
   provider model ids are already prefixed, e.g. id `custom/alpha-model` on endpoint `custom-openai`), so
   any `--models` scope selected an unresolvable model. Fixed: match the id and `provider/id` forms (the
   upstream `resolveModelScopeFromModels` rule) and always select the real catalog id.
3. `parity.rs resolve_models` indexed `scope[0]` and `select_scoped_models` hard-failed on an unmatched
   pattern. Upstream warns per unmatched pattern and keeps the rest of the scope; it now warns
   (`no credential-configured models match --models pattern …`) and an empty scope selects nothing.

Row states after this round: 5.1 verified, 5.2 verified, 5.3 verified, 5.5 verified (including the model
image-admission refusal path), 5.6 verified, 5.7 verified for scope/default selection with the TUI cycling
handoff still blocked, 5.8 verified, #180 verified at the parse + fail-closed dispatch level, #313 verified
by its six process-boundary tests. 5.4, 5.9, 5.10, 5.11 are recorded as blocked with the exact missing
primitive in `docs/parity/cli.md`.

START 2026-09-15T15:43:46Z cli5 alive

### Round 4 (cli5) — row 1 (Codex context override) + row 5.10 (catalog publish gates)

Framing received from the maintainer: the 272K Codex cap is DELIBERATE (websocket drops on
long-running sessions, OpenAI's 272K recommendation, double pricing above 272K). Nothing here
calls it a bug or regression.

Files added (both mine):
- `crates/octet-coding-agent/src/cli/codex_context.rs` — pure opt-in override policy for
  `--codex-context-window <TOKENS>`: only ever RAISES the deliberate 272K cap, requires a
  Pro/ProLite entitlement above the cap, requires the explicit
  `--codex-context-window-acknowledge-cost-cliff`, fails closed above the model's entitlement
  ceiling, and marks `has_uncertain_usage = true` above the cap (double-priced; no exact cost).
- `crates/octet-coding-agent/src/cli/catalog_publish.rs` — `octet catalog publish` with five
  fail-closed gates (checksum, schema, min-client-version, required-provider, entry-count,
  immutable-path) and an atomic create-if-absent install that never replaces an existing catalog.

Wiring: `src/cli.rs` registers both modules and adds `TopLevelCommand::Catalog`; `src/cli/parity.rs`
parses/validates the two Codex flags in `ParityOptions::validate()`; `src/lib.rs` dispatches
`Catalog` to `cli::catalog_publish::run`.

Command: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo check -p octet-coding-agent --all-targets --message-format short`
Observed (exact):
```
crates/octet-agent/src/tools/durability.rs:505:5: error[E0277]: `DurableInvocationStore` doesn't implement `Debug`: unsatisfied trait bound
error: could not compile `octet-agent` (lib) due to 1 previous error
```
`crates/octet-agent/src/tools/durability.rs` is a NEW untracked file owned by another worker
(parity-tools). It does not type-check, so `octet-coding-agent` cannot be checked in the shared
worktree. Recorded, NOT fixed (not my path). I verify my code in a scratch clone instead.

### Round 5 (cli5) — my code type-checks and its tests pass (scratch clone)

Clone: `rsync -a --exclude target --exclude .git ./ /tmp/cli5-verify/`, then
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo check -p octet-coding-agent --all-targets`
-> `Finished dev profile`, zero errors (the clone compiles `octet-agent` fine; the shared-worktree
error seen in Round 4 was a mid-edit state of the other worker's untracked `durability.rs`).

Command: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p octet-coding-agent --test parity_cli -- --test-threads=1`
Observed (exact):
```
running 13 tests
test catalog_publish_gates_are_fail_closed_and_the_path_is_immutable ... ok
test codex_context_window_override_fails_closed_without_acknowledgement ... ok
test codex_context_window_override_is_accepted_with_the_acknowledgement ... ok
test ... (10 pre-existing parity tests) ... ok
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
`cargo test -p octet-coding-agent --lib -- cli::catalog_publish` -> `7 passed; 0 failed`.
`cargo test -p octet-coding-agent --lib -- cli::tests::codex_context_window_flag_parses` -> `1 passed`.

Integration with ctx5's exported policy: the duplicated CLI-local policy module was DELETED.
The flags now parse into `crate::codex_context::CodexContextOverride` and are validated with
ctx5's constants (`CODEX_CONTEXT_WINDOW_CAP`, `CODEX_PRO_CONTEXT_WINDOW`,
`CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING`). `ParityOptions::install_codex_context_env()` publishes the
value through ctx5's stable env bridge (`CODEX_CONTEXT_OVERRIDE_ENV`,
`CODEX_CONTEXT_ACKNOWLEDGE_ENV`) from `lib.rs` before bootstrap, so `resolve_codex_context_window`
sees it without a persisted config change.

NOTES / BLOCKERS seen while verifying (NOT my paths, NOT fixed):
- `crates/octet-coding-agent/src/codex_context.rs` (ctx5, untracked): test
  `codex_context::tests::deliberate_cap_is_kept_and_reported_as_a_clamp` FAILS at line 534
  (`assert!(resolved.has_uncertain_usage)`) because a session clamped to exactly 272K has
  `context_window == CODEX_CONTEXT_WINDOW_CAP`, so `context_window > CAP` is false. I did not touch
  ctx5's file.
- Also failing in the clone lib test run (other workers, not mine):
  `app::bootstrap::tests::{disabled_tools_are_absent_from_both_schema_and_execution_registry,
  tool_schema_reserve_is_positive_and_deterministic,
  unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes}`,
  `modes::interactive::clipboard_read::tests::a_real_helper_is_read_bounded_and_its_exit_status_is_honoured`,
  `modes::interactive::tests::active_session_commands_report_through_the_read_only_session`.

### Round 6 (cli5) — row 5.9 incremental session/entry search + change notification

Files (mine):
- `crates/octet-coding-agent/src/session_catalog.rs` — catalog schema 4 adds
  `indexed_entries` (bounded user/assistant text, `(session_id, entry_id)`
  primary key), `indexed_entry_sessions` (per-session file fingerprint) and
  `catalog_meta.entry_revision`. New: `entry_fingerprints`, `apply_entries`
  (returns whether anything changed; bumps the revision only on a real change),
  `entry_revision`, `search_entries` (escaped bounded `LIKE`), `escape_like`.
- `crates/octet-coding-agent/src/session_store.rs` — `index_session_entries`
  (lenient bounded extractor that keeps only `{"Text": ...}` parts, so reasoning,
  tool calls/arguments, media and provider metadata are never indexed),
  `indexed_entry_from_record`, `EntrySearchHit`/`EntryKind`/`EntrySearchOutcome`,
  `SessionSearchWatcher`, and `search_entries`/`search_entries_with`. Only
  sessions whose fingerprint changed are re-read; unchanged sessions are served
  from the disposable catalog.
- `crates/octet-coding-agent/src/session_commands.rs` — `octet sessions search
  <QUERY> [--limit N]`, plus `EntryKind` import.

Command: `cargo test -p octet-coding-agent --lib -- session_store:: session_catalog::`
Observed: `test result: ok. 36 passed; 0 failed`. Targeted:
`entry_search_is_incremental_and_notifies_only_on_change ... ok`,
`indexed_entries_keep_only_user_and_assistant_text ... ok`.

Command: `cargo test -p octet-coding-agent --test parity_cli -- --test-threads=1`
Observed (exact):
```
running 14 tests
test sessions_search_is_incremental_and_reports_the_index_change ... ok
... (13 others) ...
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
The process-boundary test proves: the cold search indexes 2 sessions, a repeat
search prints **no** re-index notice, after one transcript changes only 1 session
is re-read, and a miss is explicit.

START 2026-09-15T16:28:32Z cli6 alive

START 2026-09-15T16:50:13Z cli7 alive

### Round 7 (cli7) — P0 startup Codex-note spam fix (in progress)

Root-reported symptom: EIGHT near-identical `note: ... is budgeted at ... context
tokens (clamped ...)` lines at startup on a DeepSeek (non-Codex) session.

Root cause: the note was emitted from the *catalog registration loop*
(`app/bootstrap.rs::codex_context_report`, called at the per-model
`codex_context_resolve_for_registration` site), so every registered Codex model
printed a note regardless of the model the session actually runs. The
above-standard-tier variant also leaked an internal Rust API name and operation
id (`Session::record_usage_uncertainty("codex-context-above-272k")`) into
user-facing prose, and its wording made the reader decode three unlabelled
numbers.

Fix (in flight):
- `codex_context.rs`: added `codex_context_session_note(model_id, &window)` — the
  ONE user-facing note for an *effective* Codex session model. `None` unless the
  window is reduced by the deliberate cap or above the 272K standard tier.
  `context_window_label()` renders labelled `272K`/`372K`/`600K` values. The note
  leads with the in-app remedy (`--codex-context-window`, model effort menu) and
  keeps the env vars as the scriptable alternative. No internal API name, no
  operation id, no "bug/regression" language.
- `CodexContextClamp::message()` reworded to the same labelled form.
- `app/bootstrap.rs`: registration now *records* one note per model
  (`codex_context_record_note` -> `CodexContextNotes`) and prints nothing;
  `Bootstrap::codex_context_note(&ModelId)` is the single effective-model
  boundary a frontend calls once per session (stderr for print/json/rpc,
  `shell.notice` for the interactive shell).
