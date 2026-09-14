# First-run setup current-candidate qualification

**Issue:** #274
**Recorded baseline:** `73a80ada0c85b66e443c320703f03fa4924a1ae2`
**Status:** candidate evidence executed; all owned guided-setup and relevant existing checks pass under the corrected isolated environment.

This document qualifies the existing guided setup adapter rather than rebuilding
`ProviderSetupService`. The service remains the sole transactional registry and
catalog boundary; the TUI only gathers input, drives one selected endpoint, and
presents the secret-free receipt before confirmation.

## Implementation boundary reviewed

- `crates/octet-coding-agent/src/modes/interactive.rs:4815` owns the guided
  access-path, endpoint, credential, discovery/manual, recovery, review, and
  cancellation journey. Secret entry uses the bounded extension-input surface.
- `crates/octet-coding-agent/src/tui/pickers.rs:382` routes setup steps through
  the ordinary select-list surface and `PanelAction::ProviderSetup`.
- `crates/octet-coding-agent/src/provider_setup.rs:524` owns the no-write
  transaction, one-endpoint probe, receipt/authority projection, CAS commit,
  and canonical catalog rebuild.
- `crates/octet-coding-agent/src/modes/interactive.rs:5283` gates onboarding on
  an empty runnable catalog and no explicit CLI model. Successful setup updates
  the in-memory catalog without making a resumed-session model an invocation
  override.
- `crates/octet-coding-agent/src/app/bootstrap.rs:5136` retains session model
  provenance before model-specific defaults are selected; the existing
  configured-startup path therefore remains outside onboarding.

## Fixture matrix

The dedicated `crates/octet-coding-agent/tests/setup_tui_acceptance.rs` fixture
uses isolated HOME/workspace/session directories, a VT100 parser, private
loopback-only HTTP, and no live model or auth flow.

| Journey | Evidence in the dedicated test | Execution status |
| --- | --- | --- |
| First access-path screen and endpoint entry | `setup_discovery_reaches_normal_prompt_and_records_secret_free_receipt` | Passed (1/1) |
| Discovery, review, authority receipt, confirmation, normal prompt path | same | Passed (1/1) |
| Auth failure and retry | `setup_auth_failure_supports_retry_edit_back_manual_review_and_cancel` | Passed (1/1) |
| Endpoint back/edit and manual model recovery | same | Passed (1/1) |
| Review edit and explicit cancellation with no registry write | same | Passed (1/1) |
| Narrow, ASCII, no-color rendering and width bounds | `setup_manual_narrow_ascii_no_color_and_configured_startup_remain_bounded` | Passed (1/1) |
| Existing configured startup does not offer onboarding | same | Passed (1/1) |

The secret assertion is negative: the test types a fresh fixture value only into
the bounded secret surface and checks that it does not occur in VT100 frames,
setup diagnostics, or saved registry/configuration state.

## Executed qualification

Every Cargo/test command below used `env -u OCTET_PACKAGE_DIR` and the shared
settings `CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target`,
`CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, and `CARGO_BUILD_JOBS=2`.

| Command | Result |
| --- | --- |
| `cargo test --locked -p octet-coding-agent --test setup_tui_acceptance -- --nocapture` | exit 0; 3 passed, 0 failed, 0 ignored |
| `cargo test --locked -p octet-coding-agent --test reasoning_defaults -- --nocapture` | exit 0; 3 passed, 0 failed, 0 ignored |
| `cargo test --locked -p octet-coding-agent --lib provider_setup -- --nocapture` | exit 0; 9 passed, 1,243 filtered |
| `scripts/test-startup-frame-pty.sh --nocapture` | exit 0; 15 passed, 0 failed, 0 ignored |

The startup command was run through the documented script and covered all 15
startup PTY cases. The broad 1,251 default-library, Serve-library, host-unit,
and provider-contract counts are preserved from integration's clean common
baseline qualification; this candidate changes no production library path.

## Failure diagnosis and scope

The first admitted compile attempt exposed an invalid named `format!` capture
inside `concat!`; the fixture now passes `status` and `content_type` positionally.
The first runtime attempt then exposed only fixture assumptions: the 80-column
review clipped the full receipt, retry input raced the fresh recovery render,
no-color emitted default reset SGRs that are not styling, and configured startup
shows the display name (`Existing Model`) rather than the canonical model ID.
The owned fixture now uses a wide receipt PTY, waits for the post-retry render,
ignores reset-only SGRs, and checks the visible configured model name. No
production source or scrolling code was changed.

The checks are bounded behavioral evidence, not performance claims or
live-provider qualification. No live credentials, physical terminal, Astra leaf
review, or issue-closure claim is included. Remaining acceptance is coordinator
integration of this candidate and any broader release-level qualification.
