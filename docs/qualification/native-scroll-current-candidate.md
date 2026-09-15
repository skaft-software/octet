# Native-scroll current candidate

Status: deterministic Rust/VT qualification passed on `df5a7e80` plus the current TUI diff; physical terminal qualification remains open. This note covers the reported native-history regression and does not claim the frozen Wave 1/#392 acceptance gate.

## Scope and recorded evidence

The controlled binary fixture used a mouse-capable 96x18 terminal, no color, no tools, and one text delta per streamed chunk. It sent `# APPEND heading\n\n`, then 48 ordinary prose chunks, adding a blank-line boundary at indices 7, 15, 23, 31, 39, and 47. The recorded local-release and installed-v0.7.6 fixtures each emitted six ED3/full-history clears. Those fixtures are evidence recorded by the task handoff; this source-only lane did not rerun them.

The diagnosed cause is a mutable display interpretation, not lost transcript data: literal source-line previews used physical rows, then a parser commit changed the already-painted prefix to canonical Markdown soft-break geometry. Native replay consequently began before the prior viewport top. The candidate makes ordinary prose use that canonical display projection as soon as its first visible line is classified as ordinary. Structural candidates, hard breaks, controls, fences, lists, quotes, tables, and other unsafe cases retain their literal or semantic fallback.

## Candidate behavior

- `StreamingMarkdown` keeps raw bytes/text authoritative. `finish()` remains the static Markdown parse of the decoded source; semantic copy remains derived from the canonical document and withheld raw suffix.
- The ordinary projection stores source lines as `Inline::Raw` text with pending soft-break separators and paragraph-boundary state. It changes display geometry only; it does not rewrite `raw`, `tail`, or transcript source.
- A parsed structural prefix followed by a proven blank boundary is classified from the ordinary suffix, so a heading or other structural block cannot force the following prose back to literal physical rows (`crates/sexy-tui-rs/src/rich_text/stream.rs`).
- The existing append-only render cache remains the incremental layout boundary. Its visual tail prefix is provisional; only `committed_rows()` represents parser-committed rows that may cross a native-scrollback commit boundary. Structural promotion, resize, hard-break/control input, and finalization retain replacement behavior rather than globally suppressing ED3.

## Source and regression coverage

- `crates/sexy-tui-rs/src/rich_text/stream.rs`: bounded ordinary-prose projection, explicit open/provisional-versus-completed paragraph coverage, structural-suffix correction, and parser-threshold geometry assertions.
- `crates/sexy-tui-rs/tests/rich_rendering.rs`: the exact 48-chunk/96-column sequence compares every incremental frame with static Markdown rows, compares only the reported `stable_prefix`, distinguishes open-paragraph reflow from parser-committed rows after a blank-line boundary, and checks finalization and raw source.
- `crates/octet-coding-agent/src/tui/view/native_history_tests.rs`: a 96x18 `NativeReplay` sends the same sequence through the real shell/Pi/VT producer and asserts no additional full redraw, while checking assistant source and copy text.
- `docs/qualification/native-scroll-current-candidate.md`: this qualification boundary and handoff note.

No generic TUI, native-scrollback, transcript-cache, host/view, Ghostty preference, or subprocess boundary file was changed by this lane.

## Earlier central verification proposal

The source-only handoff originally proposed these focused checks (current observed verification follows below):

```text
cargo test -p sexy-tui-rs --test rich_rendering ordinary_streaming_paragraph_boundaries_keep_canonical_rows_stable
cargo test -p sexy-tui-rs parser_thresholds_preserve_canonical_prose_geometry
cargo test -p octet-coding-agent native_ordinary_streaming_paragraph_boundaries_do_not_replay
```

The verifier should also run the surrounding rich-text streaming tests and inspect the exact emitted VT bytes for the fixture: no unexpected ED3, home/ED2 replay, or saved-history clear while ordinary prose grows; genuine structural, resize, and finalization replacements must remain available.

## Reference and Windows boundary

The comparison baseline is pinned Pi 0.84.4 at `b79e4cc834970cca69daebffab7df1da7d1e52c4` (MIT). The available read-only checkout is Pi coding-agent 0.85.1 at `08dc60bc52d89d6823a9738cc90b1916e5e446e5`; it is not the pinned baseline and no reference source was copied.

Windows qualification is a separate centralized handoff. It needs a Rust compiler with target `x86_64-pc-windows-gnu`, the matching Rust standard library, and a Zig GNU-compatible C/C++ compiler/linker configured for that exact target. The checker should record the selected Zig executable/version and configure the target linker/compiler variables before a cross-target Cargo check/build; a native Windows runner is still required for the process test. The existing Mac-side Zig and Rust-target availability is filesystem/tool-availability evidence only, not a Windows build or runner result. This lane did not attempt either.

## Acceptance limits

The current deterministic matrix passed; cross-compilation, a Windows native runner, the installed-binary ED3 comparison, and physical Ghostty trackpad scroll/selection verification remain unrun. Do not mark the installed candidate qualified or claim native selection preservation from renderer tests.

## Current deterministic verification

Base `df5a7e809715961b9344af6b52e43a6ca48f56b3` plus the TUI working-tree diff:

- `cargo test --locked -p octet-coding-agent --lib native_ -- --nocapture`: 32 passed.
- `cargo test --locked -p octet-coding-agent --lib tui::`: 529 passed.
- `cargo test --locked -p sexy-tui-rs --test images_current --test rich_rendering --test pi_tui_render`: 6 + 4 + 27 passed after the image fixture's exact-OSC framing correction.

The new fragmented-table fixture verifies 96×18 → 40×8 → 120×30 → 96×18,
exactly one ED2/ED3 replay at each resize, quiet subsequent frames, source/copy
fidelity, and exactly-once history. The late-reference fixture requires one
necessary retrospective replay at canonical finalization, then quiet frames.
The offscreen roster final-state transition still emits one necessary ED3; it
is not silently frozen to satisfy an artificial no-clear assertion.

[Execution record and logs](../swarm-audit/EXECUTION-tui.md). No physical paint,
wheel offset, selection, SSH, or installed-candidate acceptance is inferred.
