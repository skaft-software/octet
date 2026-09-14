# Repair-native stack result

**Status:** complete provisional source-only integration. The four frozen inputs were verified and applied in manifest order; no conflict or unexpected source change occurred.

## Integrated order

1. **Luna reasoning fixture assertion repair** — tests only. Both model-reference calls use `&luna`; the existing `select_auxiliary_reasoning(...).unwrap()` is preserved, and wire expectations are `Some("none".to_owned())` / `Some("max".to_owned())`. Generic Sol assertions and existing Sol formatting remain unchanged.
2. **Provider recovery** — the cancellation fixture drops `Run<'_>` before the session borrow; the recorded layout repair and qualification note are included. No production recovery source changed.
3. **First-run setup** — bounded TUI/CLI fixtures and two qualification notes. The first-run capsule imported no bootstrap files.
4. **Native rich text/history** — ordinary-prose preview geometry, static rich-text/source-copy coverage, native producer fixture, and candidate note. The candidate does not globally suppress ED3 and retains raw/source, semantic-copy, and final-Markdown paths; source tests are not physical Ghostty proof.

## Provisional commits

- `35a90f0f35928aef17ac651192a98b20f8221687` (parent `fcbc7b649315739f70440fc12c0bd29cb764115a`) — Luna, one test path.
- `e33862d696c835be7c97002a293aa08ee3a472fd` (parent `35a90f0f35928aef17ac651192a98b20f8221687`) — provider recovery, two paths.
- `9ae700532e8b19c8ddb76880d5cbe8ebb34b0098` (parent `e33862d696c835be7c97002a293aa08ee3a472fd`) — first-run setup, four paths.
- `837ca43d8c2421c3ce2174da86119d9067ae4a9b` (parent `9ae700532e8b19c8ddb76880d5cbe8ebb34b0098`) — native candidate, four paths.
- `17bace8d42f7980e14622b7eeec1de81a7794702` (parent `837ca43d8c2421c3ce2174da86119d9067ae4a9b`) — narrow formatting-only delta, three paths.

The JSON manifest records every original source-file/patch/result hash, source head/status, exact staged file set, and formatting before/after hash. The source-worktree statuses matched the expected quiescent dirty paths; the first-run source's bootstrap edits were left untouched.

## Checks actually run

- Verified input manifest SHA256 `b7cc5cb9db145c1e0db2f0824f340b90ada2c50a95b0dd55ed0097de22bccd2d`, all recorded source heads/files, patch hashes/byte counts, result hashes, and quiescent status sets.
- Ran `git apply --check` for each capsule immediately before application: all exit 0 with empty stderr; applied each capsule in order.
- Ran `git diff --check` after each import/formatting delta and on the integrated baseline diff; all exit 0. Ran `git diff --cached --check` for every provisional commit; all exit 0.
- Ran standalone `rustfmt --edition 2021 --check` on the seven collected Rust paths. Initial exit 1 identified only three new/modified regions; those were manually narrowed into the separate formatting commit. The identical check then exited 0.
- Read `OWNER-MAP.md` and `EXECUTION-UPDATE.md` once, and inspected the complete frozen results, patches, relevant Markdown, and narrow APIs before applying.

No Cargo/rustc/build/test/installer/remote/model/SSH/session/global-config action was run. The prior seven-stack failures and failed `9436eec9` compile remain evidence, not green. No installed candidate, physical terminal, live-provider/compaction, Windows, beta, or release acceptance is claimed.

## Handoff

The integrated source chain ends at `17bace8d42f7980e14622b7eeec1de81a7794702`; receipt files are committed separately for the next verifier. The final local receipt commit leaves the tree clean. Verify the final `HEAD` below this receipt before admission to `verify-rust`.
