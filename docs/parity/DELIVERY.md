# Local delivery and verification handoff

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
incremental/debug artifacts are disabled. At final verification about **110 GiB
was free**, with the shared target approximately 6 GiB. Source, Git history/index,
credentials, sessions and test evidence were not deleted. No install replaced the
running assistant binary.
