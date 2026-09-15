# First-run setup current-candidate qualification

**Issue:** #274
**Recorded baseline:** `73a80ada0c85b66e443c320703f03fa4924a1ae2`
**Status:** source-only repair candidate. No Cargo, test, formatter, build, or
acceptance command was run after the frozen-stack timeout; this document makes
no current pass or release-qualification claim.

This candidate qualifies the existing guided setup adapter rather than rebuilding
`ProviderSetupService`. The service remains the sole transactional registry and
catalog boundary; the TUI only gathers input, drives one selected endpoint, and
presents the secret-free receipt before confirmation.

## Implementation boundary reviewed

- `crates/octet-coding-agent/src/modes/interactive.rs` owns the guided
  access-path, endpoint, credential, discovery/manual, recovery, review, and
  cancellation journey. Secret entry uses the bounded extension-input surface.
- `crates/octet-coding-agent/src/tui/pickers.rs:382` routes setup steps through
  the ordinary select-list surface and `PanelAction::ProviderSetup`.
- `crates/octet-coding-agent/src/provider_setup.rs:524` owns the no-write
  transaction, one-endpoint probe, receipt/authority projection, CAS commit,
  and canonical catalog rebuild.
- `crates/octet-coding-agent/src/modes/interactive.rs` gates onboarding on an
  empty runnable catalog and no explicit CLI model. Successful setup updates
  the in-memory catalog without making a resumed-session model an invocation
  override.
- `crates/octet-coding-agent/src/app/bootstrap.rs:5136` retains session model
  provenance before model-specific defaults are selected; the existing
  configured-startup path therefore remains outside onboarding.

## Fixture matrix

The dedicated `crates/octet-coding-agent/tests/setup_tui_acceptance.rs` fixture
uses isolated HOME/workspace/session directories, a VT100 parser, private
loopback-only HTTP, and no live model or auth flow. The fixture now bounds each
PTY read poll and retained transcript, bounds server reads/writes and wake-up,
checks the stop flag before processing its teardown wake-up, and kills/reaps
the PTY process group on timeout or drop with a direct-child fallback. The setup
CLI dispatch also isolates its synchronous provider setup from Tokio. These are
fixture and dispatch boundaries; no production TUI lifecycle or scrolling code
was changed.

| Journey | Evidence in the dedicated test | Current qualification state |
| --- | --- | --- |
| First access-path screen and endpoint entry | `setup_discovery_reaches_normal_prompt_and_records_secret_free_receipt` | Unverified after repair |
| Discovery, review, authority receipt, confirmation, normal prompt path | same | Hung in the frozen pre-repair run; rerun required |
| Auth failure and retry | `setup_auth_failure_supports_retry_edit_back_manual_review_and_cancel` | Only an `ok` line appeared before the incomplete suite ended; not an acceptance pass |
| Endpoint back/edit and manual model recovery | same | Unverified after repair |
| Review edit and explicit cancellation with no registry write | same | Unverified after repair |
| Narrow, ASCII, no-color rendering and width bounds | `setup_manual_narrow_ascii_no_color_and_configured_startup_remain_bounded` | Hung in the frozen pre-repair run; rerun required |
| Existing configured startup does not offer onboarding | same | Unverified after repair |

The secret assertion is negative: the test types a fresh fixture value only into
the bounded secret surface and checks that it does not occur in VT100 frames,
setup diagnostics, or saved registry/configuration state.

## Retained frozen-stack failure

The immutable verifier checkout was `71e2c317dc0559654423a9485ef3a2b7d76ab6e4`.
Both logs are incomplete failures and retain all observed outcomes:

- `04-setup-tui-acceptance.log`: compilation completed; auth reported `ok`,
  discovery and manual reported running for over 60 seconds; the wrapper hit
  its 900-second tool timeout with no Cargo exit status.
- `04b-setup-tui-acceptance-serial-diagnostic.log`: auth reported `ok`,
  discovery remained running; the wrapper hit its 300-second timeout with no
  Cargo exit status.

The failure was a fixture teardown/read-loop defect, not evidence that the
production setup journey passed or failed. `PtyTerminal::read_available` only
returned on `WouldBlock`/EIO; continuous renderer output could starve the
outer 8-second screen deadline. The repair limits work per read poll and caps
retained output. TCP write/read and child-group cleanup are also bounded so an
assertion or timeout cannot leave a fixture worker or child behind.

The frozen #429 patch changes slash-popup selection dispatch in
`src/modes/interactive.rs` and `src/tui/view.rs`. Provider setup owns a panel
picker and does not route these selection keys through that slash path. The
separate old `/changelog\r\r` fixture at
`crates/octet-coding-agent/tests/startup_frame_pty.rs:2644` is transferred to
command-control; first-run-core did not edit it.

## Historical and proposed verification

The earlier #274 candidate recorded 3 TUI tests, 3 reasoning-default tests, 9
provider-setup tests with 1,243 filtered, and 15 startup PTY tests passing at
`25373b9d05d63c3c89842cce703f03fa4924a1ae2`. Those results predate the frozen
stack and this repair; they are historical evidence only.

The following focused command is proposed for centralized verification and is
**unrun** in this repair phase:

```sh
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-coding-agent --test setup_tui_acceptance -- --nocapture
```

Formatting, compilation, the integrated stack, physical terminals, live
providers/credentials, and final review remain separate gates. Source edits
remain uncommitted.
