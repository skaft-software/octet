# Independent host-boundary review

## Outcome — final frozen receipt

The four original findings below are repaired with **passing behavioral
evidence**, including the real-binary two-session RPC/EOF regression. Final
workspace check, library and recorded integration receipts are green; earlier
failed/interrupted runs are historical. This is not blanket security, parity
or release approval. Documentation is frozen after this final refresh.

### P1 — ephemeral accounting omitted earlier RPC sessions: fixed, units/process verified

The original `finish_ephemeral_run` selected only the newest transcript before
removing every session. It now collects all regular invocation-owned session
transcripts into one accounting record (`crates/octet-coding-agent/src/session_store.rs:1637–1751`).
`ephemeral_finish_accounts_for_all_sessions_including_an_empty_newest_session`
**passed** in `/tmp/octet-final/coding-lib-final-receipt.log`; an empty newest session no
longer erases earlier costs/uncertainty. The additional real-binary
`no_session_rpc_preserves_both_sessions_accounting_before_discarding_transcripts`
test **passed** in `/tmp/octet-final/cli-critical.log` (37 tests, exit 0). It seeds
usage in two idle RPC sessions and drives `new_session`/EOF without an inference
request. See [bootstrap review](REVIEW-bootstrap.md).

### P1 — failed accounting append destroyed recovery: fixed, units verified

The original teardown consumed run state and deleted its only accounting source
on append failure. The new `finish_ephemeral_run_state` stages a private
accounting-only recovery snapshot, deletes conversation data, and retains the
snapshot and pending state after failed append. Stable invocation keys and locked
ledger deduplication/torn-tail repair make retry idempotent (`session_store.rs:1653–1751,1951–2039`).
`ephemeral_append_failure_keeps_private_accounting_only_and_retries_once` and
`ephemeral_accounting_retry_repairs_a_torn_append` **passed** in `coding-lib-final-receipt.log`.
Total inability to persist the recovery snapshot itself permits only in-process
retry; the implementation does not claim durable recovery under total filesystem
failure, nor an automatic recovery CLI. This bounded qualification closes the
original append-failure bug without claiming impossible storage guarantees.

### P2 — unimplemented theme service was offered: fixed, process verified

`crates/octet-agent/src/extension_process.rs:10442–10467` now removes optional
`theme_selection` and its methods from the product offer. The unit test passed in
`agent-lib-confirmed.log`. The real Python-peer process regression
`unimplemented_theme_selection_is_not_offered_and_returns_a_canonical_refusal`
**passed** in `/tmp/octet-final/agent-integrations-final.log`. The peer receives the exact
API 0.3 `-32601 / unknown or unnegotiated method` response and shuts down healthily.
Generated schemas remain unchanged; no principal is manufactured from a peer's
namespace. **Theme selection remains unavailable**, not implemented. This
supersedes `docs/parity/extensions.md`'s historical host-mediated claim.

### P2 — Codex override exceeded live maximum: fixed, unit verified

`crates/octet-coding-agent/src/codex_context.rs:515–533` intersects the checked-in
family ceiling with the authenticated discovered maximum. The regression
`live_discovery_bounds_acknowledged_overrides_below_the_family_table` **passed**
in `coding-lib-final-receipt.log`: rejects acknowledged 500K against live 400K, permits
acknowledged 400K with uncertainty, retains the ordinary 272K cap. Existing plan
and acknowledgement policy is unchanged.

## Remaining capability/release limits

- Source-inspected product `open-all` now marks every parent/worker pane
  unresolvable pending atomic host writer claim/settlement
  (`extensions/octet-subagents/octet_subagents/launcher.py:279–332`). A fresh
  `launchable` snapshot is not authority to start a competing writer. The owner
  now reports **88 passing tests**, including repeated fresh-launchable requests
  with zero product pane effects. Refusal is verified; pane handover is not implemented.
- MCP's exact enabled static `(server, variable)` credential binding is repaired;
  extension-owner evidence is **76 passing tests**, including mixed broker/static
  isolation. No stock OAuth/credential broker authority was added.
- Parent corrected the three importer entrypoints' Git modes to **100755**
  (independently inspected via `git ls-files -s`). Release packaging separately
  rejects API 0.3 at `scripts/package-octet-extension-release.sh:119–120` despite
  runtime bundle support. Modes and runtime tests do not qualify that packager.
- Host `bus/*`, principal-bound theme catalog/selection, configuration/migration
  PostMutation emitters and durable delegated ownership/mailbox recovery remain
  missing. Native/live/public qualification remains separate.

## Scope, boundary observations and evidence

Base `df5a7e809715961b9344af6b52e43a6ca48f56b3` through shared worktree on HEAD
`e01293917f452da94d603cdf3c43012b8365a627`. Only
`docs/parity/{README,VERIFICATION,REVIEW-host-boundary}.md` are writable here.
No source edits, Git mutations, remote operations or Rust/Swift/build commands
were performed by this worker.

Complete relevant documentation reads preceded the initial inspection; this
refresh re-read REVIEW-bootstrap/agent/ai/extensions fully, inspected changed
production branches, and read parent test receipts. Product launch still requires
UnsafeHost, process permission, enablement and trust; implicit trust does not
write config. Native host protocol 1 retains Controlled effects and is distinct
from executable-extension API 0.3. Executable extensions retain user OS authority;
capability metadata is not a sandbox. Product provider authorization remains
unavailable/revoked. PostMutation rescans do not confer reload/start authority;
opaque delegated path checks do not confer a lifetime writer lease.

Earlier independent checks: **14 Python theme helper tests passed** and an
in-memory generated-wire probe reproduced the original offer/error contradiction.
Those are diagnostic history, superseded for the product defect by the passing
real-process refusal test. Final compilation/execution counts, historical
failures and Serve/web/audit receipts are in [VERIFICATION.md](VERIFICATION.md#final-review--final-audit-post-crash).
No full-surface security assurance or extra session/artifact ID is claimed.
