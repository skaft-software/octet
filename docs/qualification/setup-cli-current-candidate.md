# Provider setup CLI current-candidate qualification

**Issue:** #275
**Recorded starting HEAD:** `25373b9d05d63c3c89842cce703f03fa4924a1ae2`
**Status:** source-only repair candidate. The CLI fixture, the repaired TUI
fixture, and all proposed Cargo commands below are unrun in this phase; no
acceptance or release-qualification claim is made.

## Boundary reviewed

- `crates/octet-coding-agent/src/provider_setup.rs:723` remains the thin CLI
  adapter over the shared transactional `ProviderSetupService`; it does not
  create a second registry or an `Agent`.
- `crates/octet-coding-agent/src/lib.rs:135` dispatches `setup` after ordinary
  configuration resolution and before any frontend launch. Its synchronous
  provider-setup adapter runs inside an awaited `spawn_blocking` boundary, so
  the blocking HTTP client cannot construct or drop a Tokio runtime on the
  executor.
- `crates/octet-coding-agent/src/app/bootstrap.rs:5395` keeps print/RPC model
  resolution non-interactive and names the deterministic `octet setup --yes`
  recovery command when no model is available.
- The guided TUI uses the same receipt, endpoint probe, manual inventory,
  compare-and-swap commit, and catalog rebuild at
  `crates/octet-coding-agent/src/modes/interactive.rs`.

## Dedicated fixture matrix

`crates/octet-coding-agent/tests/setup_cli_acceptance.rs` starts the real
`octet` binary with isolated HOME/workspace/session directories. Its online
cases use only a bounded loopback HTTP listener. Every child has null stdin;
no fixture invokes a provider model or a live credential.

The real-child boundary is now explicit: stdout and stderr go to private
captures rather than potentially blocking parent pipes, `wait_for_child` polls
the existing 8-second `WAIT` deadline, and a timeout kills and reaps the child
process group on Unix (with direct `Child::kill` fallback on non-Unix). `CapturedChild::Drop`
performs the same cleanup if an assertion or coordination wait fails. Capture
reads are capped at 256 KiB. The listener has bounded header reads, response
writes, and wake-up connect; no dependency or production file was added.

| Acceptance path | Dedicated evidence |
| --- | --- |
| Discovered success, explicit `--model`, environment credential, and secret-free receipt | `cli_setup_discovers_selected_model_uses_env_credential_and_matches_tui_receipt` |
| Manual model and explicit LM Studio preset in offline mode | `cli_setup_manual_review_cancel_and_offline_paths_do_not_probe_or_prompt` |
| Review-only cancellation with no registry/config write | same |
| Offline discovery failure with no endpoint request | same |
| Unreachable and authentication failures | `cli_setup_reports_unreachable_and_auth_failures_without_writing_state` |
| Registry compare-and-swap rejection after a competing writer | `cli_setup_rejects_a_concurrent_registry_change_instead_of_overwriting_it` |
| Print/RPC unresolved startup diagnostic, no wizard, and response-only stdout | `print_and_rpc_unresolved_startup_are_actionable_and_noninteractive` |

The success receipt checks the stable fields also exercised by the existing TUI
candidate: provider/model identity, direct traffic, authority/session facts,
and `OS isolation: none`. The CLI and TUI therefore use the same service
projection rather than maintaining separate receipt formats. The fixture also
checks that the environment key reaches only the selected `/models` probe and
never appears in CLI stdout, stderr, registry, or configuration.

## Privacy and concurrency controls

Setup reviews by default and only `--yes` reaches the registry CAS. Manual and
offline paths do not probe. The delayed loopback response lets the fixture write
a competing owner-private registry after the setup snapshot is captured; the
CLI must fail with the typed concurrent-registry state and preserve the
competing provider. No fixture reads or edits a session JSONL file.

## Proposed admitted commands

Every command must use the shared source-only admission prefix below, with
`--locked`; all are **unrun** after the repair:

```sh
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-coding-agent --test setup_cli_acceptance -- --nocapture
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-coding-agent --test setup_tui_acceptance -- --nocapture
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-coding-agent --lib provider_setup -- --nocapture
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-coding-agent --lib codex_luna_fallback_uses_exact_effort_choices_for_auxiliary_requests -- --nocapture
```

The Unix process-group cleanup is not a Windows acceptance result; the
non-Unix direct-kill fallback requires a native Windows verifier if this fixture
is admitted there. Formatting, compilation, integration-stack checks, physical
terminals, live providers/credentials, and final review remain separate gates.
Source edits remain uncommitted.
