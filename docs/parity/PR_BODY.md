# Review candidate: additive Pi work, runtime fixes and honest qualification

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
- `/fast` UI remains unavailable. API 0.3 importer release packaging remains
  blocked by the API 0.2-only packager.
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
