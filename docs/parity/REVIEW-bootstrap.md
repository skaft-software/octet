# Final bootstrap/provider-auth review

Status: source edits settled; parent owns fresh compilation and execution.
Worker: `final-providers`. Shared worktree; no Git mutations, trust-policy
expansion, generated inventory changes, or recovery build commands.

## Delivery-blocking auditor findings repaired

### Ephemeral accounting across RPC sessions

`src/session_store.rs::finish_ephemeral_run` no longer selects only the newest
transcript or consumes run state before persistence. It reads all regular
workspace session transcripts and combines their usage, uncertainty and costs
into one invocation accounting record. An empty newer RPC session cannot erase
an earlier session's billed usage.

Before ledger append it stages private `.accounting-recovery.json` beneath the
invocation's temporary root and removes the conversation directory. On append
failure this accounting-only file and in-process pending state survive; the
error retains its original cause and reports the recovery path. Retrying is not
a no-op. Re-registering that root also reloads its recovery snapshot.

A stable accounting key and locked ledger read/deduplicate/repair/append sequence
prevent duplicate totals after an ambiguous completed append. Torn trailing
writes are repaired before retry. Old records without keys remain readable.
The ledger still counts invocations, not each RPC session as a separate run.

Regressions added in `src/session_store.rs`:

- `ephemeral_finish_accounts_for_all_sessions_including_an_empty_newest_session`
- `ephemeral_append_failure_keeps_private_accounting_only_and_retries_once`
- `ephemeral_accounting_retry_repairs_a_torn_append`

Real-binary regression in `tests/setup_cli_acceptance.rs`:
`no_session_rpc_preserves_both_sessions_accounting_before_discarding_transcripts`.
It drives `get_state` / `new_session` / EOF in an isolated offline RPC child and
seeds durable usage while each session is idle; it makes no inference request.
Both known cost and first-session uncertainty must survive, with no transcript.

Boundary: an inability to write the recovery snapshot itself stops before ledger
append, deletes conversations where filesystem permissions permit, and retains
in-memory accounting for retry. It does not claim durable recovery under total
filesystem failure. Oversized ledger records remain bounded and fail with their
accounting-only recovery intact; no automatic oversized-record splitting or
new recovery CLI command was added.

### Codex live entitlement ceiling

`src/codex_context.rs::resolve_codex_context_window` now intersects the checked-in
family ceiling with `discovered_max_context_window` before applying an override.
The new `live_discovery_bounds_acknowledged_overrides_below_the_family_table`
regression rejects acknowledged 500K against a live 400K maximum, permits explicit
400K with uncertainty, and keeps the ordinary 272K cap/clamp without uncertainty.
Existing plan and acknowledgement gates are unchanged.

## Other bounded repairs and preserved behavior

- AWS metadata activation reads shared-config `credential_source` before the
  credentials-file fallback. Existing opt-in/indication/suppression policy is
  preserved; no blanket metadata probing was added.
- `src/providers/auth.rs` bounds actual DMI bytes rather than rejecting sysfs's
  page-sized metadata. Added bounded-reader and Linux-conditional sysfs tests.
- Bootstrap trace emits `catalog.base` before selected inventory work and omits
  `catalog.selected` for Codex-only initialization. Process-isolated ordering
  regression: `selected_route_trace_brackets_inventory_after_the_cheap_base_phase`.
- Codex note delivery survives a same-transcript rebuild but resets for an
  explicitly selected different transcript. Extended the existing rebuild test
  with new-session and same-transcript reopen cases.
- Selected-route narrowing retains explicit namespace/resume/compaction route
  proof and conservative fleet repair for ambiguity/unresolvable routes. Existing
  activation/trust boundaries were not changed.

## Verification / handoff

Read the CLI, Codex context, provider, AI/product design and parity contracts;
inspected relevant source/tests and final scoped diffs. `git diff --check` passed
for owned paths. No fresh Rust execution is claimed by this worker.

Parent should run coding-agent library filters `ephemeral_`, `codex_context`,
`providers::auth`, the Codex notice rebuild test, and
`--test setup_cli_acceptance`; also retain existing `--test parity_cli` and
`--test codex_context_window` coverage. AI/agent retry tests are listed in
`REVIEW-ai.md`.

Artifacts: `/tmp/octet-final/tests-renderer-ai.log` is a historical successful
AI/renderer run predating the latest repairs; the inspected
`/tmp/octet-final/test-workspace-lowdisk.log` had reached compilation only.
The parent reports interrupted build invocations and will rerun on settled
sources. Linux sysfs coverage is conditional; this worker did not execute it.

### Parent execution and narrow fixture follow-up

Inspected `/tmp/octet-final/coding-lib-final.log`: **1551 passed, 15 failed,
1 ignored**. All three new ephemeral-accounting regressions, the live Codex
ceiling test, Codex note rebuild test, bounded DMI reader and shared-config
credential-source regression passed. This is not a clean full-library run.

The API 0.3 provider preflight regression failed with its enabled/trusted
`pi-provider` parked at `runtime startup Launch`. Ambient HOME extensions were
listed but disabled; they were not the identified cause. Its temporary package
contained only launcher/manifest, while `bridge.mjs` resolves `semantic_ui.mjs`
and `editor_handoff.mjs` under host-supplied `OCTET_EXTENSION_DIR`.

A bounded Node `--help` probe reproduced `ERR_MODULE_NOT_FOUND` when that variable
selected a directory without helpers (exit 1); selecting the complete package
exited 0. The test now copies bridge plus both helpers into its temporary package
and launches that copy. Production preflight, environment/trust behavior and all
provider registration/reload assertions remain unchanged. Scoped diff check
passed; parent must rerun
`unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes`.
No cargo/build command was run by this worker. Source edits are settled again.
