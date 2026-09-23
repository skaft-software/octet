# Parity detail: editor/keybindings (§2b)

Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`. No upstream edits, no vendored
TypeScript. Owner paths: `crates/sexy-tui-rs/**`,
`crates/octet-coding-agent/src/tui/keymap.rs`, this file, and
`docs/swarm-audit/EXECUTION-parity-editor.md`.

## 2b.1 Namespaced configurable JSON keybindings, conflicts, platform defaults — Landed

- Source: `crates/octet-coding-agent/src/tui/keymap/keybindings.rs`
  (module `tui::keymap::keybindings`).
- Upstream anchors: `packages/tui/src/keybindings.ts` (`defaultKeys`,
  `KeybindingsManager.rebuild`, `getConflicts`, `getResolvedBindings`) and
  `packages/coding-agent/src/core/keybindings.ts` (additive `KEYBINDINGS`
  overlay, `useWindowsKeybindings`, win32 undo default, legacy-name migration,
  `keybindings.json` load).
- Behavior: full namespaced `tui.*` registry plus the `app.*` overlay;
  `default_definitions(platform, wsl)` applies the win32/WSL/darwin overrides
  (win32 `ctrl+z` undo, WSL `alt+z`, `app.suspend` unbound on win32, search
  `ctrl+f`); user overrides replace defaults and an explicit empty list
  unbinds; `rebuild` reports a `KeybindingConflict` whenever one key resolves
  from more than one user binding; `keybindings.json` loads with BOM stripping,
  legacy flat names migrate to namespaced ids (namespaced value wins on a
  duplicate), and `matches` compares a canonical `key_event_id` against
  `normalize_key_id` so modifier order/spelling do not matter.
- Tests (in-module): `defaults_are_namespaced_and_ordered`,
  `platform_defaults_match_windows_and_wsl_behaviour`,
  `user_overrides_replace_defaults_and_disable_on_empty_list`,
  `conflicts_are_reported_for_keys_shared_between_bindings`,
  `matches_normalizes_spelling_and_modifier_order`,
  `resolved_bindings_cover_every_definition`,
  `legacy_flat_names_migrate_to_namespaced_ids`,
  `namespaced_value_wins_over_legacy_duplicate`,
  `load_from_file_reads_keybindings_json_and_ignores_non_objects`,
  `reload_replaces_user_overrides`.

## 2b.2 Undo/redo with coalescing — Landed

- Source: `crates/sexy-tui-rs/src/text_editor.rs` + `text_editor/undo.rs`.
- Upstream anchors: `packages/tui/src/undo-stack.ts`,
  `packages/tui/src/components/editor.ts` `insertCharacter` (fish-style
  coalescing: whitespace captures a snapshot each time, word runs coalesce,
  other edits are atomic). Upstream ships undo only; redo is the additive
  inverse required by this row.
- Behavior: `TextEditAction::Undo` / `Redo`; word-character runs coalesce into
  one unit; a space starts a new unit; paste/newline/backspace/delete are
  atomic; any new edit clears redo; `set_text`/`take_text`/`replace_range`
  reset history because they are programmatic, not keystrokes.
- Tests: `fish_style_undo_coalesces_word_runs_and_splits_on_space`,
  `undo_and_redo_round_trip_and_new_edits_clear_redo`.

## 2b.3 Kill-ring / yank / yank-pop — Landed

- Source: `crates/sexy-tui-rs/src/text_editor/kill_ring.rs` +
  `text_editor.rs`.
- Upstream anchor: `packages/tui/src/kill-ring.ts`, `editor.ts` `yank`,
  `yankPop`, `deleteYankedText`.
- Behavior: consecutive kills accumulate (backward prepends, forward appends);
  `Yank` inserts the newest entry and records its range; `YankPop` only fires
  immediately after a yank with more than one entry, removes the previous yank,
  rotates the ring (newest moves to the front), and inserts the new newest.
- Tests (kill_ring): `empty_kill_is_ignored`,
  `accumulate_merges_and_orders_by_direction`, `rotate_cycles_newest_to_oldest`,
  `rotate_leaves_single_entry_alone`, `first_accumulating_push_still_creates_an_entry`;
  (editor) `kill_ring_yank_and_yank_pop_cycle_entries`,
  `consecutive_word_kills_accumulate_into_one_entry`.

## 2b.4 Word/line deletion and forward/backward jumps — Landed

- Source: `crates/sexy-tui-rs/src/text_editor/word_nav.rs` (port of
  `packages/tui/src/word-navigation.ts`) + `text_editor.rs`.
- Behavior: `WordLeft`/`WordRight` move one word run within the logical line
  and spill to the adjacent line edge; `JumpForward`/`JumpBackward` search the
  whole buffer for a character, case-sensitively, never landing on the current
  position; `DeleteWordBackward`/`DeleteWordForward` and
  `DeleteToLineStart`/`DeleteToLineEnd` kill into the ring, accumulating on a
  kill run, and merge lines (killing the newline) at a line edge.
- Tests (word_nav): `backward_skips_whitespace_then_stops_at_word_start`,
  `forward_skips_whitespace_then_stops_at_word_end`,
  `punctuation_and_unicode_form_runs`,
  `boundaries_are_clamped_and_never_panic`; (editor)
  `word_moves_and_jumps_stay_on_grapheme_boundaries`,
  `line_edge_kills_merge_lines_and_are_undoable`.

## 2b.5 OSC133 A/B/C zones and prompt jumps — Landed (primitive)

- Source: `crates/sexy-tui-rs/src/text_editor/prompt_zones.rs`.
- Upstream anchors: `packages/tui/src/tui-alt-screen.ts` (`OSC133_ZONE_PREFIX`,
  `OSC133_PROMPT_START`, `scrollToPrompt`).
- Behavior: parse `A`/`B`/`C` markers at line start with BEL or ST terminators,
  strip leading markers, index prompt rows, and answer
  previous/next/at-or-before prompt jumps. Painting and viewport scrolling stay
  with the embedding application.
- Tests: `markers_are_parsed_at_line_start_only`,
  `stripping_removes_repeated_markers_without_touching_content`,
  `scan_indexes_zone_rows_and_prompt_rows`,
  `prompt_jumps_walk_semantic_prompts_in_both_directions`,
  `empty_transcript_indexes_no_prompts`.

## 2b.6 Focus reporting and focus-out interaction reset — Landed (cooperative)

- Landed: `crates/octet-coding-agent/src/tui/keymap.rs` translates
  `Event::FocusGained`/`Event::FocusLost` into
  `InputAction::FocusGained`/`FocusLost` (test
  `focus_transitions_translate_independently_of_key_state`). The shell
  (`crates/octet-coding-agent/src/modes/interactive.rs`, owned by another
  worker) consumes the variants through `apply_focus_transition`, which resets
  transient interaction state on focus-out.
- Upstream anchor: `packages/tui/src/tui-alt-screen.ts` `FOCUS_IN`/`FOCUS_OUT`
  handling, which on focus-out clears selection press, auto-scroll, scrollbar
  hover/drag, pressed URL, the mouse gesture, and the last click.
- Enabling `?1004h` remains terminal-backend-owned (as the brief states).
- View-side wiring still to do (not in this worker's paths):
  `crates/octet-coding-agent/src/tui/view/viewport.rs` owns the rendered rows
  (`transcript_lines` → `ShellState::rendered_transcript(width)`, cached in
  `view/transcript_cache.rs` by width/dirty/overlay) and the scroll offset
  (`ShellState::scroll_from_bottom`, normalised by
  `max_scroll_from_bottom`/`resolved_scroll_from_bottom`). Exact change:
  (1) build `text_editor::prompt_zones::PromptZones::scan(&*transcript_lines(state, width))`
  whenever the row cache is rebuilt (same `dirty`/`width`/`overlay`
  invalidation) and keep it on `ShellState`, since the markers are only present
  in the cached rows before `sanitize_ordinary_surface_cell` strips escapes at
  paint time; (2) add `scroll_to_previous_prompt`/`scroll_to_next_prompt` next
  to `InteractiveShell::scroll`/`scroll_lines` (`tui/view.rs:4350`, `:4392`)
  that take `PromptZones::previous_prompt`/`next_prompt` for the current
  top/bottom visible row and convert the target row into `scroll_from_bottom`
  with the same `max_scroll_from_bottom` clamp; (3) bind those actions in
  `tui/keymap.rs` (upstream binds them in `tui-alt-screen.ts::scrollToPrompt`;
  `InputAction::PageUp`/`PageDown` already exist as the scrolling precedent).

## Roadmap #349 Alt+Up queue editing — Landed (keymap layer)

- `crates/octet-coding-agent/src/tui/keymap.rs`: Alt+Up returns
  `InputAction::EditQueued`; Escape while a turn is active returns
  `InputAction::DispatchQueued`; Enter-with-modifier returns
  `InputAction::Queue`; Alt+Up is a reserved extension shortcut. The shell
  (`modes/interactive.rs`, another worker) already consumes these variants.
- Tests: `queued_controls_are_one_shot_and_slash_escape_keeps_ownership` plus
  the updated queue/dispatch expectations.

### Retractable Ctrl+S steering (follow-up to #349)

- The receipt is authoritative, not the delivery event:
  `octet_agent::PreparedSteering`/`SteeringReceipt`
  (`crates/octet-agent/src/agent.rs`) let the shell withdraw a steering
  submission before the agent claims it, releasing its reserved control budget;
  `RunControl::prepare_steer`/`steer_retractable` are the admission and send
  seam. The shell queues the entry and keeps the receipt
  (`tui/view.rs::queue_retractable_steering`), and `edit_queued_message` recalls
  the newest selectable entry in one shared admission order.
- Sticky `/answer` keeps `queue_steering` (no receipt) because it changes the
  run's tool policy, so Option+Up cannot retract it.
- Tests: `real_queued_steering_option_up_edit_pty_contract`
  (`tests/activity_wait_pty.rs`) drives the real binary against the held-provider
  fixture and asserts the next request body and the durable session contain the
  edited steering and not the recalled original.

## 2c.3 LaTeX rendering — Landed

- Source: `crates/sexy-tui-rs/src/rich_text/latex/mod.rs` +
  `crates/sexy-tui-rs/src/rich_text/latex/tables.rs`. Entry point
  `sexy_tui_rs::rich_text::latex::render_latex(&str, RenderLatexOptions) -> Option<String>`,
  with the upstream contract: `Some(text)` or `None` (the upstream `undefined`
  result) — never a panic, never a partial guess.
- Upstream anchor: `packages/tui/src/latex.ts` (1394 lines: symbol tables,
  `LatexParser`, fraction/operator/matrix layout) at
  `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`.
- Covered: the generated symbol tables (224 symbols, 88 relation commands, 32
  named operators, blackboard/negated/accent/sub-superscript tables),
  sub/superscripts, `\frac`/`\dfrac`/`\tfrac`, `\sqrt[n]`, `\binom`, `\boxed`,
  operator limits (`\sum_{i=1}^{n}` stacks its bounds in display mode;
  `\limits`/`\nolimits` override), `\left…\right`/`\bigl…` sizing, and the
  environment set `matrix`, `pmatrix`, `bmatrix`, `Bmatrix`, `vmatrix`,
  `Vmatrix`, `smallmatrix`, `array`, `cases`(`*`), `aligned`, `align`(`*`),
  `alignedat`/`alignat`, `gather`ed, `multline`, `split`, `equation`(`*`).
- Bounded: `MAX_LATEX_NESTING_DEPTH` (64) caps recursive descent; past the cap
  the parser stops descending and returns `None`. Real-world nesting is far
  below it. `None` is also returned for every command the reference renderer
  refuses (`\cfrac`, `\genfrac`, `\cancel`, `\phantom`, `\hspace`,
  `\xrightarrow`, `\verb`, `\def`, `\usepackage`, `tikzpicture`, unknown
  commands, unbalanced groups).
- Port bug found and fixed by this row: the recursive parser had **no depth
  bound**, so a chain like `\frac{`×N (or `{`×N, or nested `cases`) exhausted
  the thread stack and *aborted the process* — `cargo test -p sexy-tui-rs` was
  red because of it. The parser now fails closed, and once an expression is
  known to be unrenderable it stops descending (without that, the
  `\frac` → argument → `\frac` chain kept re-entering on the same unconsumed
  token and grew with the input).
- Tests: `crates/sexy-tui-rs/tests/latex_render.rs`, 17 tests. Run:
  `cargo test -p sexy-tui-rs --test latex_render` =>
  `test result: ok. 17 passed; 0 failed` (upstream `latex.test.ts` corpus,
  captured display/inline layout corpora, operator-limit, matrix-delimiter,
  fail-closed and nesting tests, plus `TABLE_GOLDENS`: 407 captured goldens for
  the 403 upstream table entries (224 symbols, 88 relation commands, 32 named
  operators, 18 accents, 30 plain wrappers, 7 blackboard letters, 4
  negative-spacing commands — the 3 malformed `\negmedspace`-style inputs are
  fail-closed cases) plus 7 spacing edge cases, asserted by
  `every_symbol_table_entry_renders_its_reference_glyph`).
- Differential oracle: the real upstream `latex.ts` run under Node 26 with its
  real `visibleWidth` against this port. Two sweeps:
  (1) 2913 cases (upstream suite + curated real LaTeX + 2626 randomized
  token-soup cases) => **0 divergences**; (2) editor11's complete-by-construction
  sweep of 1061 cases that mechanically enumerates every upstream table entry
  and every environment in inline and display mode => `total=1061
  divergences=0`, plus a 403-case per-entry corpus => `total=403 divergences=0`.
  The only observed differences (11 of 3000 randomized cases) are inputs
  containing non-BMP characters, where the reference splits UTF-16 surrogate
  halves and the port indexes `char`s.
- Not modelled: ANSI styling (the renderer returns semantic text; the
  embedding component styles it).
- Consumer — Landed: a completed ```` ```latex ```` fence is rendered in display
  mode by `rich_text::markdown::parse` (`markdown.rs::render_diagram_fence`,
  reached only after the closure check). Other info strings, an oversized body
  (> `MAX_DIAGRAM_FENCE_BYTES`, 16 KiB), a `None` render, an empty render, and
  an unterminated fence keep the original source. Delimited math now has a
  separate consumer described below; it does not rely on pulldown math events.
- Consumer verification (editor12d): the *dispatch decision* (not just the
  engine) was re-derived from pulldown's own closure code
  (`firstpass.rs::parse_fenced_code_block` + `scanners.rs::scan_closing_code_fence`
  in pulldown-cmark 0.12.2) and swept over 560 top-level fence shapes (opener
  indent 0–3 × closer indent 0–5 × tails `""`/`" "`/`"\t"`/`" not a close"` ×
  markers ```` ``` ````/`~~~`): **0 fences rendered from an unclosed block, 0
  complete fences missed**.

## 2c.3a Markdown math delimiters — Unreleased consumer

- Source: `crates/sexy-tui-rs/src/rich_text/latex/markdown.rs`, consumed by
  `markdown.rs` and `stream.rs`. The newer delimiter reference is Pi
  `890f92088`, distinct from the older LaTeX engine reference above.
- `$…$` and `\(…\)` render inline through the existing LaTeX engine.
  `$$…$$` and `\[…\]` can render display math at a block boundary; inside
  prose they use inline layout. Multiline display expressions keep fraction and
  matrix layout, including a standalone `=` that CommonMark would otherwise
  reinterpret as a heading underline.
- Math is recognized before CommonMark consumes escapes, underscores or table
  pipes. Inline code, code fences, and HTML are not math input. Currency and
  shell-variable guards preserve examples such as `Costs $5 and $10`,
  `$8k–$12k`, and `$HOME/$USER`; escaped dollar signs remain literal.
- Unsupported expressions and recognized incomplete math retain their raw
  delimiters/source rather than being partly rendered or reinterpreted as
  Markdown. During streaming, pending expressions remain raw until closed;
  settled blocks retain the streaming parser's committed-prefix behavior.
  The existing LaTeX nesting limit and 16 KiB math-body dispatch bound still
  apply. This is a bounded terminal renderer, not a complete TeX implementation.
- Fixtures: `crates/sexy-tui-rs/tests/markdown_math.rs` covers inline/display
  examples, currency/shell/code protection, raw fallback, and chunked streaming.
  The older LaTeX differential counts above qualify the engine only, not this
  new delimiter consumer. Run its checks separately:

  ```sh
  cargo test -p sexy-tui-rs --test markdown_math --test latex_render --test rich_fences
  ```

## 2c.4 Mermaid box-drawing diagrams — Unreleased expansion, bounded subset

- Source: `crates/sexy-tui-rs/src/rich_text/mermaid.rs` and
  `mermaid/layout.rs`; entry point
  `sexy_tui_rs::rich_text::mermaid::render_mermaid(&str) -> Result<MermaidArt, MermaidError>`.
  Pi's `packages/coding-agent/src/modes/interactive/components/mermaid.ts`
  delegates layout to `grok-mermaid` 0.2.3. Octet adapts the upstream Rust
  flowchart/group layout with plain-text output and hard bounds; it builds and
  runs without npm, Node, or network access. This layout is Apache-2.0, not Pi's
  MIT license; see [third-party notices](../../THIRD_PARTY_NOTICES.md#grok-mermaid-and-grok-build).
- Flowchart syntax includes `graph`/`flowchart`, `TD`/`TB`/`LR`/`BT`/`RL`,
  nested `subgraph`/`end`, group links, `&` node lists, chained links, cycles,
  self-links, skip-layer links, inline and pipe-delimited edge labels, comments,
  semicolon-separated statements,
  class/style decorations, quoted/CJK labels, and label breaks and supported
  entities. Per-subgraph `direction` is accepted but ignored, as in the
  reference. Reverse directions preserve label order rather than mirroring
  text. Compound layouts route around node boxes.
- This remains a bounded flowchart subset: other diagram families, invalid
  nesting, unsupported arrow/syntax forms, and over-limit input fail closed.
  Nodes use rectangular or rounded boxes rather than full Mermaid shape
  geometry; solid/dotted/thick links remain plain terminal glyphs, not Pi's
  styled spans. No full Pi/grok-mermaid grammar, geometry, style-span,
  warning-channel, or error-message equivalence is claimed.
- Source is capped at 16 KiB. Node/edge counts, label rows, canvas dimensions,
  nested groups, and total canvas cells have independent hard limits in
  `mermaid.rs` and `mermaid/layout.rs`; labels may wrap or truncate to fit
  the layout's bounded boxes. A typed renderer error keeps the fence's original
  source instead of publishing partial art. `<br/>` is normalized to whitespace;
  node labels wrap at 24 cells / four lines, not arbitrary multiline geometry.
- Evidence boundary: the original `tests/mermaid_render.rs` goldens were
  self-captured. The new `tests/mermaid_parity.rs` compares the explicit corpus
  under `tests/fixtures/mermaid/` with real grok-mermaid 0.2.3 output: 67
  flowchart cases plus the reported 64-node architecture graph with one
  equivalent edge spelling in the oracle. That complete graph is 501 cells
  wide and 86 rows tall; normal terminal widths must show source, not crop art. This is not a
  full-parser oracle: Octet accepts punctuation-bearing inline labels for which
  grok-mermaid can return partial art plus warnings; Pi's final-warning fallback
  is a separate behavior. Dotted/hyphenated IDs are also an Octet extension.
  Historical test counts do not qualify the expanded implementation.

### Shared fence consumer

`rich_text::markdown::parse` dispatches completed `latex`, `mermaid`, `graph`,
and `flowchart` fences. `graph TD` and `flowchart LR` can carry the header in the
info string. Unknown info strings (including `tex`, `math`, `dot`, `graphviz`,
and `plantuml`), oversized bodies, renderer errors, empty renders, and unclosed
fences retain the original code body. A failed Mermaid render also adds a visible
`Mermaid diagram not rendered: …` reason. A successful Mermaid fence retains
both its source and art in `Block::Diagram`: if art does not fit the available
content width, rendering shows source rather than wrapping/cropping graph rows.
A wider viewport can show art again. The language label remains as provenance;
`Document::plain_text()` is the width-independent drawn-art projection.

While a fence is open, streaming shows its growing source. The closing fence
publishes the rendered diagram; later chunks preserve committed rows. Partial
openers, four-space-indented pseudo-closers, and closers with trailing junk do
not dispatch early. Rendered glyph rows bypass source-code syntax highlighting.

The conservative nested-container boundary remains: a closing fence indented
more than three raw spaces may stay source even when CommonMark accepts it
inside a deeply indented list. Top-level list items and blockquotes can dispatch.

`crates/sexy-tui-rs/tests/rich_fences.rs` owns fence dispatch, unknown-language
non-dispatch, fallback, closure-matrix, styling, and chunked-streaming fixtures;
`tests/mermaid_render.rs` owns engine goldens, bounds and token-soup cases;
`tests/mermaid_parity.rs` owns the grok-mermaid oracle comparison. Run:

```sh
cargo test -p sexy-tui-rs --test mermaid_render --test mermaid_parity --test rich_fences
```

These are source contracts and verification commands, not claims that the
expanded renderer has passed every fixture or physical-terminal acceptance.
