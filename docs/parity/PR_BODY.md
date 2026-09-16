# Review candidate: additive Pi work, runtime fixes and honest qualification

## Current draft — qualification in progress

**Pi parity is not achieved; not ready for release approval.** Local candidate
source adds real editor/search/scrollbar and command consumers, tier-aware cost
work, durable invocation/checkpoint/retry consumers, an API 0.3 host bus, rich
HTML and explicitly opted-in local-model eval. Source readiness is not full
behavioral/upstream parity. [All 88 scope rows and current overlay](README.md)
remain explicit; no missing row is silently dropped.

### Current evidence

Parent receipts in `/tmp/octet-final/`, over the stable
`parity-next-wave3-source.sha256` snapshot (1635 files): workspace
all-target/all-feature `parity-next-check-12` **exit 0**; coding library
`parity-next-coding-lib-04` **1521 passed, 1 ignored**, exit 0; agent library
`parity-next-agent-lib-05` **570 passed**, and every agent target
`parity-next-agent-int-05` **819 passed, 0 failed**, exit 0; AI
`parity-next-ai-parity-04` **28 passed** with the whole crate green
(`parity-next-ai-all`); renderer **277 passed**; Python SDK **109 passed**; Pi
bridge **88 run / 85 passed / 3 real-runtime skips**; scripts **58 passed**;
TypeScript conformance **47 fixtures**; generator `--check` clean after
regenerating a stale runtime artifact. A fresh whole coding-agent/agent target
rerun (`parity-next-coding-all-07`, `parity-next-agent-all-08`) covers
`parity-next-wave5-source.sha256`.

Superseded failures remain preserved in the same directory rather than deleted:
coding library 1503/9 and 1519/2, agent library 564 and 559 with `agent_run`
148/4 then 154/1, AI `provider_parity` 26/2, Pi bridge 36 failures, two
in-progress compile failures, and one `eval_harness` failure under concurrent
load that passed four later standalone runs. Each was traced to a concrete cause
(bridge optional-method map missing `bus/lifecycle`; SDK calling `from_wire` on
generated tagged unions; host bus fixtures predating binding-scoped dispatch;
missing `Started` events in three stream fixtures; a 65-call fixture reusing
content-block index 64; a bench harness unable to resolve the fixture dependency
it no longer ships; three stale result expectations) and repaired in source.

**Now covered by the wave-3 rerun:** kernel F3/F4/F6, AI F7 sampling allowlist
and explicit `stop` precedence, F5 enforceable-cap refusal
(`AgentError::OutputLimitUnavailable`), immutable RPC turn cost and
`TurnFinished.turn_cost`, extension-bus SDK lifecycle repairs, and the F8 export
privacy / typed-media HTML repairs — each with tests added for the specific
defect. **F2 remains open for product evidence:** binding-scoped
`bus/lifecycle`, pending-vs-active subscribe results and SDK rebinding work in
the real two-process fixtures, but no real active-session A→B→A switch with both
processes surviving has been captured.

**Local candidate caveat:** the four tracked official bundles keep their
published `requires_octet = "=0.7.6"` pins (only `octet-subagents` was repinned
to the local `=0.8.0`), so this local candidate does not load published bundles.
Tests that need current-source behavior stage private copies carrying the
current version while asserting tracked pins are unchanged. SDK/public
distribution metadata stays 0.7.6; no release, installation or publication is
part of this draft.

Candidate CLI/eval/PTY/Pi-install/migration process qualification remains pending.
Failed/interrupted logs and formatting failures are retained; no all-green or
monolithic workspace-test pass.
Exact commands, limitations and owner reports:
[current verification](VERIFICATION.md#current-candidate--qualification-in-progress),
[delivery](DELIVERY.md#current-080-candidate--in-progress).

### Remaining work and unchanged boundaries

Missing implementation includes PiMessages/radius/current catalogs and remaining
codec/deferred/image/metadata depth, `/settings`/`/scoped-models`/`/debug`/bookmark
tree, generic terminal layout/mouse/alternate-screen parity, scoped theme/render
identity, full Pi host control/UI/tool-result accounting, all-writer ownership,
and native Firefox/Safari. These are **not merely hardware gates**. Separately,
physical-terminal/native/Windows/Apple/live-provider/MCP, real Pi runtime/SRI,
security and signed/public-release acceptance remain open.

`open-all` still refuses every product pane launch. `/fast` has consumers but
keeps durable uncertainty and the 272K Codex policy. API 0.3 packaging fixtures
now pass, but do not authorize publication or qualify unchanged released example
installation. Trust/OAuth, clipboard-image and auto-download exclusions remain.
`release-v0.8.0-confirmed` is an **older preserved binary**, not current source
qualification; no installation, publication or SDK distribution version bump
(SDK remains 0.7.6). No remote PR/push is authorized by this local draft.

## Historical draft — preserved, not current qualification

The original summary and receipts below belong to the earlier frozen handoff.
Its green counts and then-missing consumers are not current status; use the
current draft above. History is retained for review rather than rewritten.

## Summary

- Fix mid-run `/goal` and slash surfaces, silent/editable startup, shimmer
  sweep/rest and neutral colors, plain-dollar costs, and completed-worker
  transcript settlement without later-turn replay.
- Repair retry/accounting, ephemeral multi-session recovery, delegated identity,
  extension credential/capability boundaries and read-only Serve export.
- Add bounded provider/codec/tool/editor/CLI/rendering primitives and tests;
  distinguish working consumers from incomplete primitives in the 88-row ledger.
- Remove 2690 generated Swift build files from Git; reject generated build trees
  in source archives, preserve executable importer modes and cap local build growth.

## Verification

Workspace check passed with `--all-targets --all-features --locked`. Completed
shards include AI (464), agent library/integrations (556/230), coding library
(1569), all coding integration targets, renderer/migration (294), workspace doc
tests (6), separate Serve (284), web (299), scripts (51), and extension/SDK suites.
The mid-stream goal PTY and 30-case startup redraw matrix passed. Three Rust tests
remain ignored across the named library/integration suites. Exact commands,
receipts, skipped live cases and superseded failures: [delivery](DELIVERY.md),
[independent review](VERIFICATION.md#final-review--final-audit-post-crash).

There is no single uninterrupted full-workspace test pass. Initial disk-full and
timed-out commands are not passes. Formatting remains red (90 files / 683 hunks,
both existing drift and changed files); no global reformat or blanket waiver.
Cached audit and offline deny passed for both lockfiles, with documented warnings.

## Not delivered / not qualified

- **Pi parity is not achieved.** See [all 88 statuses](README.md), including request
  overrides, codec/provider depth, product editor bindings, durable consumers,
  real model-backed eval, rich HTML export and host extension services.
- Product `open-all` is fail-closed for every pane until atomic writer claim/
  settlement exists. Direct adapters are not live session handover.
- The `/fast` command is wired end to end and keeps its deliberate durable
  `responses-priority-tier` uncertainty marker; the marker is still not retired,
  because the audit's worst-case reservation/restart matrix is not yet captured
  as its own receipt.
- The extension release packager now admits API `0.2` **or** `0.3` with seven
  passing source-override packaging tests. That is source repair: no official
  release archive was produced, the four tracked bundles keep their published
  `=0.7.6` host pins, and the interactive `/fast` flag is not claimed as
  qualified behavior.
- Native/Windows/Apple/live-provider/MCP, physical-terminal, exhaustive Serve/
  companion security and signed/public-release gates remain open. No new trust,
  brokered OAuth, clipboard-image or automatic-download policy is introduced.

## Review / rollback

Base: `df5a7e809715961b9344af6b52e43a6ca48f56b3` (v0.7.6). Branch:
`vibe/pi-parity-roadmap-df5a7e80`. Domain reviews are linked from the independent
report. This is a large additive branch; review by domain rather than treating a
passing aggregate check as feature completion. Rollback requires normal review
of these local commits; no history rewrite, installed-binary replacement or
remote publication has been performed.

This file is a **draft PR body only**. Creating or pushing a remote PR requires
separate approval.
