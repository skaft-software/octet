# Rich-text render current candidate

**Status:** source-only candidate; not qualified. This note records the bounded append-local rendering repair and its verification boundary. It does not claim a physical #311 ownership refactor, emitted VT bytes, or native-terminal acceptance.

## Scope

The current #311 contract makes width accounting and sanitization boundaries the repair scope. The historical block/inline/code/table/diff move plan is not a prerequisite for this candidate. The renderer facade and cache owner remain in `crates/sexy-tui-rs/src/rich_text/render.rs`; Markdown parsing, streaming preview ownership, highlighting, and diff semantics remain in their existing modules. Width measurement and source sanitization stay at the renderer boundary rather than being duplicated by the append cache.

## Candidate change

`AppendOnlyTail` now recognizes an ordinary canonical prose preview whose paragraph consists only of the growing `Inline::Raw` suffix. It records an empty semantic prefix without invoking rich-prefix flattening or incrementing `rich_prefix_layouts`. A nonempty semantic prefix is still flattened once, with source offsets retained for the append-local suffix. This preserves the distinction between immutable rich content and the mutable raw tail while avoiding hidden whole-prefix work during ordinary prose promotion.

`StreamingMarkdown::append_preview` now defers literal-to-prose classification until an accepted suffix contains a newline, while retaining the existing full classification when a prior represented source line can still change. This avoids rescanning a growing no-newline prefix. `StreamingStats::preview_classified_bytes` records the classification work, and the 30,000-chunk regression asserts a linear bound without changing its chunk count or timeout contract.

The source and finish contracts remain authoritative: raw source is not rewritten, `finish()` remains equivalent to static Markdown parsing, and committed rows remain the only rows eligible for native-scrollback commits. The native-scroll repair capsule and unrelated history behavior are unchanged.

## Focused regression coverage

The existing focused coverage exercises:

- `rich_and_literal_append_work_scales_linearly_with_exact_live_rows` in `crates/sexy-tui-rs/src/rich_text/render.rs`, including the exact `rich_prefix_layouts == 0` ordinary case and `== 1` rich-prefix case.
- `fence_search_and_literal_preview_copy_are_append_local` in `crates/sexy-tui-rs/src/rich_text/stream.rs`.
- Rich rendering and native-history tests named by the baseline capsule remain in scope for the verifier; the existing streaming regression gained an operation-count assertion, while no native-history source was changed.

No build, formatter, test, benchmark, native terminal session, SSH journey, or live observation was run. Ghostty, Terminal.app, Ubuntu-over-SSH, native selection/copy, PageUp, scroll position, and emitted-byte acceptance therefore remain unverified. Do not infer those results from source inspection or deterministic frame comparisons.

## Baseline boundary

The immutable baseline is `a75a41157fa82ff22219b2b312cccf0eadf9e70d`. The native-scroll repair capsule is preserved. Only the permitted rich-text source and this qualification record are changed.
