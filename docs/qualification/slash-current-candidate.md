# #429 slash Enter current-candidate qualification

**Status:** Repair candidate; source-only and uncommitted. This record does not claim
release, installed-binary, physical-terminal, provider, or live-inference
qualification.

## Identity and scope

- Frozen qualification stack: `71e2c317dc0559654423a9485ef3a2b7d76ab6e4`.
- Prior #429 source candidate retained in that stack: `b1997393970d29ee6401657267f828ee25b66c37`.
- Observed failing check: `startup_frame_pty` at the frozen stack; the dedicated
  `slash_command_pty` check passed.
- This repair is limited to the startup fixture's stale two-Enter input and this
  qualification record. The prior #429 production and test edits remain intact;
  no media-owned `view.rs` or `view/tests.rs` edit is made.

## Cause and repair

The selected-slash Enter behavior is intentional. In
`crates/octet-coding-agent/src/tui/view.rs:3427-3483`, `SlashMenuAction::Select`
puts the highlighted command (and, for argument-taking commands, its trailing
space) into the editor, dismisses the popup, and reports success. The idle path
in `src/modes/interactive.rs:337-347` and the active path at `1399-1413` then
send that text through the ordinary command dispatcher. `keymap.rs:300-365`
keeps modified Enter as newline, Tab as completion-only, and whitespace-bearing
command text on the normal command path. The frozen `slash_command_pty` result
confirmed the one-Enter real-binary journey.

The failing startup test's idle branch instead wrote `/changelog\r\r`
(`startup_frame_pty.rs:2644` in the frozen snapshot). The first Enter invoked the
visible `/changelog` command and opened its report. The second Enter was not a
second command invocation: `view.rs:4525-4582` gives the report its input
lifetime, and a non-navigation key closes it. The failure log consequently shows
the ordinary welcome/prompt while the test waits for `Fixed`. Initial CLI
`/changelog` and unique `/chang` with trailing whitespace are handled by
`prepare_startup_input` (`interactive.rs:4765-4780`) and open the report before
prompt input; `/changelog` is also excluded from response prewarm at
`4783-4786`.

The repair changes the idle fixture to one Enter and documents why a second
Enter would close the report. Its provider-zero-submit assertion, session-byte
comparison, shutdown checks, and all fixture budgets remain unchanged.

## Repair delta

- `crates/octet-coding-agent/tests/startup_frame_pty.rs`: replace only the idle
  `/changelog\r\r` input with `/changelog\r`, with an ownership/lifetime comment.
- `docs/qualification/slash-current-candidate.md`: this record.

No changes were needed in `interactive.rs`, `src/tui/keymap.rs`,
`src/tui/view/changelog_tests.rs`, `crates/octet-coding-agent/tests/slash_command_pty.rs`,
or `docs/commands.md`; their prior candidate edits are preserved. The existing
command documentation already states that Tab completes and Enter invokes the
highlighted slash command.

## Frozen evidence

- `agents/verify-rust/logs/15-slash-command-pty.log`: one test passed,
  `real_octet_slash_enter_invokes_highlighted_command_in_one_submission`.
- `agents/verify-rust/logs/06-startup-frame-pty.log`: 14 tests passed and
  `real_octet_changelog_initial_and_idle_never_submit_to_provider` failed while
  waiting for `Fixed`; the final screen was the normal welcome/prompt.
- The frozen startup command used the unchanged `STARTUP_TIMEOUT`; no timing,
  expected-content, or provider-zero-submit assertion was weakened.

## Proposed admitted verification (not run in this repair session)

Run separately against the integrated candidate, with the verifier's required
Cargo environment prefix and `--locked`:

```text
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-coding-agent --test startup_frame_pty -- --nocapture
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-coding-agent --test slash_command_pty -- --nocapture
```

Also rerun the focused interactive/keymap/changelog filters and formatting check
for the changed Rust files under centralized verification. The old frozen
`rustfmt --check` result is not a repair result: it exited 1 because of
pre-existing formatting differences in `crates/octet-agent/tests/recovery_current.rs`.

## Handoff and limits

- **verify-rust:** rerun both PTY targets above at the integrated source. Retain
  the complete startup matrix and the provider request count/session-byte
  assertions; do not restore the second Enter.
- **Coordinator/Luna integration:** collect the uncommitted one-line fixture
  repair and this doc. The prior #429 patch still overlaps temporarily
  media-owned `view.rs`/`view/tests.rs`; no additional handoff to those files is
  required for this observed failure.
- **Unrun:** no Cargo tests, builds, rustfmt, Clippy, or other formatter were run
  in this repair session. No new installed binary was built or exercised.
- **Physical/live limits:** no physical Ghostty/terminal observation, live
  provider request, credential flow, remote service, or long-duration run was
  performed. The recorded PTY logs are frozen evidence for the prior candidate,
  not a claim that this uncommitted repair has passed.
