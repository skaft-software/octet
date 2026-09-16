# Independent verification of the Pi-parity pass

## Current candidate — qualification in progress

**Local 0.8.0 candidate; Pi parity is not achieved.** This section supersedes
historical status claims below only for the named scope and snapshot. The old
“final”, “green” and “authoritative” language belongs to its frozen review, not
today's moving source. Source readiness, compilation, bounded behavior, upstream
parity and release qualification are different claims. This refresh reads parent
receipts and owner reports; it is not a new independent runtime/security audit.

### Latest bounded parent receipts

**Integration failures observed while writers were mid-change, then repaired and
rerun (all preserved, none deleted):**

| Receipt under `/tmp/octet-final/` | Exact command | Observed result |
| --- | --- | --- |
| `parity-next-check-11.log` | `cargo check --workspace --all-targets --all-features --locked --offline` | Exit 101; sole compiler error E0583: in-progress kernel module `auxiliary_settlement_tests` missing. |
| `parity-next-export-tests.log` | `cargo test -p octet-coding-agent --lib --locked --offline export_ -- --test-threads=2` | Exit 101 before tests; E0063: in-progress `BusEventParams` initializer lacks `binding_id`. |
| `parity-next-coding-lib-03.log` | `cargo test -p octet-coding-agent --lib --locked --offline -- --test-threads=2` | **1519 passed, 2 failed, 1 ignored**. Failures: the API 0.3 bus contract validation in `extensions::bus_tests` and the bridge-launching provider preflight. |
| `parity-next-agent-tests-04.log` | `cargo test -p octet-agent --no-fail-fast --locked --offline -- --test-threads=2` | **559 passed, 9 failed, 1 ignored** in the library plus `agent_run` **154 passed, 1 failed**. |
| `parity-next-ai-tests-02.log` | `cargo test -p octet-ai --no-fail-fast --locked --offline -- --test-threads=2` | Library/targets green except `provider_parity` **26 passed, 2 failed** (fixture streams opened without a required start event). |
| `parity-next-coding-int-01.log` | `cargo test -p octet-coding-agent --no-fail-fast --locked --offline --test parity_cli --test eval_harness -- --test-threads=2` | `eval_harness` **9 passed, 1 failed** under concurrent load; the same target then passed four consecutive standalone runs (one serial, three at `--test-threads=2`). Recorded as an environment-sensitive flake, not a pass. |
| `parity-next-pi-bridge-tests.log` | `python3 -B -m unittest discover -s extensions/octet-pi-compat/tests -v` | **88 run, 36 failures, 3 skipped**: the bridge rejected the host's newly offered `bus/lifecycle` optional method. |

Every failure above was diagnosed to a concrete cause and repaired in source:
the bridge's optional-method/capability map omitted `bus/lifecycle`; the Python
SDK called `from_wire` on generated tagged unions (`BusLifecycleParams`,
`BusSubscribeResult`) instead of the generated `parse_*` selectors; the host
bus unit fixtures predated binding-scoped `dispatch`; two AI stream fixtures and
the kernel auxiliary-settlement fixture omitted the required `Started` event;
`agent_run`'s gated fixture reused content-block index 64 for a 65-call turn;
the bench harness could not resolve the authored fixture dependency it no longer
ships; and three tests still asserted pre-`OutputLimitUnavailable` or
pre-binding-era results. Later receipts below are the reruns.

Logs and matching `.exit` files are under `/tmp/octet-final/`. Commands below
used the parent's bounded serial runner, two build jobs and two test threads;
no Cargo, rustc, Swift, installation or global formatting ran for this refresh.
Counts include completed targets/doc tests within each command; overlapping
runs are not added together. Warnings remain.

| Receipt stem | Exact command | Observed result / boundary |
| --- | --- | --- |
| `parity-next-check-10` | `cargo check --workspace --all-targets --all-features --locked --offline` | Exit 0; Cargo 13.20s (runner 14.0s). Compilation, not all-feature runtime qualification. |
| `parity-next-coding-lib-02` | `cargo test -p octet-coding-agent --lib --locked --offline -- --test-threads=2` | Exit 0; **1517 passed, 0 failed, 1 ignored**. Library only, not CLI integration/PTY targets. |
| `parity-next-agent-tests-03` | `cargo test -p octet-agent --no-fail-fast --locked --offline -- --test-threads=2` | **Exit 101; 809 passed, 1 failed, 2 ignored**. Library 564 passed / 1 ignored and `agent_run` 153 passed are included, not extra totals. Sole failing target: `api_v03_runnable`, released manifest requires `=0.7.6` but candidate host is 0.8.0. |
| `parity-next-extension-bundle-02` | `python3 -B -m unittest discover -s scripts/tests -p test_extension_bundle.py -v` | Exit 0; **7 passed**, after directory-link/command-membership repair. Deterministic source-override fixtures, not official release/install qualification. |
| `parity-next-renderer-tests` | `cargo test -p sexy-tui-rs --locked --offline -- --test-threads=2` | Existing receipt unchanged: exit 0, **277 passed**. No physical-terminal acceptance implied. |
| `parity-next-renderer-no-syntax` | `cargo test -p sexy-tui-rs --no-default-features --locked --offline -- --test-threads=2` | Existing receipt unchanged: exit 0, **270 passed**. Not a new rerun. |
| `parity-next-ai-tests` | `cargo test -p octet-ai --locked --offline -- --test-threads=2` | Historical phase-1 receipt: exit 0, **480 passed**. **Not phase-2 or later AI qualification.** |

**Definitive serial rerun over one frozen snapshot** (`freeze-final.sha256`, 1635
files, **zero files changed during the run**): `f2-check` exit 0; `f2-coding`
**1703 passed, 0 failed across 30 targets**; `f2-agent` **819 passed, 0 failed**;
`f2-ai` **500 passed, 0 failed**. A single earlier `fv-coding` attempt failed only
`update::progress::tests::actual_updater_progress_pty_and_plain_streams` on its
2-second subprocess timeout, which the following full run passed; it is recorded
as load-induced flakiness, not silently dropped.

**Earlier post-repair serial rerun over one stable snapshot** (`parity-next-wave3-source.sha256`,
1635 files; the only file edited during the wave was `crates/octet-ai/tests/provider_parity.rs`,
which is not compiled by the agent/coding targets):

| Receipt stem | Exact command | Observed result / boundary |
| --- | --- | --- |
| `parity-next-check-12` | `cargo check --workspace --all-targets --all-features --locked --offline` | Exit 0; 52.3s. Compilation of the whole workspace including all features, not runtime qualification. |
| `parity-next-coding-lib-04` | `cargo test -p octet-coding-agent --lib --locked --offline -- --test-threads=2` | Exit 0; **1521 passed, 0 failed, 1 ignored**. |
| `parity-next-agent-lib-05` | `cargo test -p octet-agent --lib --locked --offline -- --test-threads=2` | Exit 0; **570 passed, 0 failed, 1 ignored**. |
| `parity-next-agent-int-05` | `cargo test -p octet-agent --no-fail-fast --locked --offline -- --test-threads=2` | Exit 0; every agent target ran: **819 tests passed, 0 failed**, including `agent_run` and both process-bus fixtures. |
| `parity-next-ai-parity-04` | `cargo test -p octet-ai --locked --offline --test provider_parity -- --test-threads=2` | Exit 0; **28 passed**. |
| `parity-next-ai-all` | `cargo test -p octet-ai --no-fail-fast --locked --offline -- --test-threads=2` | Exit 0; whole AI crate and targets. |
| `parity-next-renderer-04` | `cargo test -p sexy-tui-rs --locked --offline -- --test-threads=2` | Exit 0; **277 passed** (fresh rerun). |
| `parity-next-sdk-python` | `python3 -B -m unittest discover -s sdk/python/tests` | Exit 0 after repair; **109 passed**. |
| `parity-next-pi-bridge-tests-02` | `python3 -B -m unittest discover -s extensions/octet-pi-compat/tests` | Exit 0 after repair; **88 run, 85 passed, 3 real-runtime skips**. |
| `parity-next-scripts-02` | `python3 -B -m unittest discover -s scripts/tests` | Exit 0 after repair; **58 passed**. |
| — (direct) | `node sdk/typescript/tests/api_v03_conformance.mjs` | Exit 0; **47 fixtures** against the regenerated runtime. |
| — (direct) | `python3 -B scripts/generate-extension-api-v03.py --check` | Exit 0 after regenerating a stale TypeScript runtime artifact. |

`parity-next-coding-all` (one earlier attempt at the whole coding-agent target
set) is preserved as an integration failure: `embedded_documentation_preserves_current_public_source_text`
failed because the embedded archive had been built before the same run's later
source edits, and `migration_import` failed on the adapter's published-release
host pin. Both were repaired/rerun: `parity-next-coding-lib-06` passes the
documentation assertion on a fresh build, and the migration test now stages a
private copy carrying the current host version while asserting the tracked
manifest keeps its published pin. A fresh whole-target rerun is
`parity-next-coding-all-07`.

Parent reports all **1632 source hashes stable** for the earlier recheck snapshot
in `parity-next-recheck-source.sha256`; `parity-next-recheck.done` records
`coding=0 agent=101`. Later snapshots are `parity-next-wave3-source.sha256`
(1635 files) and `parity-next-wave5-source.sha256`. Those manifests are bounded
snapshots, not proof that still later source matches. No single uninterrupted workspace-test pass or
all-green candidate is claimed. `release-v0.8.0-confirmed.log` describes an older
preserved binary, not the newer candidate source; no installation/publication or
SDK distribution version bump follows (SDK remains 0.7.6).

**Preserved packaging-check failure caused by an uncommitted file:** `python3 -B
scripts/test-packaged-docs.py` stops at its first assertion because
`docs/releases/v0.8.0.md` is inventoried but not yet tracked by git, and this
session is not authorized to mutate git state (no `git add`/commit). Everything
the assertion would gate was verified read-only instead: all **454** inventoried
files exist, every reference-style/`src=`/`href=` target resolves, the **409**
tracked `docs/`, `examples/` and `sdk/` files are all inventoried, and the single
untracked inventory entry is that new release-notes file. A real release adds it
to the index; the substantive packaging assertions (deterministic archive,
installer extras, embedded/package byte equality) remain unrun here.

**Preserved non-Cargo failure caused by the local version bump (not by a feature
edit):** `python3 -B scripts/test_repository_identity.py` now reports **10 tests,
3 failures** — `test_first_party_manifests_locks_and_installer_match_workspace`
(the script derives the expected version from the workspace `Cargo.toml`, now
0.8.0, and asserts `sdk/python/pyproject.toml` and `sdk/typescript/package.json`
carry it), `test_current_version_has_release_only_notes` (`docs/installation.md`
still documents the *published* v0.7.6 download URLs), and the extension-manifest
case (the four tracked bundles still declare `version`/`requires_octet` 0.7.6).
Only `Cargo.toml` was modified among those files, so this is a release-identity
gate that a real 0.8.0 publication would satisfy by updating the SDK, installer
docs and bundle manifests. It is deliberately **not** satisfied here: the brief
keeps SDK/public distribution versions at 0.7.6 and authorizes no publication.
The historical receipt for this script passed 10/10 at 0.7.6.

Preserved failures: `parity-next-coding-lib.log` (**1503 passed, 9 failed,
1 ignored**); `parity-next-agent-tests-02.log` (library **564 passed, 1 ignored**;
`agent_run` **148 passed, 4 failed**, then stopped); initial agent compile failure
in `parity-next-agent-tests.log`; and failed `parity-next-check-09.log` plus earlier
check/product-check logs. Successful later receipts supersede those failures only
for their executed scope. The runnable-example failure above is still a failed
command even though its owner reports a test-fixture repair.

### Later changes are not qualified by those receipts

The parent's post-recheck work queue and `/tmp/octet-final/parity-audit.md`
identify these independent acceptance gates. Plans, parse checks and source
repairs do not close them:

- **Kernel F3/F4/F6:** completed invocation identity after checkout/reopen;
  bounded admission for batches above 64 calls; accepted auxiliary usage retained
  across same-poll cancellation and settlement-append failure. See
  `/tmp/octet-final/kernel-parity.md` and the audit's concrete counterexamples.
- **AI F7/F5 and new consumers:** protocol-aware sampling allowlist/caller-stop
  precedence; enforceable wire cap shared with hard-budget reservation; real
  RequestOverrides/Azure routing/authority/signing/timeout consumers. Phase-1
  480 and check10 do not qualify these later changes or all phase-2 wire fixtures.
  See `/tmp/octet-final/ai-parity.md` and `ai-parity-stage2-plan.md` in that directory.
- **Exact RPC turn cost:** kernel `TurnFinished.turn_cost`, RPC immutable settled
  cost/null propagation and nine TUI test-constructor additions postdate the
  recheck. Require priced/unpriced/reopen and cross-crate consumer tests; see
  `/tmp/octet-final/kernel-turn-cost-plan.md`, `ai-parity.md`, `editor-parity.md`.
- **Extension F2 — ACKed, implementation active:** parent approved the real
  binding/lifecycle protocol for surviving SDK peers, lifecycle rebinding and
  ingress epoch fencing. This supersedes older awaiting-ACK plan wording; no
  runtime closure is claimed. Initial two-peer/product delivery tests do not
  cover the new contract. See `/tmp/octet-final/ext-host-f2-plan.md`, `ext-host.md`
  and the parent's integration update. Runnable-example source repair privately
  stages a current-host pin without changing the published 0.7.6 manifest; its
  Rust rerun is pending, not unchanged-release installation qualification.
- **F8 — P1 export privacy blocker, repaired in source only:** the latest
  `/tmp/octet-final/parity-audit.md` export-visibility appendix independently
  confirms that credential scrubbing did not enforce private extension metadata
  visibility in ordinary JSON/HTML exports. The parent source repair now applies
  the common `session_commands::project_export_visibility` boundary to real
  `Entry.metadata.extension_metadata` before either format's redaction/rendering,
  retaining **only explicit `public:true`** envelopes. Private and omitted-public
  namespaces/values/provenance are excluded even with `--include-secrets`; that
  flag bypasses credential scrubbing only within the export-eligible projection.
  No recursive deletion of similarly named arbitrary tool/result data is added.
  The authored actual CLI matrix covers **JSON/HTML × default/include-secrets**,
  public credentials, explicit/default-private abandoned-branch annotations,
  preserved source bytes/reopen and private output permissions. Parent updated
  `docs/sessions.md`; this refresh does not edit that domain document. **Tests
  are unrun; the P1 blocker is not behaviorally closed by source inspection or
  the earlier coding-library receipt.**
- **Typed-media HTML repair — source only, unrun:** traversal now follows actual
  User/Assistant/ToolResult Media positions, preserving arbitrary `Image`/`Audio`
  metadata instead of erasing or activating it. Two new unit cases are authored.
  This is distinct from F8's shared JSON/HTML visibility projection; neither
  repair is qualified by the failed export-filter build. Complete-content/
  browser goldens and CLI qualification remain pending.

### Current consumers, missing implementations and pending process gates

Owner reports supersede their own older missing-consumer notes only where
explicitly updated. `/tmp/octet-final/editor-parity.md` records the eight product
input-dispatch tests, seven transcript-navigation tests and durable unpriced
telemetry regression passing in coding-lib-02. `/tmp/octet-final/fast-consumer.md`
records actual idle/active search ownership, command provenance, ordered model
scope, `/hotkeys`, `/copy`, `/session`, custom compact instructions, checkpoint
bootstrap and `/fast` consumers. These are no longer merely unused primitives.
The `/fast` durable uncertainty marker and deliberate 272K Codex cap remain;
new tier/budget work is not permission to erase unknown historical exposure.

Kernel session-backed memos/checkpoints, injection and auxiliary retry consumers
exist; F3/F4/F6, durable out-of-order `outcome_ready`, deferred-provider lifecycle
and all-writer lifetime claim/settlement remain separate. Product `open-all`
still has **zero pane effects**. The API 0.3 host bus exists, but F2 is open;
scoped theme selection still has no principal/generation-preserving render
consumer and is not offered. Ownerless standalone migration notification is an
integration gap, not permission to launch extension processes for observation.

**Missing implementation is not a hardware gate:** PiMessages/radius, exact
current catalogs, remaining codec/metadata/image-generation depth, deferred
provider suspend/poll, `/settings`, `/scoped-models`, `/debug`, durable bookmark
navigation, true alternate-screen/final-document transfer, generic nested layout/
scroll/mouse behavior, full Pi host control/UI/tool-result usage and termination,
and native Firefox/Safari adapters remain incomplete. Full RequestOverrides/Azure
and ACKed, actively implemented F2 still require integration/verification. See the
[88-row historical inventory and current overlay](README.md), plus
`/tmp/octet-final/{ai-parity,editor-parity,kernel-parity,ext-host,pi-bridge}.md`.

**Pending current-source process qualification:** `parity_cli` (including actual
HTML export/redaction and ordered model scope), `eval_harness` (six new model
profile process cases plus four preserved cases), `slash_command_pty`,
`activity_wait_pty`, `setup_cli_acceptance`, `pi_install` and `migration_import`.
The coding-library pass does not run those targets. Exact owner scope/commands:
`/tmp/octet-final/model-eval.md`, `fast-consumer.md`, `pi-bridge.md`, and the audit.
The eval profile is a real runtime-backed **explicit local-loopback opt-in** with
private credentials and bounded cases, not a claim of live/paid model quality or
remote-provider authorization. Extension-owner custom compaction and ownerless
configuration/migration notifications remain named integration limits.

**Independent acceptance gates:** physical terminal/native clipboard/platform
behavior, Windows PowerShell, real Pi package/SRI/runtime campaign (historical
0.84.4 support is not additive 8a7b0c03 parity), live providers/billing/MCP, Apple
hardware/signing, multiplexer ownership handover and signed/public release. Policy
exclusions remain unchanged. Existing formatter failures and old platform/PTY/
security receipts below are preserved, not rerun or waived.

### Defects found by using the local build, and their receipts

Both reports came from running the candidate binary, not from the suites, and
each was a real product defect with the data already present in the repository:

| Report | Root cause | Fix + evidence |
| --- | --- | --- |
| DeepSeek V4.1 Flash refused image attachments despite being a vision model | DeepSeek's `GET /models` is sparse; the pinned models.dev record (correctly `text,image`) was restricted to display/pricing, and the id fallback did not match the renamed `deepseek-flash` | Shared discovery now inherits input modalities from the pinned record when the endpoint asserts none. `sparse_inventory_inherits_pinned_image_input_without_overriding_endpoint_assertions` (unit) and `sparse_provider_inventory_inherits_documented_image_input_for_the_real_cli` (real process: `--list-models` reports `1000000 384000 true`, and `deepseek-v4-pro` stays text-only and refuses the image before any request) |
| The same model ran with a 128K window instead of 1M | The pinned record's `limit` was excluded for the same reason, and the V4-family constant only matched `deepseek-v4*` | Same inheritance for context/output limits when the endpoint asserts no limit field; a partially-asserting endpoint keeps independent leaves. Six existing expectations were updated with reasons; `request_output_keeps_estimator_slack_for_a_locally_served_model` and the CLI test above |
| A local vLLM server rejected a turn: *"maximum context length is 131072 … requested 30896 output tokens and your prompt contains at least 100177 input tokens, for a total of at least 131073"* | `resolve_request_max_output_tokens` returned `window − estimate` exactly, and the estimate can be one token below the provider's own count | Bounded headroom (1% of window, clamped 256–4096) is now reserved; `docs/context.md` documents it |
| The same rejection failed the turn outright | A bare numeric `400` (and `413`/`422`) selected the permanent-failure branch, so `looks_like_context_error` returned false and the existing compaction-and-retry path never ran | Those statuses are recoverable *for the context path only*; named policy/auth/quota/rate-limit codes still veto. Unit matrix `request_size_rejections_reach_the_compaction_path_in_every_server_shape` plus the end-to-end `local_server_request_size_rejection_compacts_once_and_retries` (real client, one compaction, one retry) |

Also repaired while verifying: the local-model eval fixture accepted sockets
from a non-blocking listener without forcing blocking mode, so the first read
could return `WouldBlock` and the fixture closed the connection mid-request
("connection closed before message completed"). This was the intermittent
`eval_harness` failure seen repeatedly today (including before these edits) —
a fixture race, not a product defect. Four consecutive green `eval_harness` runs
follow the fix.

**Environment note (not a product claim).** PTY contract targets
(`activity_wait_pty`, `slash_command_pty`, `startup_frame_pty`) pass individually
but have twice wedged when the whole coding suite ran back-to-back: a spawned
`octet` ends in an unkillable exit state, and the test then blocks in
`Child::wait()`. It was observed only after repeated forced kills of earlier PTY
runs in this shell session; each target passes on a clean process table, and no
product code path has been identified. It is recorded as an unresolved
environment/harness interaction rather than a passing or failing claim.

## Historical receipt boundary

**Superseded rows in the historical passes below (do not read as current).**
These rows were accurate for their frozen snapshot and are kept verbatim as
history; the current state is in the section above:

| Historical row | Superseded by |
| --- | --- |
| `T8`, `V9`, `V10` (`/fast` inert, no `set_service_tier` caller, docs saying so) | `/fast on/off/status` now calls `Agent::set_service_tier` through `app/mod.rs:550` and `app/bootstrap.rs:7227`, with tier-aware settlement/reservation; `docs/parity/providers.md` and `CHANGELOG.md` were rewritten accordingly. The durable `responses-priority-tier` uncertainty marker is deliberately retained. |
| `1c.7 Azure` / `1c.9 xAI` "Pending" | `providers/contract.rs` + `protocol` wire tests now cover Azure destination/version overrides and the current-reference xAI Responses route; live acceptance is still not claimed. |
| Any row describing `ls`/`find`/`grep` as missing work | Withdrawn by maintainer decision; behaviour is served by the ripgrep-backed `search` tool. |

Everything below is retained verbatim from earlier review passes. Their dates,
revision identities, failed commands and bounded conclusions are historical;
use the current-candidate section above for the new qualification state.

## Final review — final-audit (post-crash)

**Authoritative final frozen receipt, 2026-09-16 UTC; not a parity or release
approval.** Base `df5a7e809715961b9344af6b52e43a6ca48f56b3` through the shared
worktree on HEAD `e01293917f452da94d603cdf3c43012b8365a627`. **Final workspace check,
libraries and the recorded integration/doc-test receipts are green.** Behavioral
evidence is assembled from completed shards/targets, not a claimed successful
monolithic workspace-test invocation. Earlier failed, ENOSPC and interrupted
runs remain historical. **Pi parity is not achieved:** missing implementations,
policy exclusions and native/live/public gates below remain distinct from tests.
This worker edits only the three assigned parity reports, ran no builds, and
freezes them after this refresh; parent owns the final delivery/PR summaries.

### Findings closed with bounded behavioral evidence

| Original severity / defect | Current disposition and evidence | Remaining limit |
| --- | --- | --- |
| **P1** hidden WebSocket inference replay outside host budgets | **Fixed; agent regressions verified.** `responses_ws::run_generation` no longer resends `response.create`; rejection is fenced/forwarded. `websocket_connection_limit_is_retried_by_agent` and `qualified_codex_ws_http_cumulative_twelve_attempt_envelope` passed in `agent-integrations-final.log`; all **372 AI library tests** also passed. | `ai-confirmed.log` now passes **464 tests**, including the exact two physical requests (one WebSocket plus one explicit host HTTP request). Host budget/accounting assertions were not relaxed; live billing is not inferred. |
| **P1** ephemeral RPC accounting retained only newest session | **Fixed; unit and process verified.** `session_store.rs::collect_ephemeral_accounting` combines all invocation-owned sessions; its regression passed in `coding-lib-final-receipt.log`, and `no_session_rpc_preserves_both_sessions_accounting_before_discarding_transcripts` passed in `cli-critical.log`. | The two-session RPC/EOF fixture seeds durable usage while sessions are idle; it does not make a live inference request. |
| **P1** accounting append failure destroyed recovery | **Fixed; units verified.** Private accounting-only staging, retained pending state and locked idempotent/torn-tail ledger repair; `ephemeral_append_failure_keeps_private_accounting_only_and_retries_once` and `ephemeral_accounting_retry_repairs_a_torn_append` passed. | Total failure to write even the recovery snapshot permits only in-process retry; no automatic recovery CLI or impossible storage guarantee is claimed. |
| **P2** optional theme service advertised without handler | **Fixed; real-process verified.** Product offer removes `theme_selection`/methods; `unimplemented_theme_selection_is_not_offered_and_returns_a_canonical_refusal` passed in `agent-integrations-final.log` (exact `-32601 / unknown or unnegotiated method`, healthy shutdown). | **Theme selection itself remains unavailable** until a host-owned catalog/namespace-bound handler exists. Generated helper tests are not feature implementation. |
| **P2** Codex override exceeded authenticated maximum | **Fixed; unit verified.** Resolver intersects discovered and static ceilings; `live_discovery_bounds_acknowledged_overrides_below_the_family_table` passed: 500K refused against live 400K, acknowledged 400K allowed, ordinary 272K cap unchanged. | No live provider entitlement/billing qualification follows from this deterministic unit test. |

Fresh source inspection and passing-test names above supersede the initial open
findings, not their historical diagnosis. See [host-boundary report](REVIEW-host-boundary.md),
[bootstrap review](REVIEW-bootstrap.md), [agent review](REVIEW-agent.md) and
[AI review](REVIEW-ai.md).

### Remaining implementation/release gates and verified repairs

- **High-priority ownership containment:** product `open-all` now marks **all**
  parent/worker panes unresolvable in source (`launcher.py:279–332,384`). The host
  lacks atomic writer claim/settlement; fresh launchability, opaque handles,
  per-append locking or documented races cannot substitute. This is **disabled
  functionality**, not qualified pane handover. The extension owner now reports
  **88 tests passed**, including repeated tmux/herdr requests with fresh launchable
  snapshots producing zero product pane effects. This supersedes the earlier 84-test
  result and verifies refusal, not ownership transfer.
- **Release packaging blocker:** parent fixed importer entrypoint Git modes to
  `100755` (independently inspected). Separately,
  `scripts/package-octet-extension-release.sh:119–120` still requires API **0.2**,
  rejecting API 0.3 despite runtime bundle support. Runtime, manifest and mode
  tests do not qualify this release packager.
- **Agent fixes verified individually:** cumulative delegated snapshots against
  committed root usage, uncertainty-only records, exact picodollar borrowing,
  durable spawn principal/owner/message/policy identity, and secure bounded
  exclusive sidecar I/O all passed their unit tests. Partial republish integration
  passed. Journals are recovery observations, not authoritative usage/effect
  outcomes or exactly-once frontend delivery. Search startup/paused-clock fixture
  repairs now pass in `agent-lib-confirmed.log`: **556 passed / 0 failed / 1 ignored**.
- **Coding library and UI repairs verified:** `coding-lib-final-receipt.log`
  completed **1569 passed / 0 failed / 1 ignored**, all features, four test threads.
  This includes prior TUI/bootstrap/host-Serve failures, goal commands during a
  live run, shimmer neutral/rest behavior, footer uncertainty persistence,
  completed-worker no-replay and silent-startup composer tests. The slash-command
  PTY suite passed **8 tests**, including `/goal` against a held response stream.
  No-color clipping, off-tail anchoring and stable-picker repairs are exercised;
  this does not promote unrelated Unverified parity rows or physical appearance.
- **MCP credential boundary:** exact enabled static `(server, variable)` binding
  prevents the mixed-server broker reference from resolving another server's
  static token. Extension-owner suite: **76 passed**, including zero broker
  endpoint requests. Herdr preflight/partial-failure reporting, Pi offer drift and
  vendored inert SDK synchronization have owner test receipts. None introduces
  host OAuth, bus authority or atomic writer handover.

### Final verification evidence

All named logs are under `/tmp/octet-final`. Completed logs/exit files were
inspected here; parent/owner-reported ancillary checks are marked. A passing
target inside an interrupted command is evidence for that target, **not** a
successful command. Overlapping earlier executions are not added to totals.

| Check | Final observed result / receipt | Scope |
| --- | --- | --- |
| Workspace check | `workspace-check-final.log` + `.exit = 0`, **42.2s** | `cargo check --workspace --all-targets --all-features --locked`; compilation, not execution. |
| Coding-agent library | `coding-lib-final-receipt.log` + `.exit = 0`: **1569 passed / 0 failed / 1 ignored**, **32.60s** test time | `--all-features --lib --locked -- --test-threads=4`; supersedes prior library failures/interruption. |
| Agent library | `agent-lib-confirmed.log` + `.exit = 0`: **556 passed / 0 failed / 1 ignored** | `--lib --locked -- --test-threads=4`; search fixture repairs verified. |
| Agent integrations, all features | `agent-integrations-final.log` + `.exit = 0`: **230 passed / 0 failed / 1 ignored**, **48.2s** | Fresh clean command: `--all-features --test '*' --locked --no-fail-fast`. Includes **146 agent_run**, runnable API **2**, conformance **4 + 5**, theme policy **13**, telemetry **9**. Supersedes the integration receipt inside the earlier failed combined command; counts are not added twice. |
| AI all targets/features | `ai-confirmed.log` + `.exit = 0`: **464 passed / 0 failed**, including **372 library + 38 client_stream** | Exact physical sends, heartbeat fallback and computer-call regressions pass. |
| Coding-agent integrations | **All 26 targets have passing receipts; 169 distinct tests passed.** Target inventory checked against all 26 `tests/*.rs` files. | Detailed shard mapping below; PTY targets now have their own clean final command receipt. |
| Renderer/migration all targets/features | `renderer-migration-final.log` + `.exit = 0`: **294 passed / 0 failed**, **70.3s** | `sexy-tui-rs` and `octet-migrate-types`, all targets/all features; not an upstream oracle rerun. |
| Binaries/examples smoke | `binaries-examples-final.log` + `.exit = 0`: **3 targets, 0 tests**, **18.1s** | `octet`, `octet-host`, and agent `session_bench`, all features. Compile/harness smoke only; **adds zero behavioral tests**. |
| Workspace doc tests | `doctests-final.log` + `.exit = 0`: **6 passed / 0 failed across 5 crates** | `cargo test --workspace --all-features --doc --locked --no-fail-fast`. |
| Independent excluded Serve workspace | `serve-final.log` + `.exit = 0`: **284 passed / 0 failed**, **10.0s** | Separate `--manifest-path extensions/octet-serve/Cargo.toml --locked --no-fail-fast -- --test-threads=4`; distinct from coding-agent host-Serve units, which now also pass. Not a full security audit. |
| Full web checks | `web-test.log`: **35 files / 299 tests passed**; parent reports lint/typecheck/build/fonts/external/bundle-check all passed; corresponding logs inspected | Supersedes the earlier 18-test subset. No physical-browser/native automation claim. |
| Cargo audit | `audit-workspace.log`, `audit-serve.log`; parent reports both `--no-fetch` checks **passed** | Cached advisory DB. Allowed unmaintained warnings: workspace **bincode + ttf-parser**, Serve **ttf-parser**. Not warning-free/security approval. |
| Cargo deny | `deny-workspace.log`, `deny-serve.log`: **advisories/bans/licenses/sources ok**; parent reports both offline checks passed | Warnings retained; no network refresh or absence-of-all-risk claim. |
| Extension/SDK owner suites | [REVIEW-extensions](REVIEW-extensions.md): Subagents **88**, MCP **76**, computer use **93**, shared Python SDK **101**, Browse **80 passed / 2 skipped**, importers **11 / 8 / 4**, Pi **71 passed / 3 skipped**, Node UI **17**, TS SDK **39 fixtures** | Bounded local/mock evidence. Final Subagents tests verify repeated zero-effect open-all refusal; no atomic handover or real tmux/native/remote/Pi campaign implied. |
| Repository scripts | `scripts-unittest-final.log` + `.exit = 0`: **51 passed** | Archive-generated-content and explicit benchmark review-gate regressions included. |
| Packaged docs/resources | `packaged-docs.log` + `.exit = 0`: **443 public files, 43 extra references**; parent reports later resource consistency green before final textual edits | Latest documentation changes are not retroactively covered by the earlier packaged-doc receipt; this worker checks owned links/whitespace separately. |
| Binary installer | `binary-installer.exit = 0`; parent reports full offline harness **11 UI + 18 version + 3 cleanup passed** | No public installation/publication claim. |
| Catalog diff / generated API | Parent reports **10 passed** / generator `--check` passed | Contract generation is not feature execution. |
| Formatting | `fmt-final.log` + `.exit = 1`: **90 unique files / 683 hunks**, independently counted from diff headers | Unchanged-baseline drift and changed files both involved; no blanket attribution or global format. Supersedes the earlier 88-file/663-hunk receipt. |

#### Coding integration target receipts

| Receipt | Passing target results counted | Command disposition |
| --- | --- | --- |
| `cli-critical.log` | **37**: Codex context 14, parity CLI 15, setup CLI 8 (including real two-session ephemeral RPC accounting) | Exit **0**. |
| `tui-pty-final.log` | **37**, five targets: activity 2, late-terminal 6, setup TUI 4, SIGTERM 17, slash commands 8 (including held-stream `/goal`) | Exit **0**, **42.2s**. Replaces the completed-target evidence from interrupted `tui-pty-confirmed`; no double counting. |
| `startup-pty-short.log` | **14** startup tests; matrix test explicitly filtered | Exit **0**. |
| `startup-redraw-matrix.log` | Remaining **1** startup test: **30 composed-screen cases**, **92.09s** | Exit **0**; completes the same startup target without double-counting short-run tests. |
| `coding-integrations-providers.log` | **43** across six passing provider/frontend targets | Exit **101** from separate provider_contract stale expected **31** versus actual **36**; that failed target is not counted here. |
| `coding-integrations-host.log` | **19** across six targets, including corrected provider_contract **1** | Exit **0**; provider contract now passes with count 36. |
| `coding-integrations-migration.log` | **18** across five targets: delegated resume, eval harness, host migration, import, Pi install | Exit **0**; scripted eval fixtures do not implement real model-backed evaluation. |

#### Historical failures/interruption, not the current verdict

- `test-workspace.log` failed with ENOSPC. Recovery `build-lowdisk` exit 0 was
  `--no-run` compilation only. No successful monolithic workspace-test execution
  is claimed; the completed final shards above are the behavioral evidence.
- `agent-tests-final` library **554/2/1**, `coding-lib-final` **1551/15/1** and
  `ai-final` **463/1** retain their original failed results. Final receipts above
  supersede them; they are not current failed-library claims or extra test totals.
- Parent identified two intermittent extension timing failures in interrupted
  `coding-lib-green`, alongside a **test-only** deadlock introduced by temporary
  MutexGuards in an array expression. Parent repaired the fixture deadlock; it is
  not a production deadlock finding. The complete final library receipt is green.
- `tui-pty-confirmed` remains an interrupted command, not a pass. Its five targets
  now pass again in clean `tui-pty-final`; startup passes in the two dedicated
  receipts above. The old provider-contract command still exited 101, with its
  failed target subsequently verified by `coding-integrations-host`. Likewise,
  `agent-integrations-final` now provides a clean all-feature integration command
  receipt rather than relying on completed targets from `agent-tests-final`.
- Earlier `tests-renderer-ai` **741 passed** and this worker's **14 Python theme
  helper tests** remain historical/diagnostic, not additional final counts. The
  real-process canonical theme refusal is now the product-boundary evidence.

Parent's disk cleanup and removal of tracked Swift artifacts are repository
hygiene, not source validation. Native/physical/public acceptance and exhaustive
Serve/companion security review remain separate. No Git mutation by this worker.

### Ledger reconciliation against actual consumers

The shared README distinguishes verified behaviors from missing primitives.
“Verified” is bounded by the named receipt, not by unrelated suite outcomes.
“Implemented” still means source-inspected with its own verification outstanding.

| Rows | Current source fact; obsolete claim removed |
| --- | --- |
| `1b.1`, `1b.2`, `1b.5` | `RequestOverrides` and `ModelPreset` remain declared data (`octet-ai/src/declarations/mod.rs:272–328,399–417`). The actual `Request` / `ModelSpec` and client wire path (`types.rs:727,1315`; `client.rs:2064,2097`) do not consume these values. “Landed” becomes **Partial**, not runtime support. |
| `1b.4` | Conditional inventory is **verified**: all five `provider_conditional_inventory_tests` passed in `coding-lib-final-receipt.log` (200/304, validator reset/scope/bounds, error preservation). Old “Pending” is stale. |
| `1c.1`, `1c.8`, `1c.10` | Explicit strict/grammar declarations emit, but custom-call decoding/history/result replay is missing (AI owner confirmation; Responses stream dispatch only handles function/computer calls), unspecified tools do not default to strict (`constrained_sampling.rs:122–130`), and per-model compatibility is missing. Mistral reasoning remains rejected (`catalog.rs:369`). `Response.diagnostics` already exists (`types.rs:1578`); raw/model/thinking and per-tool usage fields do not. Grouped rows are not fully landed. |
| `1e.2` | `agent.rs:7007` consumes a bounded journal and republishes partial text/reasoning; the encoder/journal are created in the stream (`:7654`), and `agent_run.rs::a_killed_stream_republishes_its_partial_assistant_prefix_once` exists. The republish integration **passed** in `agent-integrations-final.log`; secure/bounded/torn-tail/replacement-safe journal units **passed** in `agent-lib-confirmed.log`. Not atomic frontend delivery. |
| `2b.1`–`2b.5` | Reusable editor actions, JSON binding manager and prompt-zone parser exist, but `tui/keymap.rs:252–469` does not dispatch the new undo/redo, yank/pop, word/line/jump actions or load the user manager. `view/input_overlays.rs:500–509` explicitly documents the unresolved live binding. Primitive tests do not qualify product key behavior. Prompt zones still need viewport indexing/jump dispatch. |
| `3.5` | Agent/delegation span consumers and behavioral tests exist (`agent_run.rs:9444,9546,9791`; `delegation.rs` DelegationSpan). `typed_spans_nest_run_turn_provider_and_tool_boundaries`, `typed_spans_cover_compaction_and_summary_boundaries`, failed-run and delegation-span tests **passed**. Old consumer/test-pending labels are stale. |
| `4.8`, `4.10`, `4.13` | Real consumers now exist: `LivePreviewPacer` in tool execution; unanimous batch check after durable results (`agent.rs:9116`); opt-in prompt assembly (`set_tool_prompt_section_enabled`, `model_visible_system`). Tests are `live_panel_decorations_are_coalesced_and_settle_the_latest_state`, `unanimous_tool_termination_ends_the_run_and_a_lone_request_does_not`, `tool_prompt_section_is_opt_in_visible_and_never_names_withdrawn_tools`. All three named tests **passed** in `agent-integrations-final.log`; rows are **Verified**, without inventing coding-product default enablement. |
| `4.7`, `4.11`, `4.12`, `4.14` | Checkpoint publication has an opt-in agent consumer, but no product durable sink. `DurableInvocationStore` is memory-backed; deferred poll/suspend is a decision core; the summary-retry helper is not called by `CompactionContext::compact_boundary` (`agent.rs:5431` still calls `self.summarize`). These remain **Partial**. |
| `4.6` | Windows runtime opt-in exists, but `config.rs:119–128` has no coding-product PowerShell allowlist entry. This is a missing product primitive **in addition to** Windows evidence, not hardware-only. |
| `5.4` | All-session teardown and append-failure/torn-write recovery are **verified**, including the real-binary two-session RPC/EOF regression in `cli-critical.log`. No stale backend/failure/process-test-pending claim remains. |
| `5.8` | `modes/export_html.rs:47–63` safely escapes pretty-printed JSON into `<pre>` plus bounded raster previews. It does not render Markdown, syntax highlighting or ANSI styles. The so-called golden test compares two calls of the same implementation, not an external expected artifact. Safe projection is not the full requested renderer. |
| `5.11` | `cli/eval.rs:67–79,269,317–335` measures scripted loopback replies. It explicitly cannot select a real model/backend. The fixture harness exists; model-backed evaluation remains a separate missing primitive, not merely a missing live run. |

Other unverified TUI inventory rows remain **Unverified** rather than being
promoted by neighboring tests. This review does not establish all 88 rows.

### Exact missing implementation versus deliberate exclusions and evidence gates

**Missing implementation (not hardware excuses):** PiMessages/radius request and
SSE codec, protocol/declaration/discovery integration; pinned static catalogs for
the new provider declarations; request-hook/override and model preset merges;
proxy-resolver-to-transport integration; custom grammar-call decode/replay and
strict-prefer defaults/model compatibility; Anthropic model-compat/beta/fallback
pricing; Bedrock model/profile-ARN region plumbing; per-request Codex transport
modes/debug statistics; Azure deployment map/overrides; Mistral reasoning profile
and emit path; xAI Responses declaration (shared encrypted replay exists);
response metadata/per-tool usage; faux/deferred provider fetch and durable
suspended-run leaves/permits; image-generation API/OpenRouter adapter/catalog
and generator; product editor binding/viewport integrations; session-backed
keyed invocation memos/checkpoints and summary-retry integration; real
model-backed evaluation; full rich HTML export; host `bus/*` mediation;
principal-bound `theme/select` handler; configuration/migration PostMutation
emitters; cross-process delegated writer claim/settlement and mailbox recovery
(product pane execution blocked); API 0.3 release-packager admission.

**Deliberate exclusions / authority decisions:** persisted project-trust changes;
new host-brokered OAuth/credential-policy/store/flow expansion; clipboard **image**
capture; rg/fd auto-download; chord/CBOR/Unix-socket architecture; dedicated
ls/find/grep tools withdrawn by maintainer (bash + rg are not identical built-in
APIs). Codex's default context cap is intentional. `store: true` would change
provider retention/privacy and is **not** an automatic fix for missing live
cursor resumption. Computer-use execution needs separately authorized host
approval/target/settlement services, not fabricated tool authority.

**Evidence/hardware/release gates:** real Windows PowerShell/native UIA; native
macOS accessibility and human-observed takeover/process-loss release; physical
terminal/SSH color, input, scrollback and Browse focus; live remote MCP and live
provider acceptance (including any xAI wire switch); real tmux/herdr ownership
handover; signed/public installation and release assets. Native Firefox/Safari
operation additionally lacks an implementation—it is not simply awaiting a
hardware run. SDK/process fixtures and mock backends cannot close these gates.

`ROADMAP.md` remains selected outcomes rather than a promise to finish BACKLOG.
BACKLOG's captured run-scoped worker and inherited-only model descriptions are
historical and should not be used as current code evidence. No board/issue closure,
remote publication, full Serve/companion security qualification, unchanged-Pi
extension compatibility, or Pi-parity completion follows from this local pass.

---

> **Current authority: [Final review — final-audit (post-crash)](#final-review--final-audit-post-crash).**
> Passes 1–4 below are preserved historical snapshots, not current release verdicts.
> **Historical Pass 4 introduction:**
> Pass 4 was taken at committed HEAD `03c4c31c` with a *moving* worktree (workers
> editing `agent.rs`, `auth.rs`, `view.rs`, `view/tests.rs`, `interactive.rs`,
> `bootstrap.rs`, `markdown.rs` while it ran). It overturns Pass 3 on three
> points: `/goal` mid-run **is** landed, the `rich_fences` oracle is now 15 tests,
> and the "uncommitted wave" Pass 3 measured is **committed** as `03c4c31c`.
> Pass 4 also finds two red test clusters Pass 3 had not seen (25 then 10
> failures) and one stale CHANGELOG Known-gap.
>
> **Pass 3 (this section) is superseded.** Pass 3 was taken on
> 2026-09-15 from 18:29Z by verifier `verify12b`, at *committed* HEAD
> `df2e8980` with the **working tree dirty**. Every claim below states whether
> it was checked against the **worktree** (what a PR would ship) or against
> **committed HEAD** — the commit predates most of this wave, so a claim about
> "HEAD" is very often *not* a claim about this tree. Pass 2 sent below is kept
> verbatim; where Pass 3 re-ran or overturns a Pass-2 item the disposition is
> stated in the Pass-3 text.

## Pass 3 — overall status (verify12b, 18:29Z+, dirty worktree on `df2e8980`)

Tool surface, LaTeX/Mermaid oracles and the fence consumer are **verified good**.
The `/fast` headline in `docs/parity/providers.md` is now **honest** (it was
rewritten by another worker during this wave); the intervening code still shows
that **no live run selects a service tier** and `/fast` is inert. Two TUI items
in the wave are **landed in the worktree but not in the file the claim names**,
one (`/goal` mid-run) is **not landed at all**, and the predecessor's single RED
(`rich_fences`) is now **green** — see the verdict table.

> Status of this section: written incrementally, section by section, while the
> test battery still ran. Any row marked PENDING below was re-checked at the
> end; rows that remain PENDING are named in "what I could not verify".

## Pass 3 — verdict table

Everything was observed by me on this host. "WT" = working tree (dirty);
"HEAD" = committed `df2e8980` content read from the file as it exists on disk
(which is WT unless the file is in `git status`).

| # | Area | Claim under test | Observation | Command run | Verdict |
| --- | --- | --- | --- | --- | --- |
| P1 | Tool surface (WT) | Exactly the maintainer's narrow set, `rg` by default | `CoreTools::register` has 6 `host.tool(...)` calls: ReadTool, EditTool, WriteTool, BashTool, SearchTool, and PowerShellTool gated `#[cfg(windows)]`. Nothing else registered. Not re-added under another name: the only `impl Tool for` in `crates/octet-agent/src/tools/` are `ReadTool EditTool WriteTool BashTool SearchTool PowerShellTool` plus the internal `CheckpointedBashTool` and `SessionShellTool` (not new model-visible names). | `rg -n "host\.tool\(" crates/octet-agent/src/tools/mod.rs`; `rg -n "impl Tool for" crates/octet-agent/src/tools/` | VERIFIED |
| P2 | Tool surface (WT) | `ls.rs`/`find.rs`/`grep.rs` deleted | Directory listing is `bash.rs deferred.rs durability.rs edit.rs mod.rs powershell.rs read.rs search.rs shell_environment.rs summarization.rs write.rs` — no `ls.rs`, `find.rs`, `grep.rs` | `ls crates/octet-agent/src/tools/` | VERIFIED |
| P3 | Tool surface (WT) | No dangling `LsTool`/`FindTool`/`GrepTool` reference | Zero hits outside prose in `docs/swarm-audit/EXECUTION-*.md` (4 files) and this file itself. `rg -n "LsTool\|FindTool\|GrepTool" crates/ scripts/ Cargo.toml` → **no output**. (Pass 2's own table printed mangled output because it used `rg -rn`, where `-r` is the *replace* flag, not "recursive"; the conclusion was right, the command was malformed.) | `rg -n "LsTool\|FindTool\|GrepTool" --glob '!docs/**'` | VERIFIED |
| P4 | Tool surface (WT) | `search` still shells out to ripgrep | `SearchTool::execute` → `self.execute_with_program(args, ctx, Path::new("rg"))` (`search.rs:225`); the binary is invoked by argv, no shell string | `rg -n 'Path::new\("rg"\)' crates/octet-agent/src/tools/search.rs` | VERIFIED |
| P5 | Fence consumer (WT) | LaTeX/Mermaid fences have a UI consumer and an unknown fence stays literal | `rich_text/markdown.rs` `render_diagram_fence` dispatches latex/mermaid/graph/flowchart; `tests/rich_fences.rs` now has **10** tests and I ran them: `test result: ok. 10 passed; 0 failed` — including `unknown_fences_are_never_reinterpreted`, `unsupported_bodies_degrade_to_the_original_source`, `oversized_and_unterminated_fences_stay_literal`. Pass 2's RED (`streaming_keeps_failed_diagram_fences_as_source`, 9/1) is **fixed in the worktree**. | `cargo test -p sexy-tui-rs --test latex_render --test mermaid_render --test rich_fences` | VERIFIED (WT) |
| P6 | LaTeX oracle (WT) | `--test latex_render` = 17 | `17 passed; 0 failed` (my own run, 18:29Z) | same command | VERIFIED |
| P7 | Mermaid oracle (WT) | `--test mermaid_render` = 11 | `11 passed; 0 failed` (my own run) | same command | VERIFIED |
| P8 | `/goal` mid-run (`interactive.rs`) | The reported bug (goal queued, applied only at the idle boundary) is fixed | **NOT fixed in the worktree.** `Command::Goal(goal) => PendingIdleAction::Goal(goal)` is still in `queue_command` (`interactive.rs:682`), the active-run dispatcher still falls through `command => match queue_command(command, queue)` with "command queued for the next idle boundary" (`:1959`), and the action is still applied only in the idle loop (`:4409`). `/goal` is **not** in the active-run arms (the arms immediately above handle `Changelog`, `Fast`, `Extensions`, `Model`, `Thinking`, …). | `rg -n "queue_command\|Command::Goal" crates/octet-coding-agent/src/modes/interactive.rs`; `sed -n '1955,1962p;4405,4412p'` | CONTRADICTED (not landed) |
| P9 | Footer/telemetry cost (`tui13b`) | Plain dollar estimate, no `subtotal`/`+`/`~` | **Half landed.** The telemetry panel (`tui/view/status_telemetry.rs`, dirty, +311 lines) renders `Turn cost $0.078900` / `Session cost $2.410000` via `status_dollars`, with tests `cost_lines_are_plain_dollars_even_when_usage_is_uncertain` and `honest_absence_cost_paths_survive_plain_dollar_rendering`. But the **composer footer is untouched** and still prints `format!("subtotal {} + ?", format_microdollars(cost))` at `tui/composer_surface.rs:804` (`composer_surface.rs` is not in `git status`). Pass 2's U2 was right about the composer and missed the landed panel. | `rg -n "subtotal" crates/octet-coding-agent/src/tui/`; `sed -n '28,82p' crates/octet-coding-agent/src/tui/view/status_telemetry.rs` | PARTIAL / CONTRADICTED as stated |
| P10 | Subagents activity (`tui14b`) | Settles into transcript, not replayed on every prompt | **Worktree only.** `settled_subagent_workers` (`tui/view.rs:1661`), `subagent_worker_ids` (`:832`), the settle/ignore path (`:2199-2251`), the deliberate non-clear (`:3415`) and the clear on session replacement (`:6534`), plus test `a_settled_subagent_roster_never_replays_under_a_later_prompt` (`tui/view/tests.rs:10748`). Code present; **test result pending** in my run at the time of writing. | `rg -n "settled_subagent_workers" crates/octet-coding-agent/src/tui/view.rs` | PARTIAL (code VERIFIED, green run PENDING) |
| P11 | Shimmer (`tui12b/13b`) | Model-adaptive, no rainbow outside max/ultra, real rest gap | `activity_shimmer_palette` derives both colours from the theme's model identity (`tui/view/reasoning_render.rs:474`); the rainbow strength is produced only by `status_rainbow_strength_at` (`tui/view.rs:1798`) and the test at `reasoning_render.rs:1459-1470` asserts it is 0 for every level except `max`/`ultra` (and 0 at zero elapsed even for `max`); `ACTIVITY_SWEEP_REST_FRAMES = 2` (`:52`) with a test asserting `rest_frames >= ACTIVITY_SWEEP_REST_FRAMES` (`:1633`) | `rg -n "ACTIVITY_SWEEP_REST_FRAMES\|status_rainbow_strength_at" crates/octet-coding-agent/src/tui/view{,/reasoning_render}.rs` | VERIFIED (code + test present; green run below) |

## Pass 3 — test surface (my own runs) and the live compile blocker

| Command (all mine, dirty worktree) | Observed | Verdict |
| --- | --- | --- |
| `cargo test -p sexy-tui-rs --test latex_render --test mermaid_render --test rich_fences` (18:29:49Z) | `latex_render 17 passed`, `mermaid_render 11 passed`, `rich_fences 10 passed`; 0 failed | VERIFIED |
| `cargo test -p octet-ai --no-fail-fast` (18:31Z) | **13/13 targets ok**: 366, 5, 3, 18, 1, 38, 3, 2, 16, 4, 1, 1 + 1 doc-test; 0 failures, no hang. Whole command 41s. | VERIFIED |
| `cargo test -p octet-ai --lib -- --exact responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` (18:31Z) | `ok. 1 passed … finished in 1.76s` — the test that previously **hung** still terminates | VERIFIED |
| `cargo test -p octet-coding-agent --lib` (18:32:12Z) | **`error: could not compile octet-agent (lib) due to 5 previous errors`** — see below. Nothing ran. | BLOCKED |
| `cargo test -p octet-agent …` (18:30:26Z start, killed by worker exit) | no result captured | NOT RUN |

### Pass 3 blocker B1 — `octet-agent` lib had 5 compile errors at 18:32Z

```
error[E0603]: module `bash` is private
  --> crates/octet-agent/src/agent.rs:70:19   (use crate::tools::bash::{ … }; tools/mod.rs:24 `mod bash;`)
error[E0603]: module `bash` is private     --> crates/octet-agent/src/agent.rs:70:19
error[E0603]: module `bash` is private     --> crates/octet-agent/src/agent.rs:70:19
error[E0277]: the size for values of type `str` cannot be known at compilation time
  --> crates/octet-agent/src/agent.rs:4443:13
error[E0308]: mismatched types   --> crates/octet-agent/src/agent.rs:4450:22
error: could not compile `octet-agent` (lib) due to 5 previous errors
```

`crates/octet-agent/src/agent.rs` was modified at 18:31:56Z, 16 seconds before
this observation, and is `+684` lines in the worktree, so the most likely
reading is a **mid-edit snapshot** rather than a defect that would ship. It is
still recorded because it directly contradicts the pass-2 claim "`cargo check
--workspace --all-targets`: 0 errors (18:21Z)" and because *no* `octet-agent` or
`octet-coding-agent` test can run while it holds. Re-checked later in this pass;
the outcome is recorded in "what I could not verify" below.

### Pass 3 startup-latency measurement (re-measured, 18:31Z)

| Binary | plain | `AWS_EC2_METADATA_DISABLED=true` |
| --- | --- | --- |
| `~/.local/bin/octet` (24,143,088 B, sha256 `96b138b0…`, Sep 12 14:54) | 1.753s / 1.800s | 0.029s / 0.029s |
| `target/debug/octet` (143,888,360 B, sha256 `df552ab6…`, Sep 15 14:17) | 0.446s / 0.446s | 0.032s / 0.033s |

Both print `octet 0.7.6`. Both files are **byte-identical to the bytes pass 2
measured** (same sizes, same sha256) — no rebuild happened between the two
passes, so the artifact is still older than the fix. `strings -a … | grep -c
OCTET_AWS_METADATA_CREDENTIALS` → installed **0**, checkout **1**: the
investigation's warning stands, the version string is not proof of identity.
Conclusion unchanged from pass 2: the AWS metadata penalty is real and large
(~1.7s here) in the shipped binary, the checkout artifact still pays ~0.42s, and
**the fixed behaviour is not demonstrated by any binary on this disk**.

The mechanism is re-verified in the worktree: `MAX_AWS_METADATA_BYTES = 64*1024`
(`providers/auth.rs:262`), `AWS_METADATA_TIMEOUT = 1s` (`:263`),
`enum AwsMetadataActivation` (`:671`), `fn aws_metadata_activation_from`
(`:716`), `fn aws_metadata_activation_inputs` (`:781`, with the injectable
`aws_metadata_activation_inputs_with` at `:789`). The acceptance tests count
requests with `Arc<AtomicUsize>` (`:1238, :1404, :1457-1458, :1584, :1636,
:1810, :1840`) and do not assert milliseconds; `unrelated_provider_launch_makes_zero_aws_metadata_requests` (`:1447`),
`disabled_activation_opens_no_connection_to_a_live_metadata_endpoint` (`:1490`),
`opt_in_activation_resolves_and_signs_with_live_metadata_credentials` (`:1539`),
`static_keys_and_a_plain_profile_never_activate_the_metadata_probe` (`:1610`),
`indicated_metadata_probe_prefers_the_container_uri` (`:1836`),
`an_unavailable_metadata_endpoint_costs_one_bounded_request` (`:1895`),
`aws_metadata_service_endpoint_override_is_validated_fail_closed` (`:1906`).
The cross-process Codex refresh lock is bounded by
`REFRESH_LOCK_WAIT = 3s` (`auth/codex/store.rs:25`), consumed by
`run_route_readiness` under `CODEX_READINESS_ENVELOPE = 10s`
(`app/bootstrap.rs:5063`); the contended-lock tests are
`refresh_lock_waits_on_the_private_credential_directory` (`store.rs:669`, which
*does* use 50ms/1s wall-clock probes for the contended case) and
`a_contended_refresh_lock_fails_closed_and_leaves_no_phantom_holder`
(`:1037`). "A fresh valid Codex cache triggers zero inventory discovery" is the
committed `offline_codex_registration_uses_cached_inventory_without_dynamic_capabilities`
(`app/bootstrap/tests.rs:1281`) — **code read only; its result is inside the
blocked `octet-coding-agent --lib` run**.


## Pass 2 — overall status at HEAD `df2e8980` (historical)

> **Pass 2 was the then-current truth.** It was taken at HEAD
> `df2e8980` on 2026-09-15 (UTC times in the text). Everything from here down to
> `## Pass 1 record (HEAD 7be2dc96, retained for history)` is this verifier's
> own pass-2 run. The material under that heading is the earlier pass, kept
> verbatim; where it is now stale it is annotated in the pass-2 dispositions.

## Pass 2 — overall status at HEAD `df2e8980`

- Branch `vibe/pi-parity-roadmap-df5a7e80`, base `df5a7e80` (v0.7.6).
  Working tree dirty with other workers mid-edit for the whole pass
  (`crates/octet-coding-agent/src/providers/auth.rs`, `tui/view.rs`,
  `tui/view/reasoning_render.rs`, `crates/sexy-tui-rs/src/rich_text/latex/mod.rs`).
  Every claim below is stamped with the time it was observed; a file being
  edited by another worker is stated as such rather than treated as settled.
- `cargo check --workspace --all-targets --locked`: **0 errors** (observed 18:21Z,
  `Finished` in 3.46s; 89 `is never used` warnings).
- `cargo test -p octet-ai --no-fail-fast`: **all 13 targets green, no hang**
  (366 lib + 5/3/18/1/38/3/2/16/4/1/1/1; the previously hanging
  `responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` now
  completes in 1.76s).  The pass-1 C7/C8 blockers are resolved.
- `sexy-tui-rs`: `latex_render` **17 passed**, `mermaid_render` **11 passed**.
- The tool surface is exactly the maintainer's four tools plus `search`
  (`read edit write bash search`), `ls`/`find`/`grep` are gone with no dangling
  reference, and the fence consumer for LaTeX/Mermaid has landed.
- **The one live contradiction is the `/fast` headline in
  `docs/parity/providers.md:40`** (C10 below): the octet-agent plumbing is real
  and tested, but the user-facing command still refuses to act and nothing in
  `octet-coding-agent` calls the new setter. Two further documents
  (`CHANGELOG.md`, `modes/interactive.rs` doc comment) describe the *pre-fix*
  state and are now stale in the opposite direction.

## Pass 2 — verdict table

All commands were run by this verifier on this host. "UTC" is the observation
time. Commands were run with `--locked` unless stated.

| # | Area | Claim under test | Observation | Command run | Verdict |
| --- | --- | --- | --- | --- | --- |
| T1 | Tool surface | Registered `CoreTools` surface is exactly the maintainer's narrow set | `host.tool(ReadTool)`, `EditTool`, `WriteTool`, `BashTool`, `SearchTool`, and `PowerShellTool` behind `#[cfg(windows)]` — 6 call sites, nothing else | `rg -n "host\.tool\(" crates/octet-agent/src/tools/mod.rs` | VERIFIED |
| T2 | Tool surface | `ls.rs`/`find.rs`/`grep.rs` are deleted | `ls crates/octet-agent/src/tools/` = `bash.rs deferred.rs durability.rs edit.rs mod.rs powershell.rs read.rs search.rs shell_environment.rs summarization.rs write.rs` | `ls crates/octet-agent/src/tools/` | VERIFIED |
| T3 | Tool surface | No dangling reference to the removed tools | `rg -n "LsTool\|FindTool\|GrepTool"` matches **only prose inside `docs/swarm-audit/EXECUTION-*.md`**; zero hits in any `.rs`, `.md` product doc, or build file | `rg -n "LsTool\|FindTool\|GrepTool"` | VERIFIED |
| T4 | Tool surface | `search` still shells out to ripgrep | `SearchTool::execute` → `self.execute_with_program(args, ctx, Path::new("rg"))`; failure text "search is unavailable: ripgrep (rg) was not found on PATH" | `rg -n "execute_with_program" crates/octet-agent/src/tools/search.rs` | VERIFIED |
| T5 | Tool surface | No equivalent tool re-added under another name | The only `impl Tool` in `crates/octet-agent/src/tools/*` are `ReadTool`, `EditTool`, `WriteTool`, `BashTool`, `CheckpointedBashTool`, `SearchTool`, `PowerShellTool`, `SessionShellTool`; `CheckpointedBashTool`/`SessionShellTool` are internal wrappers used by the product, not new model-visible names. `SUPPORTED_TOOL_NAMES` = read, search, edit, write, bash, search_skills, load_skill, read_skill_resource (the last three are legacy skill tools, not registered by default — test `legacy_skill_tools_are_not_registered_by_default`) | `rg -n "impl Tool for" crates/octet-agent/src/tools/`; `sed -n '118,130p' crates/octet-coding-agent/src/config.rs` | VERIFIED |
| T6 | Tool surface / product default | Model-visible default is the four tools, `search` opt-in | `ToolPolicy::default()` filters `search` out of `SUPPORTED_TOOL_NAMES` ("bash already provides faster, composable discovery through rg/find/ls"), and `tool_schema_reserve_is_positive_and_deterministic` asserts the product surface is `["read","edit","write","bash"]` while `CoreTools` alone is `["read","edit","write","bash","search"]` | `sed -n '145,155p' crates/octet-coding-agent/src/config.rs`; `sed -n '3160,3170p' crates/octet-coding-agent/src/app/bootstrap/tests.rs` | VERIFIED |
| T7 | `#175` `/fast` | Codex `service_tier` is plumbed into the live run path | `durable_responses_options`/`native_responses_options`/`responses_prewarm_request` all take `requested_service_tier` and apply `with_service_tier`; `Agent::set_service_tier` is the setter; `resolve_service_tier` gates on `protocol == OpenAiResponses && responses_profile.accepts_service_tier()` (`types.rs:218` = `matches!(self, Self::Codex)`) and fails closed with `UnsupportedError::ServiceTier` | `rg -n service_tier crates/octet-agent/src/agent.rs`; `sed -n '3689,3770p' crates/octet-agent/src/agent.rs` | VERIFIED |
| T8 | `#175` `/fast` | "`/fast` reaches the wire" (`docs/parity/providers.md:40`) | **False for the product.** No file in `octet-coding-agent` calls `set_service_tier` (only `agent_run.rs` tests do), and `apply_fast_command` (`modes/interactive.rs:1539-1565`) still hard-errors: "`/fast on` not applied: this build's Codex request path does not send a service tier yet". The same doc's own body admits "`apply_fast_command` … still prints its `inert` message until its owner switches it to the new setter" | `rg -n "set_service_tier" crates/`; `sed -n '1538,1566p' crates/octet-coding-agent/src/modes/interactive.rs` | CONTRADICTED |
| T9 | `#175` ledger label | providers.md calls `service_tier` "row 1a.1" | `docs/parity/README.md:56` row 1a.1 is the provider-declarations row (baseten/qwen/zai); README contains no `service_tier`/`/fast` row at all, so the cross-reference points at an unrelated row | `rg -n "service_tier" docs/parity/README.md`; `sed -n '56p' docs/parity/README.md` | CONTRADICTED (label only) |
| L1 | Startup latency | The AWS metadata activation rule exists and is a pure function | `pub(crate) fn aws_metadata_activation_from(&AwsMetadataActivationInputs) -> AwsMetadataActivation` — all inputs are pre-read plain data (`ec2_metadata_disabled`, container URIs, endpoint/mode, product opt-in, profile `credential_source`/endpoint); `Disabled` returns before any client is built | `sed -n '518,660p' crates/octet-coding-agent/src/providers/auth.rs` | VERIFIED |
| L2 | Startup latency | Acceptance tests use request counters, not millisecond thresholds | `unrelated_provider_launch_makes_zero_aws_metadata_requests` (`auth.rs:1326`) and `indicated_metadata_probe_*` (`:1498`, `:1529`) count with `Arc<AtomicUsize>` (`:1336-1337`, `:1504`, `:1533`); `an_unavailable_metadata_endpoint_costs_one_bounded_request` (`:1587`) asserts a *count*. No `Instant`/`Duration` assertion in the metadata tests | `rg -n "AtomicUsize" crates/octet-coding-agent/src/providers/auth.rs`; `sed -n '1326,1370p' crates/octet-coding-agent/src/providers/auth.rs` | VERIFIED |
| L3 | Startup latency | Bedrock-with-metadata intent still works | An indicated environment still probes (container URI preferred, then EC2 IMDS) and the probe is bounded to the documented requests; `AWS_EC2_METADATA_DISABLED=true` and unknown values stay closed; `OCTET_AWS_METADATA_CREDENTIALS` is the explicit opt-in | `sed -n '510,540p'`, `:618,675`, `:1464,1540` `crates/octet-coding-agent/src/providers/auth.rs` | VERIFIED |
| X1 | LaTeX oracle | `--test latex_render` = 17 | `running 17 tests` / `test result: ok. 17 passed; 0 failed` in 0.01s | `cargo test --locked -p sexy-tui-rs --test latex_render` | VERIFIED |
| X2 | Mermaid oracle | `--test mermaid_render` = 11 | `running 11 tests` / `test result: ok. 11 passed; 0 failed` | `cargo test --locked -p sexy-tui-rs --test mermaid_render` | VERIFIED |
| X3 | Mermaid doc | Does not overclaim | `docs/parity/editor.md:213-236` explicitly lists `BT`/`RL` (rejected), `subgraph`/`end`/`direction`, `&` node lists, `A -- text --> B`, HTML entities (emitted literally) and shape outlines as unsupported/not modelled | `sed -n '196,260p' docs/parity/editor.md` | VERIFIED |
| X4 | Fence consumer | "No consumer is wired yet" (`editor.md` 2c.4; pass-1 §5) | **Stale.** `rich_text/markdown.rs:36-63` `render_diagram_fence` dispatches `latex`/`mermaid`/`graph`/`flowchart` fences; `markdown::parse` is re-exported as `parse_markdown` (`lib.rs:42`) and used by the TUI transcript (`crates/octet-coding-agent/src/tui/view/assistant_block.rs:7`, `StreamingMarkdown`); `tests/rich_fences.rs` (8 tests) pins that an unknown fence stays a plain code block (`unknown_fences_are_never_reinterpreted`) and that oversized/unterminated/unsupported fences stay literal | `rg -n "render_diagram_fence" crates/sexy-tui-rs/src/rich_text/markdown.rs`; `rg -n "parse_markdown" crates/octet-coding-agent/src/tui/view/assistant_block.rs` | CONTRADICTED (doc under-claims) |

## Pass 2 — contradictions (each side quoted)

**C10 — `providers.md:40` "`/fast` reaches the wire" vs. the command's own code.**
Header: "`## Codex service_tier (row 1a.1 — landed end-to-end; /fast reaches the wire)`",
body: "``/fast`` (roadmap #175) is **live on a Codex route**".
Against HEAD, `crates/octet-coding-agent/src/modes/interactive.rs:1551-1565`:

```
    let detail = concat!(
        "this build's Codex request path does not send a service tier yet, so ",
        "nothing changed on the wire; `/fast` stays inert until the request ",
        "builder supports it"
    );
    match requested {
        Some(true) => shell.error(format!("`/fast on` not applied: {detail}")),
```

and `rg -n set_service_tier crates/` finds **no** non-test caller outside
`crates/octet-agent/src/agent.rs` itself. So: the *primitive* is landed and
proven on the wire (the T7 evidence is real — the agent test captures
`"service_tier":"priority"` in the request body), but the *user-visible*
command still does nothing, and `providers.md` says both things in one section.
The honest sentence is the one already in `providers.md` itself ("that is the
one remaining step, and it is a UI edge, not a request-path gap"); the headline
must not survive into the PR body as-is.

**C11 — two documents still describe the pre-fix state (now under-claiming).**
`CHANGELOG.md` `[Unreleased]`, Providers section: "(*The `/fast` command remains
inert: the live-run `ResponsesOptions` builders do not yet set a tier.*)" — the
builders **do** set a tier at HEAD (`agent.rs:3710-3712`, `:3761-3763`); what is
missing is a caller. Same staleness in the `apply_fast_command` doc comment
(`modes/interactive.rs:1534-1538`): "The remaining missing primitive is the
caller: every live run's `ResponsesOptions` is built … without a tier". Direction
of error is *under*-claiming (harmless to users, wrong for a PR body).

**C12 — `editor.md` 2c.4 "No consumer is wired yet" is superseded.**
`docs/parity/editor.md:227-229`: "**No consumer is wired yet**
(`crates/octet-coding-agent` has no `rich_text::mermaid` call site)." True as
literally written (no direct call site) but false as a statement about the
product: the shared fence dispatcher in the same crate
(`rich_text/markdown.rs:36`) is reached from the TUI transcript through
`sexy_tui_rs::parse_markdown` → `StreamingMarkdown`
(`tui/view/assistant_block.rs:7,132`). `tests/rich_fences.rs` is the consumer's
behavioural test, including the unknown-fence fallback the pass-1 note asked
for. The Mermaid row's own "Not modelled" list is accurate and stays.

## Pass 2 — additional verdicts (latency, oracle, TUI, ledger)

| # | Area | Claim under test | Observation | Command run | Verdict |
| --- | --- | --- | --- | --- | --- |
| L4 | Startup latency | The ~1s AWS metadata penalty is real on this host | The **installed** 0.7.6 binary: 1.051s / 1.422s plain, 0.022s / 0.024s with `AWS_EC2_METADATA_DISABLED=true` (interleaved, same minute) | `octet doctor` timed with `python3 time.time()` around `subprocess.run` | VERIFIED (penalty exists) |
| L5 | Startup latency | The built checkout artifact no longer pays it | `target/debug/octet` (built 14:17, **predates** the 18:10 latency worker, dirty `providers/auth.rs`) still takes 0.549s / 0.500s plain vs 0.044s / 0.026s suppressed. Either the artifact is stale relative to the working tree or the fix is partial; the honest statement is that **the fixed behaviour is not demonstrated by the artifact on disk** | same measurement, both binaries interleaved | UNVERIFIED (needs a fresh `cargo build -p octet-coding-agent` on a frozen tree) |
| L6 | Startup latency | Binary identity: same version string ≠ same source | Installed `~/.local/bin/octet` 24,143,088 B (Sep 12 14:54, sha256 `96b138b0…`) and `target/debug/octet` 143,888,360 B (Sep 15 14:17, sha256 `df552ab6…`) **both print `octet 0.7.6`**. The installed build contains **0** occurrences of `OCTET_AWS_METADATA_CREDENTIALS`; the checkout build contains 1 | `octet --version`, `shasum -a 256`, `strings -a … \| grep -c OCTET_AWS_METADATA_CREDENTIALS` | VERIFIED (the investigation's warning is correct) |
| L7 | Startup latency | Env var spelling used in evidence | The binary reads `AWS_EC2_METADATA_DISABLED`. `AWS_EC2_METADATA_DISABLE=true` (the spelling in the investigation summary) changes **nothing**: 3 runs stayed at 1.01s, whereas the correct spelling drops to 0.022s | interleaved timing of both spellings | CONTRADICTED (evidence used a variable the binary ignores) |
| O1 | LaTeX oracle | "oracle-swept (1061 cases, 0 divergences), 407 goldens" | I re-ran the upstream oracle myself: (a) over the exact 407 `TABLE_GOLDENS` inputs from the committed test file → **0 divergences** against the committed goldens, which `cargo test --test latex_render` (17/17 green) ties to the port; (b) over `cases_gap.json` (1061 cases) → fresh oracle output **byte-identical** to the stored `expected_gap.json`, and 0 divergences against the port's captured output | `node --experimental-strip-types /tmp/ed8/oracle.ts <cases> <out>` (Node v26.7.0) + a parser for the port's `#CASE n some LEN` capture | VERIFIED (see caveat in the text) |
| U1 | Shimmer (tui12/13) | Model-adaptive shimmer, no rainbow outside max/ultra, real rest gap | `activity_shimmer_palette` derives both colours from `theme.model_rgb(model_lab)` and the ramp rotates only the model's own hue/saturation (neutral models stay neutral); `status_rainbow_strength_at` (`view.rs:1249`) returns 0 unless the reasoning level is `max`/`ultra` **and** elapsed < 2s, and the rainbow branch is unreachable for any other level; `ACTIVITY_SWEEP_REST_FRAMES = 2` with `activity_cycle` guaranteeing the first/last frame at rest colours and a test asserting `rest_frames >= ACTIVITY_SWEEP_REST_FRAMES` | `sed` on `tui/view/reasoning_render.rs:470-583`, `tui/view.rs:1247-1260`; tests `model_and_rainbow_shimmers_are_foreground_only`, `neutral_model_identities_shimmer_without_any_hue`, `max_and_ultra_working_rainbow_fades_for_two_seconds_only`, `shimmer_reaches_every_grapheme_and_loops_at_the_label_width` | VERIFIED (code + tests present; see note on in-flight edits) |
| U2 | Footer cost (tui13) | Plain dollar estimate, no `subtotal`/`+`/`~` | **Not landed.** `tui/composer_surface.rs:804` still renders `format!("subtotal {} + ?", format_microdollars(cost))` on the `usage_uncertain` path, and `composer_surface.rs` is *not* among the modified files | `rg -n "subtotal" crates/octet-coding-agent/src/tui/composer_surface.rs` | CONTRADICTED (unimplemented as of 18:2xZ) |
| U3 | Startup screen (tui14) | No phase/notice text; composer typeable before readiness | The startup phase trace is off by default and stderr-only (`app/bootstrap.rs:5299-5331`, env-gated); `startup_readiness_tests.rs` pins that the input owner paints and resizes before branding (`startup_input_owners_render_and_resize_without_releasing_branding`, `startup_lifecycle_waits_paint_the_draft_without_provisional_model_chrome`) | `rg` over `tui/view/startup_readiness_tests.rs`, `app/bootstrap.rs` | VERIFIED at HEAD for the composer; no *new* tui14 edit is present in the working tree |
| U4 | Subagents activity (tui14) | Settles into the transcript, does not replay on a new prompt | Present in the **working tree only** (`view.rs` dirty): `settled_subagent_workers` (`view.rs:1112`), `subagent_worker_ids` (`:832`), the ignore-settled-snapshot path (`:1650-1702`), the deliberate non-clear at `:2866` and the clear on session replacement (`:5985`), plus tests `a_settled_subagent_roster_never_replays_under_a_later_prompt` (`view/tests.rs:10748`) and `live_workers_for_the_current_turn_still_open_a_block` (`:10869`); the dirty `shell_chrome.rs` diff deletes the duplicated chrome strip. Green run **not observed** by me (the file was mid-edit and the target was still building) | `rg -n "settled_subagent_workers\|subagent_worker_ids" crates/octet-coding-agent/src/tui/view.rs`; `git diff -- crates/octet-coding-agent/src/tui/view/shell_chrome.rs` | UNVERIFIED (code present, test result not obtained) |
| C13 | `CHANGELOG.md` known gaps | "the `octet-ai` test suite is **red and partly non-terminating** at this checkpoint" | False at HEAD: 13/13 targets green and the previously hanging test finishes in 1.76s | `cargo test -p octet-ai --no-fail-fast`; `--lib -- --exact responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` | CONTRADICTED (stale gap) |
| C14 | `README.md` row 3.5 | "Landed; boundaries wired, behavioral test pending" | Under-claim: the boundary tests exist and `docs/parity/telemetry.md:80-97` now lists them by file:line (`agent_run.rs:9411`, `:9513`, `:9598`, `telemetry_conformance.rs:113`, `delegation.rs:8027`) | `rg -n "fn typed_spans_" crates/octet-agent/` | CONTRADICTED (stale, under-claiming) |

### Startup-latency measurement detail (L4–L7)

Raw numbers, alternating the two binaries in the same minute (18:33Z):

```
r0 installed ~/.local/bin/octet plain          1.051s
r0 installed ~/.local/bin/octet DISABLED=true  0.022s
r0 checkout target/debug/octet  plain          0.549s
r0 checkout target/debug/octet  DISABLED=true  0.044s
r1 installed ~/.local/bin/octet plain          1.422s
r1 installed ~/.local/bin/octet DISABLED=true  0.024s
r1 checkout target/debug/octet  plain          0.500s
r1 checkout target/debug/octet  DISABLED=true  0.026s
```

`DISABLED=true` is `AWS_EC2_METADATA_DISABLED=true`. No `AWS_*` variable is set
in this shell and `~/.aws` does not exist, so the new activation rule should
classify this host as `Unindicated → Disabled` and never build a client. The
installed (pre-fix) binary shows the penalty plainly; the checkout artifact
halves it rather than removing it, and that artifact is older than the work it
is supposed to contain. **Do not claim the latency fix is verified end-to-end
until a binary built from the frozen tree is measured.** What *is* verified is
the mechanism (L1–L3: pure rule, counter-based tests, indicated environments
still probe) — subject to the tests actually running, which is recorded in the
test-surface section below.

### Oracle detail (O1)

The pass-1 concern about a committed 398 KB oracle is resolved (the `_*.rs`
harnesses are untracked and `.gitignore`d), but that left the "1061 cases,
0 divergences" claim resting on a worker receipt. I re-derived it:

- `TABLE_GOLDENS` in `crates/sexy-tui-rs/tests/latex_render.rs` has exactly
  **407** entries; I extracted them, ran the real upstream `latex.ts`
  (`/tmp/ed8/oracle.ts`, Node v26.7.0) over them in inline mode, and compared to
  the committed expected glyphs: **`total=407 divergences=0`**. Since
  `every_symbol_table_entry_renders_its_reference_glyph` asserts
  `render_latex(x, default) == golden` and that test passes (17/17), the oracle
  and the port agree on this corpus.
- `/tmp/ed11/cases_gap.json` holds **1061** cases; my fresh oracle run is
  **byte-identical** to the stored `expected_gap.json`, and when compared against
  the port's captured `#CASE n …` output it reports **`total=1061 divergences=0`**.
- Caveat, stated plainly: for the 1061-case sweep the *port* side is the
  worker's captured output file (`/tmp/ed11/rust_gap.txt`), not a run I made.
  The oracle side is mine, byte-for-byte. The 407-case corpus is fully
  independent on both sides.

## Pass 2 — test surface (my own runs)

| Command | Observed | Verdict |
| --- | --- | --- |
| `cargo check --workspace --all-targets --locked` (18:21Z) | `Finished` 3.46s, 0 `error` lines, 89 `is never used` warnings | VERIFIED |
| `cargo test -p octet-ai --no-fail-fast` (18:22Z) | 13/13 targets ok: 366, 5, 3, 18, 1, 38, 3, 2, 16, 4, 1, 1, 1; 0 failures | VERIFIED |
| `cargo test -p octet-ai --lib -- --exact responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` (18:22Z) | `ok. 1 passed … finished in 1.76s` (this is the test that previously **hung**) | VERIFIED (no hang) |
| `cargo test -p sexy-tui-rs --test latex_render` (18:22Z) | `ok. 17 passed` | VERIFIED |
| `cargo test -p sexy-tui-rs --test mermaid_render` (18:22Z) | `ok. 11 passed` | VERIFIED |
| `cargo test -p sexy-tui-rs --no-fail-fast` (18:22–18:24Z) | lib `190 passed`; 16 targets ok; **`rich_fences` FAILED 9 passed / 1 failed** | RED (one test; see below) |
| `cargo test -p octet-agent --test parity_tools --test telemetry_conformance --test delegation --no-fail-fast` (18:24Z) | 11 / 17 / 9 passed, 0 failed (target order as invoked) | VERIFIED |
| `cargo test -p octet-coding-agent --test codex_context_window --test slash_command_pty --test activity_wait_pty --test setup_cli_acceptance --test setup_tui_acceptance --no-fail-fast` | *still running when this section was written* | PENDING |
| `cargo test -p octet-coding-agent --lib --no-fail-fast` | *still running* | PENDING |
| `cargo test -p octet-agent --test agent_run` / `-p octet-agent` | *still running* | PENDING |

**The only RED this pass is inside an in-flight edit.**
`test streaming_keeps_failed_diagram_fences_as_source ... FAILED`
(`crates/sexy-tui-rs/tests/rich_fences.rs:242`: "raw body visible while the
fence is open"). Two facts pin the attribution: HEAD's copy of the file has
**8** tests and all 8 pass in this run; the working tree has **10**, and the two
added by the in-flight edit are `renders_that_produce_nothing_keep_the_original_source`
(passes) and `streaming_keeps_failed_diagram_fences_as_source` (fails). So the
committed fence consumer is green and the editor worker's new streaming case is
red at 18:24Z. `git status` shows `rich_fences.rs`, `markdown.rs` and
`latex/mod.rs` all dirty, so this is a mid-edit observation, not a HEAD verdict.

## Pass 2 — the 88 ledger rows: which states I can defend

`docs/parity/README.md` has exactly **88** rows (26 plain `Landed`, 8
`Verified`, 14 `Unverified`, 14 `Pending`, and the qualified remainder). I did
not re-derive all 88; I checked the rows whose state is cheap to falsify and the
rows this wave changed. Below, "defended" means I looked at the code the row
names.

**States I checked and can defend**

- `4.1`/`4.2`/`4.3` "Withdrawn by maintainer decision" — correct; the modules
  are gone, the guard test exists, and `tools.md` states plainly what is *not*
  covered (`ls`-style bounds, filename-only discovery). This is the honest
  direction of error.
- `2c.3` "Landed; oracle-swept (1061 cases, 0 divergences), 407 goldens" —
  defended (O1): 407 goldens counted, both sweeps reproduced on the oracle side,
  17/17 tests green.
- `2c.4` "Landed; bounded self-captured subset, unsupported syntax fails closed"
  — defended (X2/X3), with the one doc correction C12.
- `4.7`–`4.14` "Landed … tool layer, consumer pending" — the qualified wording
  matches the tree; their consumers genuinely do not exist yet.
- `5.4` "Blocked; needs accounting-only session backend" and `1a.2` "Blocked;
  release-blocking primitive" — the strongest kind of ledger entry (blocked with
  a named primitive).
- `2a.*`, `2c.1`, `2c.2`, `2c.5`, `2d.*` "Unverified"/"Pending" — honestly marked.

**States I cannot defend (flag these before the PR body quotes them)**

| Row | Ledger state | What the code shows |
| --- | --- | --- |
| `1b.1` | `Landed` | The row requires per-request `apiKey`, `fetch`, `onPayload`, `onResponse`, `transformHeaders`, `metadata`. `providers.md:120-135` itself says this is "**partial, declared plumbing**" and that the transformer/payload seam "is **reported, not changed**". `crates/octet-ai/src/client.rs` contains no `transform_headers`/`on_payload` seam at all. |
| `1b.2` | `Landed` | `ModelPreset::sampling_params` and `ModelPreset::headers` are declared in `crates/octet-ai/src/declarations/mod.rs`, but `rg -n sampling_params crates/` returns **zero references outside that file** — no codec or client merges them into a request. `providers.md` says "merge wiring is reported, not changed". |
| `1b.5` | `Landed` | Same shape: `providers.md` says the codec that emits `chat_template_args`/`priority`/`max_output_tokens`/`thinking_token_budget` is unwired, "emission wiring is reported, not changed". |
| `2b.1` | `Landed` | "Namespaced configurable JSON keybindings, conflicts, platform defaults" is a *module*: `KeybindingsManager` exists (`tui/keymap/keybindings.rs:187`) but `rg -n KeybindingsManager crates/` finds **no production instantiation** — the only outside reference is the doc comment at `tui/view/input_overlays.rs:503-509`, which carries the code's own admission: "TODO(resolved-binding): the translator still hardcodes `KeyCode::Up + KeyModifiers::ALT` … Until both exist". A user's keybinding file is never read by the running shell. |
| `3.5` | `Landed; boundaries wired, behavioral test pending` | Under-claims: the boundary tests exist (`telemetry.md:88-97` lists five by file:line) and they pass in this pass. |
| `1c.2` | `Landed; unsupported claim removed` | Defensible only through the row's "or removal" branch; `CHANGELOG.md`'s Known gaps still lists "the deferred additional-tools/tool-search emit paths" as pending. Quote the removal, not the capability. |

Rows I did **not** re-check (no state change suggested, but the PR body should
not present them as pass-2 verified): `1a.1`, `1b.3`, `1b.4`, `1b.6`, `1c.1`,
`1c.3`–`1c.10`, `1d.1`–`1d.3`, `1e.1`–`1e.3`, `2b.2`–`2b.6`, `3.1`–`3.4`,
`3.6`, `4.4`–`4.6`, `4.9`, `5.1`–`5.3`, `5.5`–`5.11`, `6.1`–`6.5`. Two of them
were spot-checked positively: `3.6` (`cache_write_1h` is real in
`octet-ai/src/pricing.rs:29-139`) and `4.4` (the bash tool's spill file and
`full_output_path` exist in `octet-agent/src/tools/bash.rs:986-1012`).

## Pass 2 — failure modes hunted

- **Claimed pass with no observable evidence.** The one that matters:
  `docs/parity/providers.md:40` (C10). Two lesser ones are stale in the
  *under*-claiming direction (C11, C12) and one is a stale "Known gap" (C13).
- **"Landed" primitive with no consumer.** Three of them, all in the ledger's
  `Landed` column: `ModelPreset::sampling_params`/`headers` (zero references
  outside `declarations/mod.rs`), the `KeybindingsManager` keybinding layer, and
  — in the working tree only — the subagent settle path (U4, consumer exists but
  unverified). `tools.md` documents its own subset honestly; the other two are
  not documented as tool-layer-only.
- **`todo!()` / `unimplemented!()` / stub bodies.** Clear in Rust:
  `rg -n "todo!\(|unimplemented!\(" --type rust crates/` → **0 matches**, and
  there are no `FIXME:`/`TODO:` markers in the four crates' sources except the
  deliberate `TODO(resolved-binding)` prose in
  `crates/octet-coding-agent/src/tui/view/input_overlays.rs:501` (which is the
  admission quoted in the table above).
- **Live/external verification never run.** No pass-2 document claims a tmux,
  herdr, Windows, macOS GUI or Xcode run; `tools.md` and `extensions.md` mark
  those as unqualified. The LaTeX differential-oracle claim *is* external
  (`latex.ts` under Node) and I re-ran it myself (O1), so it moves from
  "claimed" to "reproduced".
- **Secrets / session ids in argv or display strings.** No new leak found in the
  changed files: `providers/auth.rs` reads credentials from env/profile/IMDS into
  typed structs and never formats them into output; the Codex resolver asserts
  (`auth/codex/resolver.rs:381`) that a token never appears in an error's `Debug`
  or `Display`; the subagent launcher was verified in pass 1 (list argv,
  `shell=False`, `_SAFE_TOKEN_RE`).
- **A wrong `Landed` is worse than `Unverified`.** All of the above are
  `Landed`-column problems, not `Unverified` ones.

## Pass 2 — disposition of every pass-1 finding

| Pass-1 item | State at `df2e8980` |
| --- | --- |
| C1 telemetry.md under-claims row 3.5 | **RESOLVED.** `docs/parity/telemetry.md:77` now heads "3.5 Span boundaries — landed" and lists the five boundary tests by file:line. The under-claim moved into `README.md` row 3.5 (C14). |
| C2 providers.md over-claims `/fast` | **SUPERSEDED by C10.** The blocker it named (the `ResponsesOptions` builders) is genuinely gone; a different, user-facing gap remains. |
| C3 editor.md 2c.3/2c.4 "Blocked" | **RESOLVED.** Both sections now read "Landed" with the bounded-subset wording. |
| C4 398 KB oracle committed | **RESOLVED.** The five `_*.rs` harnesses are untracked (`git status --porcelain crates/sexy-tui-rs/tests/` is clean, `.gitignore` covers `/crates/sexy-tui-rs/tests/_*.rs`). |
| C5 CHANGELOG empty | **RESOLVED.** `[Unreleased]` now carries the parity sections; the *Known gaps* paragraph inside it is stale (C13). |
| C6 subagents fail-closed policy | **RESOLVED** (pass 1's own re-run) and unchanged. |
| C7 `octet-ai` lib red + infinite hang | **RESOLVED — my own run.** 13/13 targets green; the previously hanging test completes in 1.76s. |
| C8 `client_stream.rs` 6 failures | **RESOLVED — my own run.** 38 passed, 0 failed. |
| C9 `octet-coding-agent --lib` red (10 → 7 failures) | **RE-RUN in this pass; result in the test-surface table above.** Two of the seven were in files no worker touched (`tui/pickers.rs`, `modes/interactive.rs`), so this is the row to watch. |
| F1 `apps/web` flaky under load | **NOT RE-RUN** (no Node/vitest run in this pass) — still an unverified flake claim. |
| F2 `entry_index_revision` dead primitive | **STILL TRUE.** `session_store.rs:1791` remains the only reference (`rg -c entry_index_revision` → 1 occurrence in the whole crate). |
| F3 112 `is never used` warnings / undocumented primitives | **PARTIALLY TRUE.** My `cargo check` emits **89** `is never used` + 4 `is never read` warnings (157 warnings total, 0 errors). The undocumented-consumer class now has a second confirmed member (row 2b.1 keybindings). |
| F4 `todo!()`/`unimplemented!()`/`#[expect(dead_code)]` | **CLEAR — re-verified**, 0 matches. |
| F5 three redundant `#[allow(dead_code)]` | **RESOLVED.** None remain in `telemetry/schema.rs` or `telemetry/spans.rs`. |
| F6 secrets in argv/display strings | **CLEAR** (pass 1) and no new leak in the changed files. |
| F7 overclaimed live/external verification | **CLEAR**, and the one external claim (the Node oracle) is now independently reproduced (O1). |
| F8 doc bullets claiming capability the code lacks | **WORSE, in the ledger.** Rows 1b.1/1b.2/1b.5/2b.1 are `Landed` for behaviour whose own detail doc says the wiring is "reported, not changed"; the previous extensions.md numeric slip is unchanged (`extensions.md:30` "13 + 5 tests" still not reproducible from the file, 13 `#[test]`). |

## Pass 2 — what I could not verify

- **`cargo test -p octet-coding-agent --lib` at HEAD.** The target was still
  compiling when I wrote this; the working tree was dirty in four files
  (`providers/auth.rs`, `tui/view.rs`, `tui/view/reasoning_render.rs`,
  `rich_text/latex/mod.rs`) and other workers were running their own
  coding-agent builds, so the lock queue was the constraint. Needed: the same
  command on a frozen commit with the tree quiet. **This is the one surface that
  must be green before the PR is cut** — pass 1 saw it red twice.
- **Fixed startup latency, end to end.** Needs a binary built from the frozen
  tree (L5).
- **The 1061-case Rust side of the LaTeX oracle.** The oracle side is mine and
  byte-identical; the port side is a captured file the worker produced (O1).
- **`apps/web` `npm test` and `python3 -m pytest` suites** — not re-run this
  pass; pass-1 results stand (2 load-induced vitest timeouts, one proven
  environmental `ygg_extension` failure).
- **Windows, macOS GUI, tmux/herdr, Xcode build/sign** — unchanged and
  unavailable; no document claims otherwise.
- **`cargo test -p octet-agent --test agent_run` at HEAD** — this is one of the
  targets still queued; pass 1 saw one failure and one >5-minute test.

## Pass 2 — remaining primitives for blocked/qualified rows

Unchanged in substance from pass 1 except where noted; each is a code-level
gap, not a restatement of "pending".

1. **`/fast` (roadmap #175): one call site.** `apply_fast_command`
   (`crates/octet-coding-agent/src/modes/interactive.rs:1539`) must call
   `Agent::set_service_tier` instead of printing the inert message, and the
   session should persist the tier (`providers.md` "Gap 2"). Everything on the
   `octet-agent` side now exists and is tested.
2. **Row `1b.1`/`1b.2`/`1b.5` consumers.** A per-request
   transformer/payload seam in `crates/octet-ai/src/client.rs`, and the codec
   merge of `ModelPreset::sampling_params`/`headers` and the
   chat-template/thinking-budget fields into the request body. Until then those
   rows must not read `Landed` in a PR body.
3. **Row `2b.1` consumer.** A shell-owned `KeybindingsManager` whose `matches`
   replaces the hardcoded `alt+up` arm in `tui/keymap.rs` and whose
   `get_keys("app.message.dequeue")` feeds the queued-follow-up hint.
4. **Row `3.5`/`C14`:** correct the ledger state (tests exist and pass).
5. **`/fast` headline + `CHANGELOG.md` Known gaps + `editor.md` 2c.4 "no
   consumer"** — three docs to correct (C10/C11/C12/C13).
6. **Codex websocket resumption in live runs** (`body_requests_storage` never
   set by the agent builders), per-request transport selection (`1c.6`), the
   proxy seam (`1b.3`, `client.rs:1891`), and `client_stream` reconciliation —
   carried over from pass 1; `client_stream` itself is now green.
7. **Tool-layer primitives with no consumer** (rows 4.8, 4.10, 4.11, 4.13,
   4.14): each needs its consumer, exactly as `docs/parity/tools.md` records.
8. **`entry_index_revision` (`session_store.rs:1791`)** — either wire it or
   delete it; it is dead at HEAD.

## Pass 1 record (HEAD 7be2dc96, retained for history)

Adversarial verification pass, written by a verifier that did not author any of the
rows it checks. It re-runs the commands rather than trusting the evidence files.
Where this document disagrees with a detail document or an
`docs/swarm-audit/EXECUTION-*.md` receipt, prefer this document.

- Branch: `vibe/pi-parity-roadmap-df5a7e80`
- HEAD at start: `9c43111dad46b9c557bf14be7428c9471a84d4b1` ("vibe: wave 7 checkpoint …").
  HEAD at the end of this pass: `7be2dc96` — **a checkpoint was committed by the
  parent while I was verifying**, so every result below is stamped with the local
  time it was taken (EDT) and the tree was dirty for most of the pass.
- Base for diffs: `df5a7e80`
- Method: every "command run" cell below is a command this verifier ran on this
  host at the stated HEAD. No result is copied from a worker receipt.
  Un-run checks are marked `UNVERIFIED` with the exact missing precondition.

## Open items the PR body must state

1. `cargo test -p octet-coding-agent --lib` **fails** (7 tests at 13:42), including
   two (`tui/pickers.rs:1841`, `modes/interactive.rs:1166`) in files no worker
   modified, so they are real HEAD failures (C9). A second surface,
   `cargo test -p octet-ai --test client_stream` (6 failures) and
   `cargo test -p octet-agent --test agent_run`
   (`websocket_connection_limit_is_retried_by_agent`) were red when I measured
   them; `client_stream` was fixed at 13:42 (C8).
2. `docs/parity/telemetry.md:76` claims row 3.5 is "NOT landed" with "no
   behavioral boundary test exists" — false at HEAD; the spans and three
   boundary tests exist (C1).
3. `docs/parity/providers.md:40` claims Codex `service_tier` "unblocks roadmap
   #175 `/fast`" — `/fast` is still inert by design, as its own code says (C2).
4. `docs/parity/editor.md:125-141` still calls rows 2c.3/2c.4 "Blocked" although
   the LaTeX and Mermaid modules and their tests are in HEAD (C3).
5. Rows 4.7/4.8/4.10/4.11/4.12/4.13/4.14 and several `octet-coding-agent`
   primitives are tool-/module-layer only, with **no production consumer**
   (F2, F3). tools.md says so honestly; the `octet-coding-agent` ones do not.
6. `npm test` in `apps/web` is load-sensitive: 2 of 299 tests time out under
   concurrent compilation and pass in isolation (F1). Either raise their timeout
   or run CI on a quiet machine.
7. Three redundant `#[allow(dead_code)]` attributes (`telemetry/schema.rs:156`,
   `:261`, `telemetry/spans.rs:244`) were added by this work (F5).

## Overall status

**The tree compiles and most suites pass, but it is NOT green.
`cargo test -p octet-coding-agent --lib` still fails 7 tests at the end of this
pass — two of them in files no worker modified — and three other surfaces were
red while I measured them (`client_stream`, `agent_run`, and the `octet-ai` lib,
all since repaired). On top of that, four documentation claims are wrong against
HEAD (two over-claim, two under-claim). Everything else below is a verified pass
or a precisely-scoped "unverified".

- `cargo check --workspace --all-targets --locked` is clean (0 errors) — but it
  briefly stopped compiling during this pass on an unfinished worker edit, so
  "green" is only true between edits.
- The previously reported subagents fail-closed failure is **FIXED** (C6).
- The previously reported `octet-ai` lib failures and infinite hang are
  **FIXED mid-pass** (C7): red at 13:19, green at 13:33 — but that same rewrite
  left the uncited `client_stream` target red (C8), so the file's test surface is
  still not clean.
- Documentation is wrong in four places: telemetry (C1, under-claims), `/fast`
  (C2, over-claims), editor 2c.3/2c.4 (C3, stale), extensions test count (F8).
- One Python-suite failure is **purely environmental** and proven so (§Other
  suites).
- Two of the earlier findings were remediated while I verified: the committed
  LaTeX oracle (C4) and the empty CHANGELOG (C5).

## Contradictions (each side quoted)

**C1 — `docs/parity/telemetry.md` underclaims: row 3.5 is landed.**
The doc states (line 76 heading, line 78) "## 3.5 Span boundaries — NOT landed"
and "The seven named boundaries (…) are **not** wired into the `octet-agent` run
generator. No behavioral boundary test exists." Against HEAD:

- `crates/octet-agent/src/agent.rs:6389` `telemetry.begin_typed::<RunSpan>`,
  `:6462` `TurnSpan`, `:6863` `ProviderRequestSpan`, `:7006` `ProviderStreamSpan`,
  `:4659` / `:7195` `CompletionAttributes::record`;
  `crates/octet-agent/src/delegation.rs:3187` `begin_typed::<DelegationSpan>`.
- Boundary tests that DO exist: `crates/octet-agent/tests/agent_run.rs:9411`
  `typed_spans_nest_run_turn_provider_and_tool_boundaries`,
  `:9513` `typed_spans_label_failed_runs_without_changing_accounting`,
  `:9598` `typed_spans_cover_compaction_and_summary_boundaries`;
  `crates/octet-agent/src/delegation.rs:7316`
  `delegation_span_owns_the_child_run_and_nests_child_spans`;
  `crates/octet-agent/tests/telemetry_conformance.rs:113`
  `typed_instrumentation_nests_children_under_the_typed_span`.

The `#[allow(dead_code)]` attributes at `telemetry/schema.rs:156` (`begin_typed`),
`schema.rs:261` (`record`) and `spans.rs:244` (`context`) are also **stale** — all
three are called from the live generator (locations above). I re-ran the target:
`cargo test -p octet-agent --test telemetry_conformance` → `ok. 9 passed`, which
includes `typed_instrumentation_nests_children_under_the_typed_span`. Direction of
the error is *under*claiming, but it must be corrected so the PR does not carry a
false "not landed" line.

**C2 — `docs/parity/providers.md:40` overclaims `/fast`.**
Headline: "## Codex `service_tier` (row 1a.1 — landed, unblocks roadmap #175
`/fast`)". The codec field did land (`crates/octet-ai/src/responses.rs`
`ResponsesOptions::service_tier`), but the roadmap row is **not** unblocked. The
command implementation says so itself —
`crates/octet-coding-agent/src/modes/interactive.rs:1539-1560`
(`apply_fast_command`) returns
"``/fast`` is inert: the Codex `service_tier` field exists in octet-ai, but the
live request path never sets `ResponsesOptions::service_tier` (missing primitive:
the `ResponsesOptions` builders in crates/octet-agent/src/agent.rs), so nothing
changed on the wire". The provider doc's own "Gap" paragraph names only
`applyServiceTierPricing`, not this blocker. The behaviour is fail-closed and
loud (good), but the headline is false.

**C3 — `docs/parity/editor.md:125-141` is stale (rows 2c.3 / 2c.4 "Blocked").**
`crates/sexy-tui-rs/src/rich_text/latex/{mod,tables}.rs` and
`rich_text/mermaid.rs` exist in HEAD, with `tests/latex_render.rs` (11 `#[test]`).
The doc's "no Rust equivalent in the workspace" is no longer true.

**C4 — a 398 KB generated oracle is committed.**
`crates/sexy-tui-rs/tests/_latex_diff.rs` is 398,351 bytes and tracked
(`git ls-files`), alongside `_latex_debug.rs`, `_mermaid_debug.rs`,
`_latex_probe.rs`, `_latex_stress.rs`. A prior editor receipt claimed the harness
"lives in /tmp (not committed)"; that is false. Recommendation: delete or
`.gitignore` before the PR.

**C5 — CHANGELOG.md is empty of parity work.**
`git diff --stat df5a7e80 HEAD -- CHANGELOG.md` is empty and `## [Unreleased]`
has no entries, although the parity docs ship dozens of "CHANGELOG-ready"
bullets and `docs/parity/README.md` itself requires one per item.

**C6 — FIXED since the previous verifier: subagents fail-closed policy.**
The prior receipt recorded
`test_orchestrator.py::PolicyTests::test_spawn_schema_policy_allows_whitelisted_mutation_and_rejects_outliers`
failing (`{"model": "other"}` → not rejected). Re-run on this host:
`python3 -m pytest extensions/octet-subagents/tests -q` →
**76 passed, 61 subtests passed** in 0.88s. No failure. (Test count grew from
the previously reported 55, so the owner also extended the suite.) This
contradiction is resolved.

## Contradiction C7 (was blocking; fixed mid-pass) — `octet-ai` lib was red and hanging

This is the most serious finding. It was reported by the previous verifier and it
is **still true at HEAD 9c43111d**; I reproduced it with my own run.

Command: `cargo test --locked -p octet-ai -- --skip reconnect_attempts_and_total_wait_are_bounded`
Result: `test result: FAILED. 346 passed; 4 failed; 0 ignored; 0 measured; 1 filtered out; finished in 2.32s` (EXIT 101).

| Failing test | Panic location | Message |
| --- | --- | --- |
| `responses_ws::tests::a_drop_before_output_reconnects_and_resumes_with_each_delta_once` | `crates/octet-ai/src/responses_ws.rs:2492` | `assertion left == right failed; left: 0, right: 1` |
| `responses_ws::tests::a_mid_stream_drop_resumes_from_the_cursor_with_each_delta_once` | `crates/octet-ai/src/responses_ws.rs:2698` | `assertion failed: connection.alive.load(Ordering::Acquire)` |
| `responses_ws::tests::failed_terminals_retire_before_publication_for_text_and_binary` | `crates/octet-ai/src/responses_ws.rs:2160` | `failure escaped before the pool key was disabled` |
| `responses_ws::tests::fatal_events_retire_before_publishing_with_a_contended_pool` | `crates/octet-ai/src/responses_ws.rs:2087` | `assertion failed: matches!(error, AiError::Transport(_) \|\| AiError::Decode(_))` |

In addition, `responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded`
**hangs indefinitely**: the harness prints "has been running for over 60 seconds"
and does not finish. The previous verifier's own hung process
(`/tmp/verify7/head-target/debug/deps/octet_ai-69051c8013db57f1`) was still alive
≈50 minutes after it started, which rules out ordinary slowness.

Attribution. `crates/octet-ai/src/responses_ws.rs` is one of the files this
swarm rewrote:

- `git diff --stat df5a7e80 HEAD -- crates/octet-ai/src/responses_ws.rs` →
  `1639 insertions(+), 301 deletions(-)`; the file grew 1645 → 2983 lines.
- `git diff --stat 28c09976 HEAD -- …` (wave 7 alone) → `526 insertions(+), 37 deletions(-)`.

`failed_terminals_retire_before_publication_for_text_and_binary` and
`fatal_events_retire_before_publishing_with_a_contended_pool` both **pre-existed
at base** (`git show df5a7e80:…/responses_ws.rs` contains them); the two
`a_drop…`/`a_mid_stream_drop…` tests and the hanging test are **new**. So at
least two failures are on behaviours this branch changed. *Whether the two
pre-existing tests passed at base is UNVERIFIED* — establishing that needs a
build of the base tree, which I did not run (see "What I could not verify").

This directly qualifies the "workspace is green" claim: `cargo check` is green,
but the `octet-ai` test suite is **red and partly non-terminating**.

### Note on measurement conditions

Seven workers share this checkout. Every `cargo` invocation takes the same
`target/` lock, so at the time of this run several `cargo test` processes were
serialised (one waiting printed `Blocking waiting for file lock on build
directory`). Results below are therefore taken from runs that completed; a
blocked run is not counted as a result.

## Failure modes hunted (each with evidence)

**F1 — a claimed test pass that does not reproduce.**
The predecessor recorded `apps/web` `npm test` as "299 passed". My own run
(`cd apps/web && npm test`, 13:20 local) gives
`Test Files 2 failed | 33 passed (35)`, `Tests 2 failed | 297 passed (299)`, EXIT=1.
Both failures are `Error: Test timed out in 5000ms`:
`src/App.transcript-search.test.tsx:74` and `src/components/FleetOverview.test.tsx:110`.
Run in isolation (`npx vitest run src/App.transcript-search.test.tsx src/components/FleetOverview.test.tsx`)
both pass: `Test Files 2 passed (2); Tests 5 passed (5)` in 3.14s. So the suite is
**load-sensitive, not logically broken** — the failing tests take 1.2s / 2.0s
alone and exceed the 5s default while seven workers compile concurrently.
`npx tsc -b` is clean (EXIT=0). Verdict: `npm test` is **flaky under load**; a PR
claim of "299 passed" is not reproducible without a quiet machine.

**F2 — a "landed" primitive with NO consumer, and it is *not* documented as such.**
`crates/octet-coding-agent/src/session_store.rs:1602`
`pub fn entry_index_revision(&self) -> anyhow::Result<i64>` is **new in this
diff** (`git diff df5a7e80 HEAD` adds it) and has **exactly one reference in the
whole workspace** — its own definition (`rg -c '\bentry_index_revision\b' crates`
→ 1). The compiler agrees: `warning: method \`entry_index_revision\` is never used`.
It is not mentioned in any `docs/parity/*.md`. (Note the near-name
`entry_revision`, a different, wired primitive — this one looks like a leftover.)

**F3 — the same pattern at scale, partially documented.**
`cargo check --workspace --all-targets --locked` emits **112 `is never used`
warnings** (52 functions, 13 methods, 11 associated items, 11 structs,
11 constants, 8 enums, 2 fields). 15 of those names also appear as newly-added
definitions in this diff — e.g. `classify_reload_failure` (`tui/theme.rs:2011`),
`set_active_theme` (`tui/theme_reload.rs:336`, called only from its own test at
`:747`) and the `keybindings.rs` resolution pair (`key_event_id` at `:141` plus
its only caller `matches`). tools.md is candid about the subset it owns ("Rows
4.7, 4.11, 4.12, and 4.14 are landed as **tool-layer primitives** … documents the
exact consumer that still has to be wired"), and I confirmed each has no
production consumer: `AdaptivePreviewCoalescer` (`tool.rs:1137`) — none;
`batch_requests_termination` / `ToolOutput::requesting_termination`
(`tool.rs:1302`, `:1458`) — no loop consumer;
`collect_tool_prompt_contributions` (`tool.rs:139`) — called only from
`tests/parity_tools.rs:1000-1079`. **But the theme-reload / keybinding-resolution
primitives in `octet-coding-agent` are not documented anywhere.** The PR body
must not present them as user-visible behaviour.

**F4 — `todo!()` / `unimplemented!()` / `#[expect(dead_code)]` — CLEAR.**
`git grep -nE 'todo!\(|unimplemented!\(' HEAD` matches **only prose in
`docs/swarm-audit/EXECUTION-verify7.md`**; no source file contains such a macro.
`git grep 'expect(dead_code)' HEAD` likewise matches only docs. The code-only
diff adds zero of either.

**F5 — three redundant `#[allow(dead_code)]` added by this work.**
`crates/octet-agent/src/telemetry/schema.rs:156` (`begin_typed`), `:261`
(`CompletionAttributes::record`) and `crates/octet-agent/src/telemetry/spans.rs:244`
(`SpanGuard::context`) each carry a *new* `#[allow(dead_code)]` (the code-only
diff adds exactly these 3). All three are now called from production code
(`agent.rs:6389`, `:4540`, `:4659`, `:7195`; `delegation.rs:3187-3188`), and the
compiler lists none of them among the 112 `never used` warnings, so the
suppression is unnecessary. Harmless today; remove before the PR.

**F6 — secrets / session ids in argv or display strings — CLEAR.**
`extensions/octet-subagents/octet_subagents/launcher.py` (684 lines):
`_run` (`:446`) always calls `subprocess.run(list(argv), shell=False, timeout=…)`
and refuses empty/oversized argv; `_pane_argv` (`:316`) builds tmux argv lists;
the only string join is `_herdr_command`, and every token must first match
`_SAFE_TOKEN_RE = ^[A-Za-z0-9_@%+=:,./-]{1,512}$` (`:67`). The only environment
read is `OCTET_SUBAGENTS_OCTET_BIN` (`:136`), a binary path. No credential,
token or transcript path is read, printed or passed.

**F7 — overclaimed live/external verification — CLEAR.**
`rg -i 'verified live|ran live|smoke-tested manually|verified on hardware'` over
`docs/parity/*.md` and `docs/*.md` returns nothing. The docs state the opposite
explicitly: tools.md row 4.6 "the Windows execution path is `#[cfg(windows)]` and
is **not compiled here**: real Windows CI evidence is blocked on a Windows
runner"; extensions.md marks the macOS/Windows native backends "Implemented,
**not qualified**" and lists "macOS/Windows automation on real hardware" as
absent. No doc claims a tmux/herdr/Terminal.app live run.

**F8 — doc bullets claiming capability the code lacks — partially CLEAR.**
No parity doc claims a capability the code wholesale lacks; the errors found are
an *under*claim (telemetry.md), a stale *block* (editor.md 2c.3/2c.4) and an
*over-claim of consequence* (providers.md `/fast`). One numeric slip:
`docs/parity/extensions.md:30` says `extension_theme_selection.rs` has
"(Rust 13 + 5 tests)"; the file contains exactly 13 `#[test]` functions and the
source module has none, so "13 + 5" is not reproducible.

**C9 (OPEN) — `cargo test -p octet-coding-agent --lib` is RED.**
`cargo test --locked -p octet-coding-agent --lib --no-fail-fast` gave
`FAILED. 1358 passed; 10 failed; 1 ignored` at ~13:39 and, on a re-run at 13:42,
`FAILED. 1361 passed; 7 failed; 1 ignored; finished in 14.59s`. The seven still
failing at 13:42 are the first six rows of the table below plus
`tui::view::tests::subagent_panel_groups_states_and_collapses_finished_workers_by_default`;
`tui::view::reasoning_render::…`, `tui::view::tests::queued_follow_up_heading_…`
and `update::progress::…` were fixed between my two runs. **This target is red at
the end of my pass.**

| Failing test | Panic |
| --- | --- |
| `app::bootstrap::tests::disabled_tools_are_absent_from_both_schema_and_execution_registry` (seen alongside) | `app/bootstrap/tests.rs:2850` — `left: ["read","ls","find","grep"] right: ["read"]` |
| `app::bootstrap::tests::tool_schema_reserve_is_positive_and_deterministic` | `app/bootstrap/tests.rs:3153` — `left: ["read","edit","write","bash","ls","find","grep"] right: ["read","edit","write","bash"]` |
| `app::bootstrap::tests::unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes` | `app/bootstrap/tests.rs:3111` — preflight did not project the fixture provider/model |
| `modes::interactive::clipboard_read::tests::a_real_helper_is_read_bounded_and_its_exit_status_is_honoured` | `modes/interactive.rs:1166` — `left: Failed right: Empty` |
| `modes::interactive::tests::active_session_commands_report_through_the_read_only_session` | `modes/interactive.rs:8017` — "export did not render a report" |
| `tui::pickers::tests::live_subagent_picker_refreshes_and_keeps_the_stable_selection` | `tui/pickers.rs:1841` — `left: Some("node-c") right: Some("node-b")` |
| `tui::view::reasoning_render::tests::activity_shimmer_highlight_is_measurably_visible_on_both_profiles` | `tui/view/reasoning_render.rs:1019` |
| `tui::view::tests::queued_follow_up_heading_advertises_the_platform_edit_hint` | `tui/view/tests.rs:13805` — `left: "  └ steering only · +1 more" right: "  └ local follow-up"` |
| `tui::view::tests::subagent_panel_groups_states_and_collapses_finished_workers_by_default` | `tui/view/tests.rs:990` |
| `update::progress::tests::actual_updater_progress_pty_and_plain_streams` | `update/progress.rs:386` |

Caveat and counter-caveat, honestly stated:

- Several of these files were **being edited by other workers at that moment**
  (`app/bootstrap.rs`, `lib.rs`, `tui/view/reasoning_render.rs`,
  `cli/parity.rs` were dirty), so those particular failures may be mid-edit noise.
- But two of them are **not** in any modified file and **reproduce
  deterministically in isolation**:
  `cargo test -p octet-coding-agent --lib -- --exact tui::pickers::tests::live_subagent_picker_refreshes_and_keeps_the_stable_selection modes::interactive::clipboard_read::tests::a_real_helper_is_read_bounded_and_its_exit_status_is_honoured`
  → `FAILED. 0 passed; 2 failed; 0 ignored; 1367 filtered out; finished in 0.06s`.
  `modes/interactive.rs` and `tui/pickers.rs` are **not** in `git status`.
- Therefore, at least those two are real failures of the committed HEAD, and the
  PR must not be cut until a clean re-run of this target is green.

This is the second, independent reason the "workspace is green" claim does not
hold: `cargo check` passes, but two of the four Rust test surfaces that were
actually run are red (`octet-ai`, `octet-coding-agent --lib`).

## Rust test surface — my own runs

All commands run with `--locked`. Timestamps are local (EDT).

| Command | Observed result | Verdict |
| --- | --- | --- |
| `cargo check --workspace --all-targets --locked` (13:13) | `Finished` in 15.47s, 0 `error` lines, 154 warnings (112 `is never used`) | VERIFIED |
| `cargo test -p octet-ai` (13:35) | lib: `351 passed; 0 failed`; then `tests/client_stream.rs`: `FAILED. 31 passed; 6 failed` (cargo fails fast and stops there) | RED at 13:35, green at 13:42 |
| `cargo test -p octet-ai --no-fail-fast` (13:42) | **every target ok**: 351, 5, 3, 18, 1, 38, 3, 2, 16, 4, 1, 1, 1 — 0 failures anywhere | GREEN now |
| `cargo test -p octet-ai --lib -- --skip reconnect_…` (13:19) | `FAILED. 346 passed; 4 failed; 1 filtered out`, all 4 in `responses_ws::tests` | RED at 13:19 |
| `cargo test -p octet-ai --lib` (13:33) | `ok. 350 passed; 0 failed; 1 filtered out` | GREEN now |
| `cargo test -p octet-ai --lib -- --exact responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` (13:34) | `ok. 1 passed … finished in 1.79s` | GREEN now (hung earlier) |
| `cargo test -p sexy-tui-rs --no-fail-fast` (13:35) | lib `190 passed; 0 failed`; every integration target `ok` (1,1,1,5,1,6,16,27,4 …); 0 `FAILED` | VERIFIED |
| `cargo test -p octet-coding-agent --test codex_context_window` | `ok. 14 passed` | VERIFIED |
| `… --test slash_command_pty` | `ok. 7 passed` (matches tui.md's "running 7 tests") | VERIFIED |
| `… --test activity_wait_pty` | `ok. 2 passed` | VERIFIED |
| `… --test setup_cli_acceptance` | `ok. 6 passed` | VERIFIED |
| `… --test setup_tui_acceptance` | `ok. 4 passed` | VERIFIED |
| `cargo test -p octet-agent --test parity_tools --test telemetry_conformance --test read_concurrency_current --no-fail-fast` (13:53) | `parity_tools` `ok. 23 passed`; `read_concurrency_current` `ok. 5 passed`; `telemetry_conformance` `ok. 9 passed` | VERIFIED |
| `cargo test -p octet-agent --test agent_run` | aborted: `websocket_connection_limit_is_retried_by_agent … FAILED` and `qualified_codex_ws_http_cumulative_twelve_attempt_envelope` ran >5 min (a `start_paused` loop asserting 20,200 `ProviderWaitingForNetwork` events, `agent_run.rs:7912`) | PARTIAL / RED |
| `cargo test -p octet-coding-agent --lib --no-fail-fast` (13:39 / 13:42) | `FAILED. 1358 passed; 10 failed` then `FAILED. 1361 passed; 7 failed` — see C9 | **RED (open)** |

Two hazards made the `octet-agent` half of this table hard to obtain, and both
are relevant to the PR:

1. **The `octet-agent` suite contains very long tests.** On my first run the lib
   target was still inside `effect::tests::file_tool_payload_contract_fits_streaming_intent_boundary`
   after >12 minutes (a pre-existing, *unmodified* test that builds two 32 MiB
   strings — `crates/octet-agent/src/effect.rs:1421`), and
   `agent::sustained_network_recovery_tests::qualified_presend_outage_waits_beyond_finite_budget_and_is_cancellable`
   printed "has been running for over 60 seconds" before passing. A later run of
   `tests/agent_run.rs` hit `qualified_codex_ws_http_cumulative_twelve_attempt_envelope
   has been running for over 60 seconds`. I did not get a clean end-to-end
   `cargo test -p octet-agent` result. **This is not proof of a hang** (unlike
   the `octet-ai` case, where the same binary ran >50 minutes); it is proof that
   the suite is slow enough that a naive CI job needs a large timeout.
2. **The shared tree stopped compiling mid-pass.** At ~13:45 both `octet-agent`
   and `octet-coding-agent` test runs aborted with
   `error[E0596]: cannot borrow \`pre_output\` as mutable … crates/octet-ai/src/responses_ws.rs:1032`
   and the same for `pre_output_bytes` at `:1033` — an *uncommitted, in-flight*
   edit by another worker. It compiled again by 13:47. Any "workspace is green"
   statement is only true between worker edits.

**C8 (WAS RED AT 13:35, FIXED BY 13:42) — pre-existing `client_stream.rs` tests were broken.**
`cargo test -p octet-ai --test client_stream` reproduced deterministically
twice at 13:35: `FAILED. 31 passed; 6 failed`. The same command at 13:42 gives
`test result: ok. 38 passed; 0 failed` — a worker fixed it while I was writing.
The table below is the state I measured; keep it so the fix is not credited to
luck.

| Failing test | Panic |
| --- | --- |
| `responses_websocket_connection_limit_retires_socket_and_falls_back` | `tests/client_stream.rs:914` — not `Some(Ok(StreamEvent::Started { .. }))` |
| `responses_websocket_failure_after_send_is_terminal` | `:1076` — error is not `AiError::Transport` with `phase == Body && !timeout` |
| `responses_websocket_failed_output_next_explicit_request_uses_full_http_replay` | `:1131` — same shape |
| `responses_websocket_heartbeat_timeout_after_created_is_terminal` | `:1181` — not `Started` |
| `responses_websocket_heartbeat_failure_is_terminal_and_next_request_falls_back` | `:1220` — not `Started` |
| `responses_websocket_pongs_do_not_extend_response_idle_timeout` | `:1298` — not `Started` |

Attribution is clear: `crates/octet-ai/tests/client_stream.rs` is **not touched**
by this diff (`git diff --stat df5a7e80 HEAD` lists only `src/client.rs` (+115)
and `src/responses_ws.rs` (+1639/−301)), and all six test names exist at base
(`git show df5a7e80:crates/octet-ai/tests/client_stream.rs`). So the websocket
rewrite changed behaviour those tests pinned. `docs/parity/codecs.md:131` cites
only the **lib** evidence (`cargo test -p octet-ai --lib responses_ws`, 25 tests)
and is therefore accurate as written — but the PR must say the integration
target `client_stream` is red, or fix it.

## Other suites (my own runs)

| Suite | Command | Observed |
| --- | --- | --- |
| octet-subagents | `python3 -m pytest extensions/octet-subagents/tests -q` | `76 passed, 61 subtests passed in 0.88s` |
| sdk/python | `PYTHONPATH=sdk/python python3 -m pytest sdk/python/tests -q` | `1 failed, 72 passed, 22 subtests` — environmental (see below) |
| octet-computer-use | `cd extensions/octet-computer-use && python3 -m unittest discover -s tests` | `Ran 93 tests … OK` |
| scripts | `python3 -m unittest scripts.tests.test_{changelog,source_archive,bench_pi_runtime,bench_render,bench_systems}` | `Ran 42 tests … OK` |
| catalog diff | `python3 -m unittest scripts.test_diff_model_catalog` | `Ran 10 tests … OK` |
| octet-import-aider | `python3 -m pytest extensions/octet-import-aider/tests -q` | `8 passed, 13 subtests` |
| octet-import-cline | `python3 -m pytest extensions/octet-import-cline -q` | `11 passed, 6 subtests` (the tests live at the package root, **not** in a `tests/` dir — `… /tests` collects nothing) |
| octet-import-pi | `python3 -m pytest extensions/octet-import-pi/tests -q` | `4 passed, 4 subtests` |
| extension API 0.3 | `python3 scripts/generate-extension-api-v03.py --check` | EXIT=0 |
| extension API 0.3 | `python3 -m pytest sdk/python/tests/test_theme_selection_api_v03.py -q` | `14 passed` |
| apps/web | `npm test` / `npx vitest run <2 files>` / `npx tsc -b` | see F1: `2 failed, 297 passed` under load; `5 passed` in isolation; tsc EXIT=0 |

**The sdk/python failure is environmental — proven, not asserted.**
`test_extension.py::CleanIdentityTests::test_previous_import_name_is_not_a_source_alias`
runs `python -S -c "import octet_extension; import ygg_extension"` with cwd
`sdk/python` and asserts a non-zero exit. `sdk/python/ygg_extension/` exists and
contains **only** a stale `__pycache__` (`__init__.cpython-314.pyc` dated Aug 23,
`extension.cpython-314.pyc` Sep 3 — all pre-dating this swarm), so Python 3
resolves it as a **namespace package** and the import succeeds:
`python3 -S -c "import ygg_extension"` → `OK _NamespacePath([.../ygg_extension])`.
`git status --porcelain -uall --ignored sdk/python/ygg_extension` shows
`!! …/__pycache__/*.pyc` (the directory is invisible to a normal `git status`
because everything in it is ignored). I copied the tree to `/tmp/v8/sdkcopy`,
deleted only that directory there, and re-ran:
`2 passed, 9 deselected`. So the failure is an artifact of a stale local cache
directory, **not** of this branch. Deleting `sdk/python/ygg_extension/` clears
it; no source change is needed.

## What I could not verify (and what would be needed)

- **`cargo test -p octet-agent --test agent_run` completion.** I captured the
  `websocket_connection_limit_is_retried_by_agent` failure, but the sibling
  `qualified_codex_ws_http_cumulative_twelve_attempt_envelope` ran for more than
  five minutes so I did not get a full target summary. Needed: a quiet machine
  and a large timeout.
- **A clean re-run of `cargo test -p octet-coding-agent --lib`.** My red result
  came from a working tree that other workers were actively editing. Needed: the
  same command on a frozen commit.
- **`cargo test -p octet-ai` at base (`df5a7e80`).** I did not build the base
  tree (it would need a separate `CARGO_TARGET_DIR` and a full compile), so I
  cannot say from my own run whether the 6 `client_stream` failures are new or
  pre-existing-but-newly-exposed. The tests and the file are untouched by the
  diff, which puts the burden on the author; it is not positive proof.
- **`npm run build`** for `apps/web` — not run (only `npx tsc -b`, EXIT=0).
- **Live/external qualification.** Windows CI, macOS, Terminal.app, tmux/herdr
  live panes and any real network call are unverified and unavailable here; the
  docs correctly mark them unqualified.
- **Fixed mid-pass, so my earlier observations are already stale:**
  the `octet-ai` lib failures + hang (C7: red at 13:19, green at 13:33);
  `CHANGELOG.md` (C5: empty at 13:13, +143 lines at 13:32);
  the committed LaTeX oracle (C4: tracked at 13:13; by 13:32 `.gitignore` adds
  `/crates/sexy-tui-rs/tests/_*.rs` and the five `_*` files are staged deleted).

## Remaining primitives for blocked rows

These are the concrete, code-level gaps named by the docs and confirmed against
the tree; they are what a follow-up needs, not restatements of "pending".

1. **`/fast` (roadmap #175).** A caller: `durable_responses_options` /
   `native_responses_options` in `crates/octet-agent/src/agent.rs` must set
   `ResponsesOptions::service_tier` on an endpoint whose
   `ResponsesRuntimeProfile::accepts_service_tier` is true.
   `apply_fast_command` (`modes/interactive.rs:1539`) already fail-closed.
2. **Codex websocket resumption in live runs.** `body_requests_storage`
   (`store: true`) is never set by the agent builders, so
   `client.rs` never installs a `ResponseResumer` and a post-output drop cannot
   resume (`docs/parity/codecs.md:133-143`). Same two builder functions.
3. **Codex per-request transport selection** (`sse` / `websocket` /
   `websocket-cached` / `auto`), an explicit connect deadline and debug stats —
   `1c.6` remains partial; selection is endpoint-declared today.
4. **`client_stream.rs` (6 tests) must be reconciled** with the new socket
   lifecycle before the row can be called landed (C8).
5. **Proxy seam (`1b.3`).** `crates/octet-ai/src/declarations/proxy.rs` has the
   resolver + tests; `client.rs:1891` (`reqwest::Client::builder()`) does not
   call it.
6. **Sampling-param / per-model-header merge (`1b.2`)** and per-request
   transformer hooks (`1b.1`) — data landed in `declarations/mod.rs`, merge into
   `protocol/*` and `client.rs` not wired.
7. **Tool-layer primitives with no consumer** (rows 4.7, 4.8, 4.10, 4.11, 4.12,
   4.13, 4.14): each needs its consumer — the live preview panel, the run loop's
   batch-termination check, the prompt builder, and the session's keyed
   replace/scan API for durable memos. Plus the undocumented ones in F3.

## Status table

Rows are the top-level claims a PR body would make. "Command run" is this
verifier's own invocation, not a worker receipt.

| Area | Claim under test | Observation | Command run | Verdict |
| --- | --- | --- | --- | --- |
| Workspace build | Whole workspace + all targets compile | `Finished` in 15.47s, 0 `error` lines (13:13). ⚠️ broke for ~2 min at 13:45 on an unfinished worker edit | `cargo check --workspace --all-targets --locked` | VERIFIED at 13:13 |
| Subagents policy | `unsupported_model` outlier rejected | Target test now passes; suite 76 passed / 61 subtests | `python3 -m pytest extensions/octet-subagents/tests -q` | VERIFIED (was CONTRADICTED) |
| Telemetry 3.5 | "Span boundaries NOT landed; no behavioural boundary test exists" (`docs/parity/telemetry.md:76-79`) | Spans are wired in the live generator and three boundary tests exist | `rg` over `agent.rs`/`delegation.rs`; see §2 | CONTRADICTED |
| Providers `/fast` | Codex `service_tier` "landed, unblocks roadmap #175 `/fast`" (`docs/parity/providers.md:40`) | `/fast` is inert by design; code itself says the caller is missing | `sed -n '1533,1560p' crates/octet-coding-agent/src/modes/interactive.rs` | CONTRADICTED |
| Editor LaTeX/Mermaid | Rows 2c.3 / 2c.4 blocked (`docs/parity/editor.md:125-141`) | Modules + tests are in HEAD | `ls crates/sexy-tui-rs/src/rich_text/`, `tests/latex_render.rs` | CONTRADICTED (stale) |
| Repo hygiene | No generated oracle committed | At 13:13 `_latex_diff.rs` (398,351 B) was tracked; **remediated by 13:32** — `.gitignore` now has `/crates/sexy-tui-rs/tests/_*.rs` and all five `_*` files are staged deleted | `git ls-files`, `git diff -- .gitignore` | CONTRADICTED → FIXED mid-pass |
| CHANGELOG | Parity items ship CHANGELOG entries | At 13:13 `## [Unreleased]` was empty; **by 13:32** `CHANGELOG.md` is `+143` lines with parity sections | `git diff --stat -- CHANGELOG.md` | CONTRADICTED → FIXED mid-pass |
| Extensions API 0.3 | Theme-selection surface landed | Primitive + Rust integration test + generated Python API; 14 py tests pass | `python3 scripts/generate-extension-api-v03.py --check`; `pytest sdk/python/tests/test_theme_selection_api_v03.py -q` | VERIFIED |
| sdk/python | 1 failing identity test | Environmental: stale `ygg_extension/__pycache__` makes an empty-of-source dir a namespace package | `pytest sdk/python/tests -q`; isolated copy test | VERIFIED (environmental) |
| computer-use | 93 tests OK | `Ran 93 tests ... OK` | `cd extensions/octet-computer-use && python3 -m unittest discover -s tests` | VERIFIED |
| scripts tests | 42 + 10 OK | `Ran 42 ... OK`, `Ran 10 ... OK` | `python3 -m unittest scripts.tests.test_changelog ...`; `scripts.test_diff_model_catalog` | VERIFIED |
| Dead code | No `todo!()`/`unimplemented!()`/`#[expect(dead_code)]` added | 0 hits | `rg 'todo!\(|unimplemented!\('`; `rg 'expect\(dead_code\)'` | VERIFIED |
| Secret handling | No secret/session-id in argv or display strings | Launcher builds argv lists, validates shell-safe tokens | `rg` over `launcher.py` | VERIFIED (§5) |
| Rust: octet-ai | per-crate test surface | `6 failed` at 13:35 → **every target ok at 13:42** | `cargo test -p octet-ai --no-fail-fast` | RED at 13:35 → FIXED 13:42 (C7/C8) |
| Rust: sexy-tui-rs | per-crate test surface | lib `190 passed; 0 failed`; all integration targets ok | `cargo test -p sexy-tui-rs --no-fail-fast` | VERIFIED |
| Rust: named pty/acceptance targets | 5 coding-agent targets | `codex_context_window` 14, `slash_command_pty` 7, `activity_wait_pty` 2, `setup_cli_acceptance` 6, `setup_tui_acceptance` 4 — all `ok` | `cargo test -p octet-coding-agent --test …` | VERIFIED |
| Rust: octet-agent targets | parity_tools / telemetry_conformance / read_concurrency_current | `23 passed`, `5 passed`, `9 passed` — all ok | `cargo test -p octet-agent --test … --no-fail-fast` | VERIFIED |
| Rust: agent_run | websocket recovery in the agent | `websocket_connection_limit_is_retried_by_agent` FAILED; a sibling test ran >5 min | `cargo test -p octet-agent --test agent_run` | CONTRADICTED |
| Rust: coding-agent lib | per-crate test surface | `1361 passed; 7 failed` at 13:42 (was `10 failed` at 13:39) | `cargo test -p octet-coding-agent --lib --no-fail-fast` | **CONTRADICTED — still red** (C9) |

## Final state at HEAD `7be2dc96` (13:43 EDT)

For a reader who only wants the bottom line:

- `cargo test -p octet-ai --no-fail-fast` — **all 13 targets green**.
- `cargo test -p sexy-tui-rs --no-fail-fast` — green (190 lib + integration).
- `cargo test -p octet-agent --test parity_tools --test telemetry_conformance --test read_concurrency_current`
  — green (23 / 9 / 5).
- `cargo test -p octet-coding-agent --test codex_context_window --test slash_command_pty --test activity_wait_pty --test setup_cli_acceptance --test setup_tui_acceptance`
  — green (14 / 7 / 2 / 6 / 4).
- `cargo test -p octet-coding-agent --lib --no-fail-fast` — **RED: 1361 passed, 7 failed**.
  This is the only red Rust surface at the end of the pass, and the two failures
  in unmodified files (`tui/pickers.rs:1841`,
  `modes/interactive.rs:1166`) are deterministic.
- Python / web suites as tabulated above; the only failure is the proven
  environmental `ygg_extension` namespace-package artifact, and `apps/web`'s two
  timeouts are load-induced.
- `cargo check --workspace --all-targets --locked` was clean at 13:13 but the
  tree did not compile at ~13:45 (in-flight worker edit) — **re-run the workspace
  check on the frozen commit before publishing.**

---

# Pass 4 — verify12c, 2026-09-15 19:02Z+, re-checked at committed HEAD `03c4c31c`

> Written one section at a time while the test batteries run, so a kill left
> partial truth rather than none. Every command below was run by me (`verify12c`)
> on this host. Where a claim was checked against committed content vs. the
> working tree I say which; see **Identity** immediately below, because the
> situation CHANGED since pass 3.

## Pass 4 — Identity: the "uncommitted wave" is now committed; the tree is not frozen

* `git log --oneline -1` → `03c4c31c vibe: wave 11 checkpoint` (author `octet vibe`,
  15:02:01 -0400). This commit *contains* the wave pass 3 measured as "dirty":
  `git show --stat 03c4c31c` lists `crates/octet-agent/src/agent.rs (856+/-)`,
  `crates/octet-coding-agent/src/{app/bootstrap.rs (765+/-), app/bootstrap/tests.rs
  (384+), modes/interactive.rs (468+/-), providers/auth.rs (554+/-), tui/view.rs
  (763+/-), tui/view/panel_render.rs, tui/view/reasoning_render.rs,
  tui/view/shell_chrome.rs, tui/view/status_telemetry.rs, tui/view/transcript_render.rs}`,
  `crates/sexy-tui-rs/src/rich_text/{markdown.rs,latex/mod.rs}`,
  `crates/sexy-tui-rs/tests/rich_fences.rs (+220)`, plus the parity docs.
  **So most of pass 3's "worktree (WT)" verdicts are now claims about committed
  content** — the `WT`/`HEAD` split pass 3 drew has collapsed for those files.
* **But the tree was NOT frozen during this pass.** My first `git status
  --porcelain` at 19:03Z showed 3 modified files (audit docs only); at 19:04Z it
  showed **`M crates/octet-agent/src/agent.rs`** and **`M
  crates/octet-coding-agent/src/providers/auth.rs`** as well. Two live edits
  landed *while my test battery was running*: `+91` lines in `agent.rs`, `+48/-20`
  in `auth.rs` (`git diff HEAD --stat`), the auth hunks inside its test region.
  Command: `git status --porcelain`; `git diff HEAD --stat`. **Every test result
  below must be read as "worktree at ~19:03–19:05Z with those two files in
  flight", not as a frozen revision.** The one thing this does not excuse is a
  failure in a file that is *not* dirty — see the two red suites below.

## Pass 4 — Tool surface (requested check 1): CONFIRMED GREEN

| # | Claim | My observation | Command | Verdict |
| --- | --- | --- | --- | --- |
| V1 | Exactly the maintainer's narrow set | `host.tool(...)` appears 6 times in `CoreTools::register`: ReadTool, EditTool, WriteTool, BashTool, SearchTool, PowerShellTool (`#[cfg(windows)]`). Nothing else registered. | `rg -n "host\.tool\(" crates/octet-agent/src/tools/mod.rs` → lines 162,163,164,165,168,171 | VERIFIED |
| V2 | No `ls.rs`/`find.rs`/`grep.rs` | Directory is exactly `bash.rs deferred.rs durability.rs edit.rs mod.rs powershell.rs read.rs search.rs shell_environment.rs summarization.rs write.rs` | `ls crates/octet-agent/src/tools/` | VERIFIED |
| V3 | No re-added equivalent under another name | The only `impl Tool for` in the directory: `SearchTool` (search.rs:68), `SessionShellTool` (shell_environment.rs:82, internal), `ReadTool` (read.rs:84), `WriteTool` (write.rs:33), `BashTool` (bash.rs:72), `CheckpointedBashTool` (bash.rs:704, internal), `PowerShellTool` (powershell.rs:10), `EditTool` (edit.rs:75). No glob/list/tree/find alias. | `rg -n "impl Tool for" crates/octet-agent/src/tools/` | VERIFIED |
| V4 | No dangling `LsTool`/`FindTool`/`GrepTool`/`GlobTool`/`ListTool`/`RipgrepTool` | Zero hits in the whole repo outside `docs/swarm-audit/**` and this file | `rg -n "LsTool|FindTool|GrepTool|GlobTool|ListTool|RipgrepTool" --glob '!docs/swarm-audit/**' --glob '!docs/parity/VERIFICATION.md' .` → no output | VERIFIED |
| V5 | `search` still shells out to ripgrep by default | `SearchTool::execute` → `self.execute_with_program(args, ctx, std::path::Path::new("rg"))` at `search.rs:225` (argv, no shell). The words `ls`/`find`/`grep` appear in `search.rs` only as the `glob` *argument name* and in test fixtures, never as a tool name. | `rg -n 'Path::new\("rg"\)' crates/octet-agent/src/tools/search.rs`; `rg -n '"ls"\|"find"\|"grep"\|"glob"' crates/octet-agent/src/tools/*.rs` | VERIFIED |
| V6 | Ledger rows 4.1/4.2/4.3 agree | `docs/parity/README.md:114-116`: "Withdrawn by maintainer decision; behaviour served by ripgrep-backed `search`" — matches the code above | `rg -n "4\.[123]" docs/parity/README.md` | VERIFIED |
| V7 | CHANGELOG does not claim a `ls`/`find`/`grep` tool | No `[Unreleased]` bullet claims those tools; the only `grep`-adjacent text is the ripgrep-backed `search` description | `rg -n -i "service tier\|service_tier\|/fast" CHANGELOG.md` (and reading the file) | VERIFIED |

**Tool surface: no contradiction found.** This is the requested check 1 answered
affirmatively, at both committed `03c4c31c` and the worktree (the three files
involved are not dirty).

## Pass 4 — `#175` `/fast` (requested check 2): field present, still NO consumer

| # | Claim | My observation | Command | Verdict |
| --- | --- | --- | --- | --- |
| V8 | `service_tier` is set only when the resolved profile accepts it | `resolve_service_tier` (`crates/octet-agent/src/agent.rs:3786`) returns `Ok(None)` for `None`, else fails closed with `AiError::Unsupported(UnsupportedError::ServiceTier)` unless `model.spec.protocol == Protocol::OpenAiResponses` **and** `model.endpoint.runtime.responses_profile.accepts_service_tier()` (`crates/octet-ai/src/types.rs:218`, which is `matches!(self, Self::Codex)`). Both live builders call it and only attach the tier on `Some`: `durable_responses_options` (`:3756`,`:3770`), `native_responses_options` (`:3807`,`:3821`); `responses_prewarm_request` (`:5798`) reuses the durable builder. 34 `service_tier` references in `agent.rs`, matching the count in the task. | `rg -n "service_tier" crates/octet-agent/src/agent.rs`; `sed -n '3780,3830p' crates/octet-agent/src/agent.rs` | VERIFIED (agent API) |
| V9 | `/fast` actually changes the wire | **It does not, and the code says so.** `apply_fast_command` (`crates/octet-coding-agent/src/modes/interactive.rs:1644`) never calls `Agent::set_service_tier`; on a Codex route it prints "`/fast on` not applied: this build's Codex request path does not send a service tier yet … nothing changed on the wire", and on any other route "`/fast` is only available on Codex Responses routes". | `rg -n "set_service_tier" crates/` → definition (`agent.rs:6505`) + agent tests only; `rg -c "service_tier" crates/octet-coding-agent/src/**` → the only hits are that doc comment, a leak-list fixture and a test helper | CONTRADICTED as a user-visible capability; the *honesty* of the message is VERIFIED |
| V10 | Docs match V9 | `docs/parity/providers.md` states "the remaining missing primitive is the caller … `/fast on` changes nothing on the wire; the accurate status is 'agent API ready, UI consumer pending'" and names the exact call site `interactive.rs:1644`. `CHANGELOG.md:75` says the same: "Add the Codex `service_tier` request field. (*The `/fast` command remains …*)" | reading `docs/parity/providers.md:59-110`; `rg -n -i "service tier\|service_tier\|/fast" CHANGELOG.md docs/parity/README.md` | VERIFIED (docs do not overclaim) |

**`/fast`: this is the correct shape of "landed primitive, missing consumer".**
The primitive is real and gated; the user-visible switch is inert and both the
docs and the runtime message say so. Nothing here should be described as a
working `/fast`.

## Pass 4 — fence consumer (requested check 4): REAL, not a dead arm — and one oracle is now BETTER anchored than the earlier passes said

| # | Claim | My observation | Command | Verdict |
| --- | --- | --- | --- | --- |
| V11 | The consumer is real, not a match arm nobody reaches | `rich_text/markdown.rs` has 7 `render_latex`/`render_mermaid` references: module doc (:5,:6), `use` (:22,:23), and three live call sites inside `render_diagram_fence` (:45 `latex` → `render_latex(code.trim(), RenderLatexOptions::display())`, :46 `mermaid` → `render_mermaid(code).ok()?.plain()`, :58 `graph|flowchart` → header re-injection + `render_mermaid`). It is reached from the parser, not only from tests: `rg -n "render_diagram_fence" markdown.rs` → the definition plus the `Frame::Code` dispatch, gated on `fence_is_closed`. | `rg -n "render_latex\|render_mermaid" crates/sexy-tui-rs/src/rich_text/markdown.rs`; reading `render_diagram_fence` | VERIFIED |
| V12 | Unknown fence stays a plain code block | `render_diagram_fence` lowercases only the first info word and `_ => return None` for everything else; `None` keeps the original `CodeBlock`. Test `unknown_fences_are_never_reinterpreted` + `unknown_and_alias_fences_never_reach_a_diagram_renderer` (15 alias info strings) **pass in my run**. | `cargo test -p sexy-tui-rs --test rich_fences` (19:03:17Z) → `15 passed; 0 failed` | VERIFIED |
| V13 | Unsupported body degrades to source without a panic | `render_latex(...)?` / `render_mermaid(code).ok()?` → `None` on any typed error; an empty render also returns `None` (`if rendered.trim().is_empty()`). Tests `unsupported_bodies_degrade_to_the_original_source`, `renders_that_produce_nothing_keep_the_original_source`, `oversized_and_unterminated_fences_stay_literal`, `streaming_keeps_failed_diagram_fences_as_source` — all pass. | same command | VERIFIED |
| V14 | `--test latex_render` = 17, `--test mermaid_render` = 11 | My own run: `latex_render 17 passed`, `mermaid_render 11 passed`, `rich_fences 15 passed`; 0 failed in all three. (Pass 3 recorded `rich_fences` = 10; wave 11 grew it to 15.) | `cargo test -p sexy-tui-rs --test rich_fences --test latex_render --test mermaid_render` | VERIFIED |
| V15 | `docs/parity/editor.md` does not overclaim Mermaid | §2c.4 explicitly lists the **unsupported** set — other diagram types, `BT`/`RL` "rejected rather than misrendered", `subgraph`/`end`/`direction`, `&` node lists, `A -- text --> B`, other arrow tokens, unbalanced brackets, cycles, edges that skip a layer, and the `MAX_MERMAID_*` caps — and repeats it under "Not modelled" (shape outlines, link stroke styling, subgraphs, `BT`/`RL`, HTML-entity decoding emitted literally, the unrendered-diagram fallback). `mermaid.rs` agrees: `STATEMENT_KEYWORDS_IN_UNSUPPORTED = ["subgraph","end","direction"]` (`:242`) and the module doc lists `&` node lists as unsupported. | reading `docs/parity/editor.md:196-290`; `rg -n '"BT"\|"RL"\|subgraph\|is not supported' crates/sexy-tui-rs/src/rich_text/mermaid.rs` | VERIFIED (no overclaim) |
| V16 | Goldens: which ones are self-captured | **Not all of them — and the difference matters.** `tests/mermaid_render.rs:1-5` is honest: "the renderer is self-contained (no upstream Rust equivalent and no network dependency), so the goldens are captured from the implementation". But `tests/latex_render.rs:1-20` claims an **external** oracle: "Every expected value in this file was captured by running the upstream reference implementation `packages/tui/src/latex.ts` (pi @ `8a7b0c03…`)", naming `UPSTREAM_SUITE` (the upstream `latex.test.ts` corpus), `UPSTREAM_OBSERVED`, `DISPLAY_LAYOUT_CORPUS`, `INLINE_LAYOUT_CORPUS` and a Node differential harness. | reading both test headers | PARTIAL — see V17 |
| V17 | The LaTeX external oracle's precondition actually exists on this host | `/Users/achumukundan/github/earendil-works/pi` exists, is a git checkout, and `git rev-parse HEAD` there is **exactly** `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`; `packages/tui/src/latex.ts` and `packages/tui/test/latex.test.ts` both exist. So the pinned reference the goldens claim to come from is real and at the pinned revision — the claim is *not* unbacked. (I did not re-run the Node differential harness; see remaining primitives.) | `ls ~/github/earendil-works/pi`; `git rev-parse HEAD` in it; `ls packages/tui/{src/latex.ts,test/latex.test.ts}` | VERIFIED (precondition), harness re-run UNVERIFIED |

## Pass 4 — test surface (my own runs, 19:03–19:04Z, worktree with `agent.rs`+`auth.rs` dirty)

| Command (all mine) | Observed | Verdict |
| --- | --- | --- |
| `cargo test -p sexy-tui-rs --test rich_fences --test latex_render --test mermaid_render` (19:03:17Z) | 15 / 17 / 11 passed, 0 failed | VERIFIED |
| `cargo test -p octet-ai --no-fail-fast` (19:03:38Z) | **13 targets ok, 0 failures, 30.6s**, no hang: 366, 5, 3, 18, 1, 38, 3, 2, 16, 4, 1, 1 and 1 doc-test. This matches the task's "`octet-ai` must be green with no hang" requirement. | VERIFIED |
| `cargo test -p octet-agent --lib --no-fail-fast` (19:03:51Z) | **546 passed; 1 FAILED; 1 ignored** — `agent::tests::partial_output_checkpoints_pace_bound_and_never_claim_completion`, `crates/octet-agent/src/agent.rs:10741`: "the newest bytes are the ones kept" | RED (file in flight) |
| `cargo test -p octet-coding-agent --lib --no-fail-fast` (19:04:24Z) | **1386 passed; 27 FAILED; 1 ignored** — full list in the next section | RED |
| `cargo test -p octet-agent --test parity_tools --test telemetry_conformance` (19:06:07Z) | 17 passed / 9 passed, 0 failed | VERIFIED |
| `cargo test -p octet-agent --test delegation` (19:06:37Z) | 11 passed, 0 failed | VERIFIED |
| `cargo test -p octet-coding-agent --test codex_context_window --test slash_command_pty --test activity_wait_pty` (19:06:44Z) | **never ran**: `error[E0425]: cannot find function collapsed_subagent_groups_row in this scope --> tui/view.rs:1437:24`; `error: could not compile octet-coding-agent (lib) due to 1 previous error`. The symbol exists at `view.rs:1388` in the very next save, so this was a **mid-save snapshot of a live edit**, not a defect. Recorded because it proves no claim about those three suites can be made from this pass. | BLOCKED (transient) |
| `cargo test -p octet-coding-agent --test setup_cli_acceptance --test setup_tui_acceptance` (19:07:09Z) | 6 passed / 4 passed, 0 failed | VERIFIED |
| `cargo test -p octet-agent --test agent_run --no-fail-fast` | started 19:07Z; status recorded in the last section of this pass | see final section |

## Pass 4 — startup latency + binary identity (requested check 3): penalty REPRODUCED on the shipped binary, ABSENT on the freshly built one

Measured by me at ~19:15Z on this host, `/usr/bin/time -p <bin> doctor`, stdout to
`/dev/null`:

| Binary | identity | plain | `AWS_EC2_METADATA_DISABLED=true` |
| --- | --- | --- | --- |
| `~/.local/bin/octet` (installed) | 24,143,088 B, sha256 `96b138b0…`, mtime **Sep 12 14:54** | **1.96s, 1.01s** | 0.00s, 0.00s |
| `target/debug/octet` (checkout) | 144,305,768 B, sha256 `f37f86bf…`, mtime **Sep 15 15:07 local** | 0.45s (first, cold), **0.01s, 0.01s** | 0.01s, 0.01s |

| # | Claim | My observation | Command | Verdict |
| --- | --- | --- | --- | --- |
| V18 | The ~1s AWS metadata penalty is real in the shipped artifact | `~/.local/bin/octet` pays **1.0–2.0s** plain and **0.00s** with `AWS_EC2_METADATA_DISABLED=true`. Same direction and magnitude as pass 3 (1.75/1.80 vs 0.029). | `/usr/bin/time -p ~/.local/bin/octet doctor` | VERIFIED (defect real in shipped binary) |
| V19 | Binary identity, not version strings | `strings -a ~/.local/bin/octet` contains `AWS_EC2_METADATA_DISABLED` and "AWS metadata response exceeds the byte limit" but **no** `OCTET_AWS_METADATA_CREDENTIALS` — consistent with a build that predates the opt-in fix. `strings -a target/debug/octet` **does** contain `OCTET_AWS_METADATA_CREDENTIALS` (the opt-in marker). The installed binary is byte-identical to pass 3's measurement (same size + `96b138b0…`), so it still predates the fix; the debug binary is **new** (pass 3 saw 143,888,360 B / `df552ab6…` / 14:17) and was rebuilt at 15:07 local from this wave. | `stat -f%z`; `shasum -a 256`; `strings -a <bin> \| grep -iE "aws_metadata\|OCTET_AWS"` | VERIFIED |
| V20 | The fixed behaviour is demonstrable at runtime | On the **fresh** debug binary the plain and metadata-disabled runs are **the same (0.01s)** — no sustained metadata probe. On the installed binary they differ by 1–2s. So the fix is real *in this checkout's build*, and only the shipped artifact still pays. | timing table above | VERIFIED (in checkout; the shipped binary on PATH is NOT fixed) |
| V21 | Acceptance tests use request counters, not millisecond thresholds | `providers/auth.rs` test region contains **no** `Instant::now`, `Duration::from_millis` or `elapsed()` at all; it counts with `Arc<AtomicUsize>` (`:1566, :1732, :1785-1786, :1921, :1973, :2148, :2178, :2469, :2562`) and asserts exact counts (`:1919, :2166, :2229, :2240, :2360`). `disabled_activation_opens_no_connection_to_a_live_metadata_endpoint` (`:1818`) is explicit: "No sleeps and no millisecond thresholds are used". | `rg -n "AtomicUsize\|Instant::now\|Duration::from_millis\|elapsed\(\)" crates/octet-coding-agent/src/providers/auth.rs` | VERIFIED |
| V22 | Waits cancel/settle with no blocked lock worker | Code present: `REFRESH_LOCK_WAIT = 3s` (`auth/codex/store.rs:25`), consumed under `CODEX_READINESS_ENVELOPE = 10s` (`app/bootstrap.rs:5122`, used at `:5056,:5204`); `run_route_readiness` passes `REFRESH_LOCK_WAIT` as the lock timeout. The behavioural tests for the contended case are `refresh_lock_waits_on_the_private_credential_directory` (`store.rs`) and `a_contended_refresh_lock_fails_closed_and_leaves_no_phantom_holder`; both are inside the `octet-coding-agent --lib` suite, whose full-parallel run was red (below) and whose isolated re-run is recorded in the final section. | code read; `rg -n "REFRESH_LOCK_WAIT\|CODEX_READINESS_ENVELOPE" crates/` | PARTIAL (code VERIFIED, run pending) |
| V23 | "Fresh valid Codex cache triggers zero inventory discovery" | The named test is `offline_codex_registration_uses_cached_inventory_without_dynamic_capabilities` (`app/bootstrap/tests.rs:1281`, task also cites `:1312`). **It failed in my full-suite run** at 19:04:24Z — `UnknownModel(ModelId("cached-account-model"))`, i.e. the cached inventory was not registered — and then **passed 3/3 when re-run in isolation** at 19:09:59Z and 19:10:01Z. Not a clean pass; not a clean defect. | `sed -n '1281,1315p' …/bootstrap/tests.rs`; targeted re-runs | CONTRADICTED in the parallel suite, VERIFIED in isolation |

### Pass 4 — the `--lib` cluster failure is partly test-interference, and I proved it

The 27 failures at 19:04:24Z could have been a real regression or a broken
suite. I re-ran the non-TUI half of them, alone, in the 90 seconds after:

```
provider::auth::tests::disabled_activation_opens_no_connection_to_a_live_metadata_endpoint
  run 1 (19:08:xx) ok · run 2 (19:09:04Z) ok · run 3 (19:09:45Z) ok        → 3/3 PASS
app::bootstrap::tests: narrowed_readiness_never_consults_an_unrelated_provider,
  readiness_plan_narrows_only_a_proven_route,
  offline_codex_registration_uses_cached_inventory_without_dynamic_capabilities
  run 1 (19:09:59Z) 3 passed · run 2 (19:10:01Z) 3 passed                    → 3/3 PASS
```

So four of the twenty-seven are **flaky under the full parallel suite and green
in isolation** — including the AWS-metadata control test, whose own structure
explains it: it drains a nonblocking loopback listener with **no sleep** and
asserts the control connection was drained (`auth.rs:1835 assert_eq!(drained, 1,
"the control connection must be observed")` → observed `0`). That is a
test-harness race, not a metadata-probe defect. The same caveat applies to the
`update::progress` PTY test, which failed on a 2-second subprocess timeout while
two other cargo test runs were saturating the machine.

**Consequence for the PR body:** "`cargo test -p octet-coding-agent --lib` is
green" cannot be claimed from any run in this pass. The accurate statement is:
*1386 passed / 27 failed in a full parallel run at 19:04Z; of the four I
re-ran alone, all four passed; the remaining 23 are all in `tui::view*` /
`status_telemetry` and were still red when their own source file started being
edited at ~19:05Z.*

## Pass 4 — TUI items in flight (requested check 5): two landed, two contradicted, one unchanged

Snapshot hashes for every verdict in this section (md5, mtime local): `tui/view.rs`
`21bbd9ab…` 15:12:43 · `tui/view/tests.rs` `8e7463e0…` 15:13:23 ·
`tui/view/status_telemetry.rs` `f6c39661…` **15:00:36 (not in `git status` at any
point in this pass → committed content)** · `octet-agent/src/agent.rs`
`970d6b07…` 15:09:03 · `providers/auth.rs` `d015179a…` 15:11:29.

| # | Item | Claim | My observation | Verdict |
| --- | --- | --- | --- | --- |
| V24 | `/goal` mid-run (the reported bug) | Must take effect mid-run, not queue to the idle boundary | **LANDED.** `queue_command` now refuses it (`interactive.rs:763` `Command::Goal(_) => anyhow::bail!("`/goal` is applied immediately and is never queued as an idle action")`), and the active-run dispatcher applies it in place (`interactive.rs:2028` `Command::Goal(goal) => match inspection.goal_access() { Ok(access) => apply_goal_command(access, shell, goal, goal_deadline) … Err(error) => shell.error(...) }`, comment: "Applied now … The run in progress is untouched"). Idle path still owns `/goal` (`:5092`). Four tests cover it and **passed** in my 19:04 full-suite run: `goal_commands_have_no_queued_form` (`:9670`), `active_goal_objective_mutates_the_durable_store_without_queueing` (`:9684`), `active_goal_commands_apply_while_the_run_is_still_streaming` (`:9730`), `active_goal_pause_resume_and_clear_are_coherent_mid_run` (`:9897`). **Pass 3's P8 ("NOT fixed") is stale — this was fixed by wave 11.** | `rg -n "PendingIdleAction::Goal\|Command::Goal" crates/octet-coding-agent/src/modes/interactive.rs`; `sed -n '755,770p;2020,2040p;5085,5095p;9670,9706p'` | VERIFIED (contradicts the task brief and pass 3) |
| V25 | Silent startup: phase label removed | `starting extensions` must not be painted | Gone from rendering: `rg -c "starting extensions" modes/interactive.rs` → **0**; the only surviving occurrence in the tree is inside the *forbidden* list of the acceptance test (`status_telemetry.rs:334`), i.e. a negative assertion, not a painted label. | `rg -n "starting extensions" crates/octet-coding-agent/src/` | VERIFIED |
| V26 | Silent startup: typeable composer + atomic first frame | `silent_startup_paints_a_blank_typeable_composer_and_one_ready_frame` passes | **RED in two independent runs** (19:04:24Z full suite, 19:11:09Z single-threaded): `status_telemetry.rs:352` `assert_eq!(shell.pending(), "x draft during startup z")` → left `"x draft during startupz"`. The test feeds `Char('x')` + `Paste(" draft during startup")` + `Char('z')` (`:288-295`), which **cannot** produce the expected string: the expected literal contains one more space than its own input sequence. `status_telemetry.rs` is not in `git status`, so this is committed content, and no run in this pass made it pass. | `sed -n '928,956p' /tmp/v12c-iso.log`; `sed -n '288,295p;334,352p' crates/octet-coding-agent/src/tui/view/status_telemetry.rs` | CONTRADICTED (broken acceptance test / no green evidence) |
| V27 | Footer + telemetry cost: plain dollar, no `subtotal`/`+`/`~` | Both surfaces | **Half landed, and internally contradicted.** The telemetry panel is plain dollars (`status_telemetry.rs:188-190` asserts the forbidden list `["subtotal","+","~","?","unknown"]`), but the **composer footer still prints `subtotal <n> + ?`** (`tui/composer_surface.rs:804`, file clean/committed) and the committed test at `view/tests.rs:14062-14063` asserts exactly that (`footer.contains("subtotal")`, `footer.contains("+ ?")`) — those two assertions **passed** inside the failing test at 19:11Z, so the footer wording is confirmed by execution, not just by reading. | `sed -n '798,812p' crates/octet-coding-agent/src/tui/composer_surface.rs`; iso log panic at `tests.rs:14065` (the `subtotal` asserts were passed to reach it) | CONTRADICTED for the footer; VERIFIED for the telemetry panel |
| V28 | Uncertainty is never rendered as zero | `provider_usage_uncertain_never_infers_zero_from_pricing` / `..._survives_success_settlement_and_resume` | **RED in two independent runs, and the failure is a product regression, not a stale expectation:** `tests.rs:14107` `assertion failed: !telemetry.contains("$0")` — the telemetry panel renders a dollar zero while provider usage is uncertain; and `tests.rs:14065` `assertion failed: telemetry.contains("totals unknown")` — the uncertainty wording is gone from the panel. Both tests are in `view/tests.rs` (in flight) but they encode the documented invariant "uncertain usage is never rendered as zero", which wave 11's `status_telemetry.rs` change broke. | iso log lines 989-997; `sed -n '14098,14110p' crates/octet-coding-agent/src/tui/view/tests.rs` | CONTRADICTED (real regression at the snapshot) |
| V29 | Shimmer: model-adaptive, no rainbow outside max/ultra, real rest gap | `ACTIVITY_SWEEP_REST_FRAMES`, `status_rainbow_strength_at` | Code + tests present and **no `reasoning_render` test failed in either run**: `ACTIVITY_SWEEP_REST_FRAMES = 2` (`reasoning_render.rs:63`, consumed `:55`, `:1955`), rainbow gate exercised at `:1864-1870` (all levels + `Duration::ZERO` for `max`), rest-gap assertion `rest_frames >= ACTIVITY_SWEEP_REST_FRAMES` at `:2092`. | `rg -n "ACTIVITY_SWEEP_REST_FRAMES\|status_rainbow_strength_at" crates/octet-coding-agent/src/tui/view{,/reasoning_render}.rs`; failure lists of both runs | VERIFIED at 19:04/19:11 — but **three `reasoning_render` colour-rotation tests turned RED at 19:15:31Z** while that file was being edited (`both_status_ramps_apply_the_same_hue_rotation`, `near_neutral_model_colours_rotate_only_as_far_as_their_own_chroma_allows`, `working_and_thinking_share_one_hue_family_and_differ_by_brightness`), so the shimmer surface is green-then-red in this pass |
| V30 | Subagents activity settles into the transcript, not replayed per prompt | `settled_subagent_workers` + test | `settled_subagent_workers` has 6 references in `view.rs`; the named test **`a_settled_subagent_roster_never_replays_under_a_later_prompt` failed in the 19:04 parallel run and PASSED single-threaded at 19:11:09Z** (`/tmp/v12c-iso.log:646 ... ok`), and a second test `the_settled_subagent_block_stays_at_the_point_the_delegation_happened` (`tests.rs:11023`) now exists. | grep + both logs | PARTIAL (green only in the isolated run, on a mid-rewrite snapshot) |
| V31 | Subagent transcript/panel rendering | Presumed green after the wave-11 panel redesign | **RED single-threaded: 8 `subagent_transcript`/`subagent_panel` tests failed at 19:11:09Z** — e.g. `assert_eq!(rows.len(), 4)` vs 5 actual, `assertion left: 0 right: 2`, column-parsing mismatches (`left: ["└ 審査", …] right: ["審査", …]`), wrapped failure text truncated to `"Failed: spawn reje"`. The renderer and its test helpers are being rewritten in lockstep (test names themselves changed between my two runs: `aligns_unicode_names_by_visible_width` → `aligns_columns_by_visible_width`, `failure_wraps_at_the_worker_indent` → `failure_wraps_at_the_group_indent`), so this cluster is **unfinished work in flight**, not a settled result either way. | `/tmp/v12c-iso.log:1047-1062`; earlier `/tmp/v12c-battery.log:3666-3695` | CONTRADICTED at both snapshots (mid-rewrite) |

## Pass 4 — the 88-row ledger (requested check 6): states I cannot defend

The ledger has **exactly 88 numbered rows** (`awk -F'|' '/^\| [0-9]/' docs/parity/README.md | wc -l` → 88).
State distribution as read: **26 `Landed`**, 14 `Unverified`, 14 `Pending`,
8 `Verified`, 3 `Withdrawn by maintainer decision`, 1 `In progress`, and 22 rows
whose 4th column carries a qualifier (`Partial; …`, `Landed; …`, `Pending; …`).

I did **not** re-audit all 88 row-by-row (pass 2 did, at `df2e8980`, and its
per-row dispositions are above). What I re-checked at this head:

| # | Row | Claim under test | My observation | Verdict |
| --- | --- | --- | --- | --- |
| V32 | 4.1/4.2/4.3 | "Withdrawn by maintainer decision; behaviour served by ripgrep-backed `search`" | Code matches (see V1–V7). Withdrawal is accurate and consistent with the maintainer's instruction. | VERIFIED |
| V33 | 4.6 | "Partial; opt-in gating landed, Windows CI evidence hardware-gated" | Honest: it names the missing evidence instead of claiming it. `PowerShellTool` is `#[cfg(windows)]` (`tools/mod.rs:171`), so on this macOS host the Windows path is genuinely unexercised. | VERIFIED as an honest state |
| V34 | 6.1 | "Landed (12 topic pages; 239 relative links, 0 unresolved; receipt in repo-tooling.md)" | Bounded doc claim with a named receipt; not re-verified by me (would need a link resolver run). | UNVERIFIED (link count not re-run) |
| V35 | any row whose 4th column is bare `Landed` (26 rows) | "code plus a behavioural test that was actually run" | This is where the risk is concentrated. Given V26/V28 are red **Landed-adjacent** items for the same wave (silent startup, uncertainty rendering), and 8 subagent-rendering tests are red at the same head, I cannot defend a blanket reading of `Landed` for rows in the TUI/telemetry area. Rows whose `Landed` claim covers a UI surface touched by wave 11 (`view.rs`, `status_telemetry.rs`, `reasoning_render.rs`, `panel_render.rs`, `transcript_render.rs`) should be re-read as **"code landed; suite red at 19:04–19:12Z"**. | verdict tables above | Qualified: state not defensible from this pass |
| V36 | 2c.3 / 2c.4 (edited this wave) | LaTeX/Mermaid rows | Both green in my own run (17 + 11 tests) and the consumer is real (V11–V17). These two are the strongest rows in the ledger. | VERIFIED |

**No row claims live/external evidence I could falsify.** The one place a row could
have overclaimed — row 4.6 — explicitly says the Windows CI evidence is
hardware-gated. Row 6.1's "239 relative links, 0 unresolved" is the only numeric
doc claim I left unverified.

## Pass 4 — CHANGELOG (requested check 7): one stale Known-gaps bullet, one bullet that is true only because the code lives in Python

| # | Claim | My observation | Verdict |
| --- | --- | --- | --- |
| V37 | Known gaps: "the `octet-ai` test suite is **red and partly non-terminating** at this checkpoint following the Codex websocket rewrite; this is being repaired" (`CHANGELOG.md:138-140`) | **Contradicted by measurement.** My `cargo test -p octet-ai --no-fail-fast` at 19:03:38Z finished in 30.6s with **all 13 targets green (0 failed)** and no hang; pass 3's pinned previously-hanging test finished in 1.76s. The session's stated requirement ("`octet-ai` must be green with no hang") is met, so this bullet is stale and is the single clearest CHANGELOG/reality mismatch in the file. | CONTRADICTED (stale) |
| V38 | `[Unreleased]` §Subagents: "Add `/subagents open-all tmux` and `/subagents open-all herdr` … passes ids as separate argv elements with no shell interpolation; never places credentials in argv" | **True, but only because the implementation is Python.** `rg -n "open-all\|open_all" crates/` → no hits; the implementation is `extensions/octet-subagents/octet_subagents/launcher.py` (`plan_open_all` `:356`, `execute_plan` `:539`), documented in `docs/subagents.md:72,79,110,148,190` and probed in `docs/parity/AUDIT-security.md` (S1: stub `tmux` on PATH records `"$@"` as separate argv elements). A Rust-only search would have produced a **false** "capability claimed, code absent" accusation — worth stating explicitly, because this repo's parity work spans Rust + Python. | VERIFIED (with the Python caveat); no live tmux/herdr run is claimed by the doc |
| V39 | `[Unreleased]` preamble promises "`Landed` below means code plus a behavioural test that was actually run" | The definition is honest, but the CHANGELOG's own `[Unreleased]` bullets include surfaces whose tests are red at this head (silent startup, uncertainty rendering, subagent transcript). The bullets are *capability* claims and most are still true; the ones that are not are named in V26/V28. | PARTIAL |
| V40 | Known gaps: "`cargo check --workspace --all-targets --locked` is clean" (line 138) | Not re-run by me in this pass; my runs found a transient `E0425` in `view.rs` at 19:06:44Z (mid-edit) and clean compiles before/after. | UNVERIFIED (transient break observed) |

## Pass 4 — the rest of the requested surface, and the whole truth about what is red

### Suites I ran (every number is mine)

| Suite | Result | When (UTC) | Verdict |
| --- | --- | --- | --- |
| `sexy-tui-rs --test rich_fences --test latex_render --test mermaid_render` | 15 / 17 / 11 passed, **0 failed** | 19:03:17Z | GREEN |
| `octet-ai` (all targets, `--no-fail-fast`) | 13 targets, 366+5+3+18+1+38+3+2+16+4+1+1+1 passed, **0 failed**, 30.6s, no hang | 19:03:38Z | GREEN |
| `octet-agent --test parity_tools --test telemetry_conformance` | 17 / 9 passed, 0 failed | 19:06:07Z | GREEN |
| `octet-agent --test delegation` | 11 passed, 0 failed | 19:06:37Z | GREEN |
| `octet-coding-agent --test setup_cli_acceptance --test setup_tui_acceptance` | 6 / 4 passed, 0 failed | 19:07:09Z | GREEN |
| `octet-coding-agent --test codex_context_window --test slash_command_pty --test activity_wait_pty` | 14 / 7 / 2 passed, **0 failed** (the 19:06 attempt was blocked by a mid-save `E0425`; the retry at 19:14:37Z was clean) | 19:14:37Z | GREEN |
| `octet-coding-agent --lib` | **1419 passed / 15 FAILED** (was 1386/27 at 19:04:24Z; the cluster shrank *while I measured*, because `view.rs`+`view/tests.rs` are being repaired) | 19:15:31Z | RED |
| `octet-coding-agent --lib`, `tui::view` only, `--test-threads=1` | 417 passed / **10 FAILED** | 19:11:09Z | RED (isolated, not interference) |
| `octet-agent --lib` | **548 passed / 0 FAILED** / 1 ignored (was 546/1 at 19:03:51Z) | 19:16:03Z | GREEN now |
| `octet-agent --test agent_run` | started 19:07Z, **still running at 19:17Z** (190 results so far) with `websocket_connection_limit_is_retried_by_agent ... FAILED` | — | INCOMPLETE |

The 15 failures at 19:15:31Z, verbatim and complete (extracted from the `failures:`
block that precedes `test result: FAILED. 1419 passed; 15 failed`):

```
app::bootstrap::tests::unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes
modes::interactive::clipboard_read::tests::a_real_helper_is_read_bounded_and_its_exit_status_is_honoured
modes::interactive::tests::active_session_commands_report_through_the_read_only_session
tui::pickers::tests::live_subagent_picker_refreshes_and_keeps_the_stable_selection
tui::view::reasoning_render::tests::both_status_ramps_apply_the_same_hue_rotation
tui::view::reasoning_render::tests::near_neutral_model_colours_rotate_only_as_far_as_their_own_chroma_allows
tui::view::reasoning_render::tests::working_and_thinking_share_one_hue_family_and_differ_by_brightness
tui::view::status_telemetry::tests::silent_startup_paints_a_blank_typeable_composer_and_one_ready_frame
tui::view::tests::full_tui_colour_modes_preserve_readable_content_and_supported_encoding
tui::view::tests::live_workers_for_the_current_turn_still_open_a_block
tui::view::tests::provider_usage_uncertain_never_infers_zero_from_pricing
tui::view::tests::provider_usage_uncertain_survives_success_settlement_and_resume
tui::view::tests::subagent_transcript_aligns_columns_by_visible_width
tui::view::tests::subagent_transcript_bounds_a_large_roster_and_keeps_live_workers_visible
update::progress::tests::actual_updater_progress_pty_and_plain_streams
```

**The set is not stable between my runs, and that is itself the finding.** Between
19:04:24Z (27 failures) and 19:15:31Z (15) the following *disappeared*, i.e. were
repaired while I measured: `subagent_transcript_groups_live_workers_and_collapses_terminal_groups`,
`subagent_transcript_failure_wraps_at_the_group_indent`, `subagent_panel_keyboard_cycles_the_state_filter_and_the_row_ordering`,
`a_settled_subagent_roster_never_replays_under_a_later_prompt`, and the whole
`subagent_stability_tests` pair. The following *appeared*: three
`reasoning_render` colour-rotation tests, `tui::pickers::live_subagent_picker_refreshes_…`,
`modes::interactive::clipboard_read::…`, `modes::interactive::tests::active_session_commands_…`,
`app::bootstrap::tests::unknown_api_03_…` (the last four had passed in the earlier
full run). A suite whose member set changes by a dozen names in eleven minutes can
only be reported as a snapshot, never as a verdict.

## Pass 4 — contradictions (each side quoted)

1. **`cargo test -p octet-coding-agent --lib` is not green.**
   *Claim side* (CHANGELOG `[Unreleased]` preamble): "`Landed` below means code plus
   a behavioural test that was actually run". *Observation*: `test result: FAILED.
   1419 passed; 15 failed` (19:15:31Z), and 10 of those fail single-threaded.
2. **The silent-startup acceptance test cannot pass, and never has in my runs.**
   *Claim*: "the `starting extensions…` label is gone … with a typeable composer and
   an atomic first frame". *Observation*: `status_telemetry.rs:352`
   `assert_eq!(shell.pending(), "x draft during startup z")` → left
   `"x draft during startupz"`. Its own inputs (`Char('x')`, `Paste(" draft during
   startup")`, `Char('z')`) cannot produce the expected string. The label *is* gone
   (V25); the composer/first-frame evidence is not.
3. **Uncertain usage is rendered as `$0`.**
   *Claim* (CHANGELOG): "Record cost and usage above 272K as uncertain rather than
   exact". *Observation*: `assertion failed: !telemetry.contains("$0")`
   (`view/tests.rs:14107`) and `assertion failed: telemetry.contains("totals
   unknown")` (`:14065`), both in two independent runs. This is the one failure in
   this pass that looks like a **product** regression rather than a stale test.
4. **The composer footer still speaks the old cost language.**
   *Claim*: "footer/telemetry cost a plain dollar estimate with no
   'subtotal'/'+'/'~'". *Observation*: `composer_surface.rs:804`
   `format!("subtotal {} + ?", format_microdollars(cost))`, and the committed
   assertion `assert!(footer.contains("subtotal"))` at `view/tests.rs:14062`
   **passed** inside a test that failed later on a different line. The telemetry
   panel half of the claim is true (`status_telemetry.rs:188-190`).
5. **CHANGELOG "Known gaps" is stale about `octet-ai`.**
   *Claim* (`CHANGELOG.md:138-140`): "the `octet-ai` test suite is **red and partly
   non-terminating** at this checkpoint". *Observation*: all 13 `octet-ai` targets
   green in 30.6s, no hang (19:03:38Z). "Known gaps" does not match reality.
6. **The wave-11 subagent panel has no green evidence at any snapshot I took.**
   *Claim* (CHANGELOG §Interaction): "Redesign the `/subagents` panel: group by
   state with counts, collapse terminal groups by default, aligned columns with a
   header row". *Observation*: 22 of the 27 failures at 19:04Z and 8–10 of the 15
   at 19:15Z are exactly those rendering tests, with the renderer and the test
   helpers being rewritten in lockstep (test names changed mid-pass).
7. **The shipped binary still pays the AWS metadata penalty.**
   *Claim* (task/roadmap): startup latency fixed. *Observation*: installed
   `~/.local/bin/octet` = 1.96s/1.01s plain vs 0.00s disabled; the fix is present
   in the checkout build only (`OCTET_AWS_METADATA_CREDENTIALS` in
   `target/debug/octet`, absent from the installed binary).
8. **A transient compile break existed at 19:06:44Z.**
   `error[E0425]: cannot find function collapsed_subagent_groups_row --> tui/view.rs:1437`
   while the symbol sat at `view.rs:1388` in the next save. Not a defect — but it
   means the three PTY suites could not be run at that instant.
9. **Test-suite interference is real and inflates the failure count.**
   Four tests that failed in the full parallel run — one `octet-agent` checkpoint
   test, the AWS-metadata control test, and three `app::bootstrap` readiness tests —
   passed 3/3 and 3/3 when re-run alone. The metadata test explains itself: it
   drains a nonblocking loopback listener with no sleep and asserts
   `drained == 1` (`auth.rs:1835`). Any "N failures" claim about this repo's lib
   suites must state the thread count and load.
10. **Local, gitignored probe tests live in a test directory.**
    `crates/sexy-tui-rs/tests/_*.rs` = **15 files** (`_latex_diff.rs` alone is
    398 KB), ignored by `.gitignore:54` so they are not part of the PR, but cargo
    auto-discovers them: a bare `cargo test -p sexy-tui-rs` compiles and runs them.
    Not a shipping defect; it does mean local green/red can differ from CI.

## Pass 4 — overall status

* **Tool surface**: green and exactly as the maintainer asked (6 registrations, no
  `ls`/`find`/`grep`, ripgrep by default, no dangling references, ledger rows
  4.1–4.3 agree). No contradiction found.
* **`#175` `/fast`**: the agent-side primitive is real and gated; the user-visible
  command is deliberately inert and says so, and `providers.md`/`CHANGELOG` do not
  overclaim. Correctly labelled "UI consumer pending".
* **Startup latency**: the defect is real in the shipped artifact and reproducible
  on demand; the fix is real in the checkout build; the acceptance tests count
  requests, not milliseconds. Green in isolation.
* **Fence consumer**: real, reached from the parser, degrades safely, 43 tests green
  across the three suites; `editor.md` §2c.4 is a model of honest scoping; the
  LaTeX oracle's pinned upstream checkout exists at the exact revision it names.
* **TUI wave**: `/goal` mid-run (the reported bug) **is fixed** with four tests
  green. Silent startup's label is gone but its acceptance test is red and, as
  written, unfalsifiable in the passing direction. The subagent panel and
  uncertainty/cost surfaces are **red at this head** and being rewritten as I
  measured — they are not finished, and no `Landed` claim in that area is
  defensible from this pass.
* **Ledger / CHANGELOG**: 88 rows, honest qualifiers on the blocked ones; the one
  clearly stale artefact is the `octet-ai` "Known gaps" bullet.

## Pass 4 — remaining primitives (exact preconditions)

1. **A frozen snapshot.** Commit the wave (or check out a fixed revision) and re-run
   `cargo test -p octet-coding-agent --lib` — as of 19:15:31Z it is 1419/15 on a
   moving tree, and any PR claim needs a number that does not move under it.
2. **`cargo test -p octet-agent --test agent_run`** did not finish in this pass
   (started 19:07Z, still running at 19:17Z, one failure so far:
   `websocket_connection_limit_is_retried_by_agent`). It needs one uninterrupted run.
3. **The `tui::view` subagent cluster** needs a single-threaded green run *after*
   the renderer and its helpers stop changing (10 red at 19:11:09Z).
4. **`reasoning_render::tests::working_and_thinking_share_one_hue_family_and_differ_by_brightness`**
   flipped from green (19:04Z, 19:11Z) to red (19:15:31Z) while
   `reasoning_render.rs` was being edited — needs one run on settled content.
5. **The LaTeX differential harness** (the claim behind `tests/latex_render.rs:1-20`)
   was not re-run; it needs Node plus the pinned `~/github/earendil-works/pi` tree,
   which does exist at `8a7b0c03…`.
6. **`cargo check --workspace --all-targets --locked`** (CHANGELOG line 138) was not
   re-run by me; it needs a quiet tree.
7. **Ledger row 6.1** ("239 relative links, 0 unresolved") needs the link resolver run.
8. **Hardware/authority-gated evidence is unchanged**: Windows PowerShell execution,
   live `tmux`/`herdr` panes, native Firefox/Safari, physical terminal paint. The
   `open-all` implementation is real (`extensions/octet-subagents/octet_subagents/launcher.py`)
   but every probe of it in this repo stubs the multiplexer on `PATH`.
