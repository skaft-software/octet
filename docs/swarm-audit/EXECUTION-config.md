# Phase1 #313 configuration diagnostics execution

## Candidate and scope

- Base: `df5a7e809715961b9344af6b52e43a6ca48f56b3` (`df5a7e80`), plus the eventual owned-path working-tree diff. Initial `git status --short` was empty. Other workers may change unrelated paths during qualification.
- Owner: `config-diagnostics`; owned production paths are `crates/octet-coding-agent/src/cli.rs` and its private `cli/` module; configuration-specific integration tests and the diagnostics design/qualification docs are also in scope.
- No commits, branch/index changes, cleanup, global formatting, or new Cargo target directory. Existing `target/` used (initial `du -sh target`: 56G).
- Read fully: `docs/design/maintainability.md`, `docs/design/config-diagnostics.md`, `docs/qualification/configuration-diagnostics-full.md`, `CONTRIBUTING.md`, and `docs/build-profiles.md`.
- The base already includes the two subprocess regression tests; they must be qualified and strengthened, not represented as newly invented coverage.

## Incremental command record

Before extraction:

1. `git rev-parse HEAD; git status --short` — base above, clean at initial inspection.
2. Initial combined inspection ended with `cargo test -p octet-coding-agent --lib cli::tests --profile ci-test --locked` under an accidentally short 1000ms shell timeout — **INTERRUPTED**, signal 9 during dependency compilation, not a test result.
3. `cargo test -p octet-coding-agent --lib cli::tests --profile ci-test --locked` — **PASS**, 80 passed, 0 failed (48.51s including compilation). Compiler emitted 26 existing warnings outside `cli.rs` (pi/package, presentation, auth/copilot, tui/composer). Expected unknown-key test warnings appeared on stderr.

4. `cargo test -p octet-coding-agent --all-targets --profile ci-test --locked` — **BLOCKED**, exit 101 before tests (21.77s). Concurrent, unowned `tui/keymap.rs` changes added `DispatchQueued`, `EditQueued`, and `Queue` variants while `modes/interactive.rs:337,1415` still had non-exhaustive matches (E0004). No configuration production files had been changed.
5. `git status --short; cargo test -p octet-coding-agent --test configuration_diagnostics_full --profile ci-test --locked` — **BLOCKED**, same unrelated E0004 before tests (5.20s). Status confirmed concurrent changes in ai client/protocol, extensions, and keymap paths. Those paths were not edited by this worker.

6. `rustfmt --edition 2021 crates/octet-coding-agent/tests/configuration_diagnostics_full.rs` — **PASS**, owned integration file only. Followed by `cargo test -p octet-coding-agent --test configuration_diagnostics_full --profile ci-test --locked` — **FAIL**, 4 passed/2 failed (15.29s). Newly strengthened strict-output assertions initially assumed literal newlines. Inspection of `src/lib.rs:57` and `src/output.rs` confirmed the existing process error boundary sanitizes embedded newlines to literal `<U+000A>`; production behavior was not changed.
7. Same file-local formatting and subprocess test commands after correcting expected sanitization — **FAIL**, 5 passed/1 failed (14.39s); one new exact assertion still omitted the final output newline. Corrected the expected fixture, not production.
8. With `set -o pipefail`: `cargo test -p octet-coding-agent --test configuration_diagnostics_full --profile ci-test --locked 2>&1 | tail -n 15` — **PASS**, 6 passed (1.29s test time). This is the strengthened process-boundary baseline, before extraction. It verifies exact warning and sanitized strict stderr, layer/source ordering, untrusted-project exclusion, environment/CLI strict precedence, aliases, and ignored legacy keys.
9. With `set -o pipefail`: `cargo test -p octet-coding-agent --all-targets --profile ci-test --locked 2>&1 | tail -n 65` — **FAIL**, 1249 passed, 2 failed, 1 ignored in the lib suite (16.94s test time); integration suites not reached. Failures: `app::bootstrap::tests::unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes` (pi-provider runtime startup Launch / Parked); `modes::interactive::tests::cancellation_retains_answer_draft_and_ordered_undelivered_steering` (missing first/second queued text). Both occurred before any configuration production extraction; the prior concurrent compilation blocker had cleared. Steps 8–9 took 76.70s together including builds.

After extraction:

10. Moved the contiguous `ConfigSourceKind`–`read_layer` implementation into private `src/cli/config_diagnostics.rs`; only visibility, imports, and formatting differ. Four diagnostic unit tests moved with it; CLI flag parsing and other policy/merge/persistence tests stayed in the parent. Added five loader-boundary regressions (missing paths, malformed/value/UTF-8 failures, one-MiB limit/regular files, symlinks, inline locations/legacy ignored keys).
11. `rustfmt --edition 2021 crates/octet-coding-agent/src/cli/config_diagnostics.rs crates/octet-coding-agent/tests/configuration_diagnostics_full.rs` — **PASS**, only the two owned files formatted. `cli.rs` was not wholesale formatted.
12. With `set -o pipefail`: `cargo test -p octet-coding-agent --lib cli:: --profile ci-test --locked 2>&1 | tail -n 95` — **PASS**, 85 passed, 0 failed (0.08s test time, 29.14s combined with formatting/build). This includes all 80 prior CLI tests plus five loader regressions, with the four moved tests now under `cli::config_diagnostics::tests`.
13. With `set -o pipefail`: `cargo test -p octet-coding-agent --test configuration_diagnostics_full --profile ci-test --locked 2>&1 | tail -n 15` — **PASS**, 6 passed, 0 failed (2.30s test time), matching the pre-extraction process baseline.
14. With `set -o pipefail`: `cargo test -p octet-coding-agent --all-targets --profile ci-test --locked 2>&1 | tail -n 65` — **FAIL**, 1266 passed, 2 failed, 1 ignored (19.80s test time). The bootstrap Pi provider Launch failure persists. New concurrent `extensions::hook_tests::post_mutation_rescan_consumes_current_resources_and_rejects_changed_or_stale_sources` failed at `src/extensions/hook_tests.rs:183`; the earlier interactive cancellation failure no longer appeared. Integration suites not reached. Steps 13–14 took 83.56s together including builds. No unowned failures were modified.

15. Initial Python in-memory extraction comparison — **FAIL in the comparison script**, because stripping whitespace did not normalize the two trailing parameter commas introduced by rustfmt. Inspected the exact diff: only the reporter/loader signature layout differed. Repeated comparison using `rustfmt --edition 2021 --emit stdout --config skip_children=true` on both extracted blocks after removing `pub(super)` — **PASS**. Also asserted byte-for-byte equality of parent `cli.rs` against base minus the extraction/four moved tests plus the intended import wiring — **PASS**. Neither comparison changed source files. In-memory base and candidate `cli.rs` `rustfmt --edition 2021 --check --config skip_children=true` checks returned 0 with no diff hunks.
16. With `set -o pipefail`: `cargo check -p octet-coding-agent --all-targets --locked 2>&1 | tail -n 18` — **PASS** (44.24s Cargo time). Existing warnings remain outside configuration paths (26 lib-test warnings, 13 duplicates).
17. `rustfmt --edition 2021 --check crates/octet-coding-agent/src/cli.rs crates/octet-coding-agent/src/cli/config_diagnostics.rs crates/octet-coding-agent/tests/configuration_diagnostics_full.rs` — **PASS**; no global formatter was used.
18. With `set -o pipefail`: `cargo test -p octet-coding-agent --lib unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes --profile ci-test --locked 2>&1 | tail -n 25` — **FAIL**, 0 passed/1 failed (0.16s test time; steps 17–18 took 42.99s including compilation). The Pi-provider Launch/Parked bootstrap failure reproduces independently; its fixture is `src/app/bootstrap/tests.rs:3030` and launches the checked-in Node Pi bridge. No configuration production behavior was modified to bypass it.
19. `git diff -- crates/octet-coding-agent/src/cli.rs crates/octet-coding-agent/tests/configuration_diagnostics_full.rs docs/design/config-diagnostics.md docs/qualification/configuration-diagnostics-full.md` and `git diff --no-index -- /dev/null crates/octet-coding-agent/src/cli/config_diagnostics.rs` — reviewed the complete owned diff (the no-index command correctly reports a new-file difference).
20. `git diff --check -- crates/octet-coding-agent/src/cli.rs crates/octet-coding-agent/src/cli crates/octet-coding-agent/tests/configuration_diagnostics_full.rs docs/design/config-diagnostics.md docs/qualification/configuration-diagnostics-full.md docs/swarm-audit/EXECUTION-config.md` — **PASS**.

## Candidate file fingerprints

`shasum -a 256 crates/octet-coding-agent/src/cli.rs crates/octet-coding-agent/src/cli/config_diagnostics.rs crates/octet-coding-agent/tests/configuration_diagnostics_full.rs docs/design/config-diagnostics.md docs/qualification/configuration-diagnostics-full.md`:

```text
92ec83d75ace15ab278e87f818a8a0cef07692224f0970a6c7aab50631d41247  crates/octet-coding-agent/src/cli.rs
0ac4c2fea6ef0be41b86909149770dd47f9596eb6c3038e708a4f9579a6a215b  crates/octet-coding-agent/src/cli/config_diagnostics.rs
c4d8cd9c8621d82483c373787dc023296faa3c3d472e6e5c77e6d8eb0c3ca2fd  crates/octet-coding-agent/tests/configuration_diagnostics_full.rs
5aad3a78be9b78938b26a0796558b374beb5147b590a61fdf0345cc737327274  docs/design/config-diagnostics.md
435c949297af8bc99e01461c7a4a28232e8f70428e1b967769800174e9caf745  docs/qualification/configuration-diagnostics-full.md
```

## Status and unrun checks

Extraction and focused regressions pass. Full-crate qualification remains blocked by unrelated failures in the shared candidate. Workspace-wide format/check/test/doc-test/lint, Cargo audit/deny (both workspaces), and web npm dependency/security checks from `CONTRIBUTING.md` were **NOT RUN by this worker** and remain the integrating parent's responsibility. No commits or changes outside the owned paths were made by this worker. The implementation does not unify the setting/key inventory or change other CLI behavior.
