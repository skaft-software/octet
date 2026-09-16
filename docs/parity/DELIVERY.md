# Local delivery and verification handoff

## Current 0.8.0 candidate — in progress

**Not completed Pi parity, release approval or an all-green candidate.** The
[88-row ledger/current overlay](README.md) preserves requested scope; the
[current verification](VERIFICATION.md#current-candidate--qualification-in-progress)
separates source readiness from bounded tests, missing implementation and native/
live gates. [PR body](PR_BODY.md) remains a local draft only.

### Latest bounded handoff receipts

**Latest attempts failed during compilation, then in repair:** `parity-next-check-11.log`
exited 101 solely on the in-progress kernel's missing `auxiliary_settlement_tests`
module; `parity-next-export-tests.log` exited 101 before tests on a
`BusEventParams` initializer missing `binding_id`. Every subsequent failure was
diagnosed and repaired; those logs are preserved, not relabeled.

All stems refer to `/tmp/octet-final/<stem>.log` and matching `.exit` files.
Exact commands and superseded failures are in current verification; do not sum
these overlapping runs or infer CLI/PTY coverage from a library pass.

| Stem | Observed result |
| --- | --- |
| `f2-check` | Workspace all targets/features, locked/offline: exit 0; warnings remain. Frozen snapshot `freeze-final.sha256`, zero files changed during the run. |
| `f2-coding` | Every coding-agent target: exit 0; **1703 passed, 0 failed** across 30 targets. |
| `f2-agent` | Every agent target: exit 0; **819 passed, 0 failed**. |
| `f2-ai` | Every AI target: exit 0; **500 passed, 0 failed**. |
| `parity-next-coding-lib-04` | Exit 0; **1521 passed, 0 failed, 1 ignored**. |
| `parity-next-agent-lib-05` | Exit 0; **570 passed, 0 failed, 1 ignored**. |
| `parity-next-agent-int-05` | `--no-fail-fast`, exit 0; every agent target ran: **819 passed, 0 failed**. |
| `parity-next-ai-parity-04`, `parity-next-ai-all` | Exit 0; `provider_parity` 28 passed and the whole crate/targets ran. |
| `parity-next-renderer-04` | Exit 0; **277 passed** (fresh rerun). |
| `parity-next-sdk-python` | Exit 0 after repair; **109 passed**. |
| `parity-next-pi-bridge-tests-02` | Exit 0 after repair; **88 run / 85 passed / 3 real-runtime skips**. |
| `parity-next-scripts-02` | Exit 0 after repair; **58 passed**. |
| direct | TypeScript API 0.3 conformance **47 fixtures**; generator `--check` clean after regenerating a stale runtime artifact. |
| `parity-next-extension-bundle-02` | Exit 0; seven tests passed after directory-link/member repair. API 0.2/0.3 source-override packaging, not a public release. |
| `parity-next-coding-all` (preserved failure) | Whole coding target set exit 101: embedded-documentation mismatch from an archive built before same-run edits, plus the adapter's published host pin. Both repaired; `parity-next-coding-lib-06` passes the documentation assertion on a fresh build, and `migration_import` now stages a private copy while asserting the tracked pin. |

Superseded failures kept verbatim: coding 1503/9/1 and 1519/2/1; agent library 564
and 559 with `agent_run` 148/4 then 154/1; AI `provider_parity` 26/2; Pi bridge 36
failures; initial compile and check09/11/export-test failures; one `eval_harness`
load-sensitive failure that passed four later standalone runs. Snapshot manifests
`parity-next-wave3-source.sha256` (1635 files) and `parity-next-wave5-source.sha256`
bound what those reruns actually covered. SDK distribution metadata stays 0.7.6,
and the four tracked official bundles keep their published `=0.7.6` host pins.

### Handoff still required

- **Qualified in the wave-3 rerun:** kernel audit F3 (completed invocation
  identity cannot reopen as pending), F4 (wave-scoped invocation admission for
  ≥65-call turns), F6 (accepted auxiliary result settles before the guard is
  disarmed), F5 kernel side (`AgentError::OutputLimitUnavailable` fails closed
  for cap-omitting routes under either hard ceiling), AI F7 sampling allowlist
  plus explicit `stop` precedence, immutable RPC turn cost, `TurnFinished.turn_cost`,
  extension-bus SDK lifecycle repairs, and the F8 export/typed-media repairs.
  Each is covered by the receipts above, including tests added for the specific
  defect. Not qualified by earlier passes merely because older consumers compiled.
- **F2 — protocol ACKed, SDK/Rust repaired, product evidence incomplete:** the
  binding-scoped `bus/lifecycle` contract, pending-vs-active subscribe results and
  SDK rebinding now work in the real two-process fixtures, and the SDK's
  tagged-union parsing defect is fixed. Surviving-peer product recovery evidence
  (a real active-session switch A→B→A with both processes surviving) is still
  outstanding, so no full F2 closure is claimed.
- **Current CLI/process tests:** `parity_cli`, `eval_harness`,
  `slash_command_pty`, `activity_wait_pty`, `setup_cli_acceptance`, `pi_install`
  and `migration_import` still need candidate receipts. Source now has ordered
  scope/command consumers, rich HTML and real local-loopback opt-in model eval;
  neither earlier scripted eval nor coding-library passes qualify these targets.
- **Missing implementation:** PiMessages/radius, exact current catalogs and
  remaining codec/deferred/image/metadata depth; `/settings`, `/scoped-models`,
  `/debug`, bookmark tree and generic terminal layout/mouse/alternate-screen
  parity; scoped extension theme/render identity, full Pi host control/UI and
  tool-result usage/termination; all-writer ownership; native Firefox/Safari.
  These are not hardware-only gates. Ownerless migration notifications and
  extension-owner custom compact instructions remain integration limitations.
- **Safety boundaries unchanged:** `open-all` refuses all product pane effects;
  `/fast` now has consumers but retains durable uncertainty and the 272K Codex
  policy. Host bus delivery exists but F2 rebinding/ingress fencing is open.
  Trust/OAuth, clipboard-image, auto-download and transport exclusions remain.
- **Independent acceptance:** physical terminal/native clipboard/Windows/Apple,
  live providers/MCP/billing, actual Pi runtime/SRI, real multiplexer ownership,
  exhaustive Serve/companion security and signed/public release qualification.
  Earlier receipts below do not newly qualify those gates.

Exact implementation reports and audit findings:
`/tmp/octet-final/{ai-parity,kernel-parity,editor-parity,fast-consumer,ext-host,pi-bridge,model-eval,parity-audit}.md`.
`release-v0.8.0-confirmed` remains an **older preserved binary**, not evidence for
newer source. No installed-binary replacement, publication, SDK version bump,
Cargo execution or global formatting was performed in this documentation refresh.

## Historical frozen handoff

The original handoff below is retained verbatim, including its bounded passes,
failed formatting receipt, disk observations and then-missing consumers. Its
“completed”, “final” and “ready” wording is historical, not current qualification.
Use the current section above where later source/receipts supersede it.

Review candidate against `df5a7e809715961b9344af6b52e43a6ca48f56b3` (v0.7.6),
on `vibe/pi-parity-roadmap-df5a7e80`. Pi reference remains read-only at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`.

**This is a locally reviewed, tested delivery, not completed Pi parity, a release
approval, a pushed branch, or a remote PR.** The [88-row ledger](README.md) and
[independent review](VERIFICATION.md#final-review--final-audit-post-crash) retain
partial, excluded and unverified work. [PR body](PR_BODY.md) is ready for review.

## Priority outcomes

- `/goal` and the tested inspection/picker slash commands work during an actually
  held streaming response; no inference is needed to inspect or change the goal.
- Startup remains editable without the extension-starting label; context notes
  are deferred until terminal teardown. Narrow, modeless, model-switch, resize,
  terminal-reply and shutdown PTYs pass.
- Shimmer traverses the dot and every label cell, rests at the seam, preserves
  neutral identities and a shared model-derived hue family across appearances.
  Unit/PTY coverage is not physical-terminal acceptance.
- Footer costs are plain dollars. Durable uncertain usage, resumed accounting,
  NOOP/InMemory outcomes, hard budgets and the deliberate 272K Codex policy remain
  intact. Overrides cannot exceed the authenticated discovered maximum.
- Completed workers settle once into the owning transcript block without replay
  on later turns. History anchors, no-color output and stable picker selection
  have regression coverage.
- Review repairs include host-budgeted WebSocket retries, delegated usage
  reconciliation, durable spawn identity, bounded partial journals, all-session
  ephemeral accounting/recovery, MCP credential isolation, and read-only delegated
  Serve export. An unavailable theme service is no longer offered as implemented.

## Completed parent-run Rust checks

Logs and exit receipts are local under `/tmp/octet-final/`; each stem below names
both `.log` and `.exit`. Tests ran serially between Cargo invocations with two
build jobs, incremental/debug symbols disabled, and four test threads. No counts
below add overlapping reruns. Warnings remain.

| Check | Receipt | Result |
| --- | --- | --- |
| Workspace, every target and feature, locked | `workspace-check-final` | `cargo check` passed |
| AI, every target and feature | `ai-confirmed` | 464 passed |
| Agent library | `agent-lib-confirmed` | 556 passed, 1 ignored |
| Agent integrations, all features | `agent-integrations-final` | 230 passed, 1 ignored; includes 146 agent-run tests |
| Coding-agent library, all features | `coding-lib-final-receipt` | 1569 passed, 1 ignored |
| Critical CLI/Codex integrations | `cli-critical` | 37 passed, including real two-session ephemeral RPC |
| Activity/slash/setup/reply/shutdown PTYs | `tui-pty-final` | 37 passed |
| Startup PTYs | `startup-pty-short`, `startup-redraw-matrix` | 14 + 1 passed; latter exercises 30 geometry/mode cases |
| Provider integrations | `coding-integrations-providers` | 43 passed; stale inventory-count assertion failed, subsequently repaired and passed below |
| Host integrations and repaired provider contract | `coding-integrations-host` | 19 passed |
| Delegated resume, migration, eval, Pi install | `coding-integrations-migration` | 18 passed |
| Renderer and migration types, every target/feature | `renderer-migration-final` | 294 passed |
| Workspace documentation tests, all features | `doctests-final` | 6 passed |
| Remaining binaries/examples | `binaries-examples-final` | 3 targets built and ran, zero tests |
| Separate Serve workspace | `serve-final` | 284 passed |

Every coding-agent integration target has a completed passing target receipt;
there is **no single uninterrupted whole-workspace test-command pass**. Initial
ENOSPC and tool-timeout runs are not passes. The startup matrix alone takes about
92 seconds, so it was separated from the other startup tests. Intermediate
failures and a test-only mutex-guard deadlock were repaired and rerun; their old
logs remain diagnostic history. A docs comparison also caught edits occurring
during compilation; the final library receipt was taken after those edits settled.

## Other observed checks

- Final resource-embedding tests passed (28), and the refusal fixture passed after
  preserving its SSE event delimiter with a trailing comment. These are reruns,
  not additional tests in the aggregate counts above.
- A committed-source archive was produced and inspected: generated build trees
  are absent and all three importer entrypoints retain executable modes. This
  qualifies source packaging, not the API 0.3 extension release packager.
- Generated API 0.3 and Pi provider-compatibility `--check` gates passed. Final
  packaged-docs gate passed for 453 public files and 43 extra references, including
  producer-byte, relative-link and negative-boundary checks (`packaged-docs-final`).
- Web: 299 tests across 35 files; lint, typecheck, build, font, external-request and
  bundle checks passed.
- Repository script unit suite: 51 passed; offline installer harness passed
  (11 UI, 18 version, 3 cancellation/descendant scenarios).
- Provider acceptance 39, metadata refresh 4, release metadata gate 13, repository
  identity 10, catalog diff 10, Windows release metadata 6, panic policy 15 passed.
- Both Cargo lockfiles passed cached `audit --no-fetch` and offline `deny` checks,
  with allowed unmaintained-dependency warnings; not a fresh advisory-DB review.
- [Extension review](REVIEW-extensions.md) records the actual Python/Node/SDK
  commands, passing counts and skipped native/live cases. Subagents' 88 tests
  include repeated product open-all requests with zero pane effects.
- `cargo fmt --all -- --check` **fails**: latest receipt `fmt-final` contains 90
  unique files / 683 hunks, including baseline drift and changed files. No global
  formatting or blanket baseline waiver was applied.
- An unpinned network pricing-refresh check reported stale live models.dev prices.
  It is not reproducible pinned-source evidence; no generated JSON was hand-edited.

## Explicit remaining limits

Product open-all refuses **all** parent/worker pane launches until the host has
atomic writer claim/settlement. Direct tmux/herdr adapters and opaque handles do
not implement ownership transfer. `/fast` has an agent service-tier primitive but
no enabled UI consumer. Importer entrypoints now have executable Git modes, while
the extension release packager still admits only API 0.2, not their API 0.3.

Missing runtime consumers and features remain itemized in the ledger: PiMessages/
radius, request overrides and proxy integration, grammar-call decode/replay,
provider-specific depth, editor bindings/prompt jumps, durable invocation/checkpoint
consumers, deferred provider lifecycle, rich HTML rendering, real model-backed eval,
host bus/theme services and further roadmap work. Native Firefox/Safari also lacks
implementation; it is not merely waiting for a hardware test.

Windows/native/live-provider/MCP, real multiplexer handover, Apple signing and
hardware, physical terminal paint, exhaustive Serve/companion security review and
signed/public release qualification remain unverified. The trust/OAuth policy,
clipboard-image, auto-download and transport exclusions are unchanged. No roadmap
issue was closed remotely.

## Disk safety and repository hygiene

Only regenerable Swift `.build` trees and the duplicate standalone Serve target
were physically removed, recovering about 1.7 GiB after the earlier root target
had already disappeared. Untracking 2690 generated Swift files separately repairs
the delivery; untracking alone did not free disk. Scratch renderer tests were
preserved outside Cargo discovery in `/tmp/octet-final/scratch-tests`.

The sole build supervisor checks free space and stops its owned process group
below 25 GiB. Serve shares the root target, build jobs are capped at two, and
incremental/debug artifacts are disabled. At final verification about **108 GiB
was free**, with the shared target approximately 7 GiB. Source, Git history/index,
credentials, sessions and test evidence were not deleted. No install replaced the
running assistant binary.
