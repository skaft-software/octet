# TUI runtime final local review

## Scope and verification boundary

Exclusive recovery scope: `crates/octet-coding-agent/src/tui/**`, `src/modes/interactive.rs`, `tests/{slash_command_pty,activity_wait_pty}.rs`, and these review reports. Unrelated dirty work is preserved. No git mutations, remote operations, global formatting, cargo, rustc, Swift, or build commands were run. **Compilation and Rust/PTY test execution remain with the parent because of disk recovery.**

Read the starting report completely; read README, parity editor/TUI/telemetry, terminal, commands, session-format, subagents, themes, TUI-design, and subagents bundle README documentation; inspected the owned starting diff and relevant implementations/tests. Source inspection is not behavioral qualification.

## Actionable findings and fixes

1. **Old terminal rosters replayed after a new prompt**, including workers never observed live by this shell. `view.rs:2372` now refuses terminal-only rosters when no current block exists. Empty spawn-failure evidence is still admitted.
2. **Mixed rosters replayed old terminal workers.** Only active workers or members already attributed to the current transcript block are retained. This excludes previously unseen old terminal workers too, not merely IDs seen in an earlier turn. Current workers keep their terminal update in their original block; repeated terminal snapshots update rather than append. The obsolete session-wide seen-ID cache was removed. Previously accounted old-worker costs no longer re-enter this turn's provisional footer sum.
3. **Live strip was unconditionally empty.** `view/shell_chrome.rs:143` now renders only active workers during an active owning run, with history filters disabled for that transient projection. Terminal workers disappear from the strip but remain in the single existing transcript block. No idle/new-prompt replay of completed workers.
4. **Silent startup accepted invisible input.** `view/shell_chrome.rs:203` paints the unbranded composer even without a lifecycle label; setup panels/overlays retain ownership, and the provisional model footer remains hidden. The startup frame regression now checks the actual draft and raw cursor marker instead of accepting a blank frame. Stripped plain text cannot prove cursor placement, so captured frame strings retain the cursor marker.
5. **Goal and cost review.** `/goal` already reaches the shared durable `GoalAccess` in `modes/interactive.rs:2028` without queueing; existing unit fixtures exercise mid-stream mutation and failure. Added a held-SSE real-binary PTY test (`tests/slash_command_pty.rs:450`) for set/status/clear before the response tail is released. Plain-dollar footer behavior is already implemented in `tui/composer_surface.rs:791`; telemetry keeps plain provider-reported labels and dollars without changing durable `usage_uncertain`. Existing pricing/uncertainty absence tests are retained.

## Behavioral regression coverage added/updated (not executed here)

- `view/subagent_stability_tests.rs:220`: native and extension entry points; previously seen and unseen terminal workers; mixed new-active/old-completed rosters; repeated settlement in the same block; no replay under a third prompt; legitimate continuation of a previously completed worker; old-worker cost exclusion.
- `view/subagent_stability_tests.rs:285`: positive active-strip rendering, mixed current-worker settlement, filter independence, bounded widths/heights, composer cursor, and one settled transcript heading.
- `view/tests.rs`: retained completed-only/no-replay regressions; renderer/control fixtures now first observe their current-turn workers in bounded waves rather than treating an old session roster as new delegation (`publish_current_turn_roster`, line 10477).
- `view/status_telemetry.rs:326` and `view/startup_readiness_tests.rs:227`: silent startup draft/cursor visible, no startup/extension label, one ready branded frame, and native/application viewport coverage.
- `tests/slash_command_pty.rs:450`: `/goal` during a provably held response. Goal is cleared before releasing the response, avoiding an unintended continuation request.
- Shimmer review: see [REVIEW-rendering.md](REVIEW-rendering.md).

## Observed local checks

- `git diff --check` over all changed owned Rust paths: exit 0.
- Python text/arithmetic check of the actual shimmer constants: widths 1/7/8/18/36/80 have periods 13/19/20/30/48/92; every cell is visited and both endpoints are at rest. This is **not** Rust execution or emitted-ANSI verification.
- No Rust tests, PTYs, formatting check, or physical-terminal acceptance run in this recovery worker.

## Exact recommended parent commands

Run serially, using the parent's disk-safe target directory/profile settings:

```sh
cargo test --locked -p octet-coding-agent --lib tui::view -- --nocapture
cargo test --locked -p octet-coding-agent --lib tui::composer_surface -- --nocapture
cargo test --locked -p octet-coding-agent --lib goal -- --nocapture
cargo test --locked -p octet-coding-agent --test slash_command_pty --test activity_wait_pty -- --nocapture
```

Remaining qualification: compile and run the above, investigate failures, and confirm physical-terminal appearance. The roster is intentionally attributed by observed live/current-block membership: terminal workers first encountered without such membership are not fresh turn material. Host/session accounting and inspection remain untouched. No worker session/artifact identifier was exposed beyond these report paths.


## Follow-up after parent full-library run

Parent evidence: `/tmp/octet-final/coding-lib-final.log` reported **1551 passed, 15 failed, 1 ignored**. Reviewed all failure sections. Twelve failures are in this worker's scope; the bootstrap/provider and two Serve failures are outside it.

Corrections ready for parent rerun:

- **Actual rendering defect:** subagent cells now clip as plain text before theme styling (`view.rs::truncate_subagent_cell`), eliminating the generic truncator's orphan SGR resets in no-color collapsed summaries, grids, and failure rows. New raw-row test covers ASCII/Unicode, narrow/wide, collapsed/expanded. The existing full-color-mode assertion is unchanged.
- **Actual viewport defect:** the active strip yields while `follow_tail == false` (`shell_chrome.rs:143`), so incoming worker changes cannot steal rows from the reader's anchored history. Worker blocks continue updating in the transcript, and the strip returns at the live tail. All three original scroll regression assertions are unchanged; added a hide/restore plus exact visible-anchor test.
- **Actual picker defect:** a selected worker moving into a collapsed terminal group is revealed individually by stable node ID, without expanding siblings or silently selecting a different worker. Panel budgeting/counts respect that individual visibility. Explicit collapse/filter controls clear the reveal. Added repeated-refresh/hidden-sibling/Enter regression; the async picker test now waits for an actual refresh signal rather than racing two sleeps.
- **Fixture corrections:** settle the parent run before the mixed-roster test starts its third run; give current-worker test rows distinct visible task names and assert old attribution exactly once; point the active-session inspection fixture's store at its actual directory (`SessionStore::for_directory`), rather than a nonexistent workspace-key child. Export's report/file assertions remain intact.
- **Presentation contract correction:** footer/telemetry assertions now require plain dollars (or absent unreported cost), not obsolete subtotal/unknown decorations. `usage_uncertain`, resume persistence, fresh-session reset, and absent durable-cost assertions remain explicit. Configured zero-price display is not treated as clearing uncertainty.
- **Clipboard fixture scheduling:** successful subprocess decode/exit-status cases use an injected 10-second test deadline under the heavily parallel suite. Production still uses 600 ms; the wedged-helper case still calls that production path and retains its bounded-duration assertion. The fixture uses `exec sleep` so its timeout target is the actual child rather than an orphanable shell descendant. This does not increase the production timeout.

Observed since these corrections: owned-path `git diff --check` exit 0. **No cargo/build/test execution by this worker.** Ready for the parent to rerun:

```sh
cargo test --locked -p octet-coding-agent --lib tui:: -- --nocapture
cargo test --locked -p octet-coding-agent --lib modes::interactive::tests::active_session_commands_report_through_the_read_only_session -- --nocapture
cargo test --locked -p octet-coding-agent --lib modes::interactive::clipboard_read::tests -- --nocapture
```

The follow-up changes are not yet runtime-qualified; they do not claim the full-library failures have passed.
