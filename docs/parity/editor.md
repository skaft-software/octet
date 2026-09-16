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
  an unterminated fence keep the original source. `$$…$$`/`\[…\]` math is
  deliberately **not** wired — the parser does not enable math events; see the
  consumer note under 2c.4.
- Consumer verification (editor12d): the *dispatch decision* (not just the
  engine) was re-derived from pulldown's own closure code
  (`firstpass.rs::parse_fenced_code_block` + `scanners.rs::scan_closing_code_fence`
  in pulldown-cmark 0.12.2) and swept over 560 top-level fence shapes (opener
  indent 0–3 × closer indent 0–5 × tails `""`/`" "`/`"\t"`/`" not a close"` ×
  markers ```` ``` ````/`~~~`): **0 fences rendered from an unclosed block, 0
  complete fences missed**.

## 2c.4 Mermaid box-drawing diagrams — Landed (bounded subset)

- Source: `crates/sexy-tui-rs/src/rich_text/mermaid.rs`, entry point
  `sexy_tui_rs::rich_text::mermaid::render_mermaid(&str) -> Result<MermaidArt, MermaidError>`.
- Upstream anchor:
  `packages/coding-agent/src/modes/interactive/components/mermaid.ts` delegates
  layout to the external `grok-mermaid` package. That dependency cannot be added
  here (no network/npm dependency in a renderer), so this row ships a
  self-contained engine instead of a port.
- Covered: `graph`/`flowchart` with `TD`/`TB`/`LR` (with optional `;`, and `;`
  may terminate the header and separate same-line statements:
  `flowchart LR; A --> B; B --> C`), node ids with or without labels, the label
  shapes `[]`, `()`, `{}`, `(())`, `([])`, `[[]]`, `{{}}` (all drawn as a box),
  quoted labels including labels containing the closing delimiter (`A["a[b]c"]`)
  and quoted `|link labels|`, `%%` comments anywhere outside a quoted label,
  chained and repeated links, `-->`/`->` (arrow head), `---` (no head),
  `-.->`/`==>` (arrow head; stroke styling not modelled), `|label|` edge
  labels, `:::class` decorations, `classDef`/`class`/`style`/`linkStyle`/`click`
  directives, quoted and CJK labels, and disconnected components.
- Fails closed with a typed `MermaidError` (no panic, no partial diagram, no
  unbounded work): other diagram types (`pie`, `sequenceDiagram`, …), `BT`/`RL`
  (rejected rather than misrendered — mirroring the grid would reverse labels),
  `subgraph`/`end`/`direction` statements, `&` node lists, `A -- text --> B`
  inline labels, other arrow tokens, unbalanced brackets/quotes, cycles, edges
  that skip a layer, and anything over `MAX_MERMAID_*` (16 KiB source, 64 nodes,
  256 edges, 48-cell labels, 400×200 art).
- Not modelled: shape outlines (diamonds/stadiums render as boxes), link stroke
  styling, subgraphs, `BT`/`RL`, HTML-entity decoding in labels (emitted
  literally), upstream's style-span/warning channels, and the "unrendered
  diagram" fallback text — the fence consumer below keeps the original source
  for every `Err`.
- Evidence scope — do **not** overclaim: the 29 goldens below are
  **self-captured** from this engine's own output; they pin the bounded subset,
  they are not a differential oracle. Unlike 2c.3 (upstream `latex.ts` run under
  Node), there is no upstream harness to diff against:
  `packages/coding-agent/.../mermaid.ts` delegates layout to the external
  `grok-mermaid` npm package, which is not vendored and cannot be added here (a
  renderer must build and run offline, with no network/npm dependency). So
  `BT`/`RL` (rejected rather than mirrored), `subgraph`/`end`/`direction`, `&`
  node lists and HTML-entity labels are a deliberate, pinned fail-closed
  boundary — not a verified match to `grok-mermaid`'s layout or error strings.
  The fence-level goldens in `rich_fences.rs` are self-captured from this engine
  too: they pin *which* blocks reach 2c.3/2c.4 and what happens when they do,
  not that the drawn art matches upstream.
- Consumer — Landed (supersedes the earlier "no consumer is wired yet" note):
  `rich_text::markdown::parse` renders a *completed* fenced block whose info
  string explicitly names `latex`, `mermaid`, `graph`, or `flowchart`
  (`markdown.rs::render_diagram_fence`, called from `Frame::Code` only when
  `fence_is_closed`; `graph TD`/`flowchart LR` carry their header in the info
  string). Every other info string, any body over `MAX_DIAGRAM_FENCE_BYTES`
  (16 KiB), a typed renderer error, an empty render, and an unterminated fence
  keep the original bounded source as a plain `CodeBlock` — never an empty
  block and never a reinterpretation of a non-diagram fence. While a fence is
  open the streaming layer shows the raw growing body
  (`stream.rs::stabilize`) and publishes the diagram once, when the closing
  fence arrives; later chunks do not move the committed rows. The opener may
  itself arrive in pieces — a `<3`-backtick or partial info string is withheld
  or shown raw and never dispatches early. A body line that merely *looks* like
  a closer — indented four spaces, or carrying trailing junk such as
  ```` ``` not a close ```` — does not terminate the fence either:
  `fence_is_closed` mirrors the parser's own CommonMark closure rules, so no
  stray body line can be glued into partial art (the LaTeX engine would
  otherwise accept the backticks as text). `$$…$$` /
  `\[…\]` math is **not** wired: the parser does not enable math events, so
  `$…$` stays literal text (`Costs $5 and $10 total.` is untouched, and
  `Event::DisplayMath`/`InlineMath` in `markdown.rs` are unreachable arms).
  Non-dispatch is asserted byte-for-byte: 15 unknown/alias info strings (`tex`,
  `math`, `latexish`, `dot`, `graphviz`, `plantuml`, …) and a bare fence produce
  exactly the block a `text` fence produces with the same body, for both a
  LaTeX-shaped and a Mermaid-shaped body. The `latex`/`mermaid` label survives on
  the block as provenance, so the rendered art is labelled and copy-text returns
  the drawn diagram, not the source. Verified styling: with
  `syntax_highlighting: true` and truecolor on, the glyph rows carry **no** ANSI
  escapes (only the language label is dimmed), so the one-colour-per-grapheme /
  no-background-fill invariants hold for rendered art. Fence-layer tests:
  `crates/sexy-tui-rs/tests/rich_fences.rs`, 19 tests. Run:
  `cargo test -p sexy-tui-rs --test rich_fences` =>
  `test result: ok. 19 passed; 0 failed` (LaTeX/Mermaid box-drawing goldens
  through the fence, `graph`/`flowchart` info-string headers, unknown-fence
  non-reinterpretation and byte-for-byte non-dispatch, unsupported/oversized/
  unterminated degradation, pseudo-closing lines that stay source, CRLF and
  `~~~` fences, blockquote nesting, empty-render degradation,
  math-not-dispatched, syntax-styling invariance, the 160-case closure matrix
  below, the byte-at-a-time streaming publication scan, and the nested-container
  fail-closed boundary; plus
  three streaming tests: diagram published only at the closing fence with stable
  committed rows, a failed fence that streams its raw source and stays literal
  after the close, and an info string split across chunks that never dispatches
  early).
  Observed:

      ```latex\n\begin{pmatrix}1&2\\3&4\end{pmatrix}\n``` =>
      ⎛ 1 │ 2 ⎞
      ⎝ 3 │ 4 ⎠
      ```mermaid\ngraph LR\n  A[Start] --> B[Done]\n``` =>
      ┌───────┐    ┌──────┐
      │ Start ├───▶│ Done │
      └───────┘    └──────┘
      ```rust\n\\frac{1}{2}\n``` =>
      \frac{1}{2}            (unknown fence: plain code block)
      ```tex\n\\frac{1}{2}\n``` =>
      \frac{1}{2}            (a LaTeX alias, still not dispatched)
      $$\\frac{1}{2}$$ => $$\frac{1}{2}$$  (math is not a fence, stays literal)
      ```latex\n\\cfrac{1}{x}\n``` =>
      \cfrac{1}{x}           (unsupported: original source)
      ```latex\n{}\n``` => {} (empty render: original source, not empty block)
- Consumer verification sweeps (editor12d, `rich_fences.rs` +
  gitignored `_ed12d_dispatch.rs` probes):
  (1) **closure matrix** — 192 committed cases (opener indent 0–3 × closer indent
  0–5 × tails `""`/`" "`/`"\t"`/`" not a close"` × markers ```` ``` ````/`~~~`)
  assert dispatch **exactly** when pulldown's own rule closes the fence
  (indent ≤ 3 relative to the container content indent, marker run ≥ the
  opening run, spaces-only tail), and that every other shape keeps the original
  source; the wider 560-case probe sweep is 0 fabricated renders / 0 missed;
  (2) **byte-at-a-time streaming scan** — pushing
  `intro\n\n```mermaid…\n```\n\noutro\n` one character at a time, the diagram row
  appears at exactly the byte that completes the closing fence line, is never
  lost afterwards, never appears partially, and the committed rows equal a
  full-document render;
  (3) **bounds** — a 1 MiB `mermaid` body stays literal source (the 16 KiB
  `MAX_DIAGRAM_FENCE_BYTES` cap, no art, no panic, ~2.3 s in the debug profile);
  the LaTeX engine's output grows **linearly** with the body (17.6 KB of
  `\frac{1}{2}` → 3 rows × 4798 cells, ~25 ms; dispatch itself stops at 16 KiB);
  a >64-node graph is rejected by `MAX_MERMAID_NODES` before any layout; 400
  randomized fence-soup sources survive parse + chunked streaming with no panic.
- Known boundary, pinned by `deeply_indented_container_closers_stay_literal`: a
  fence nested in a container whose closing line is indented more than three
  spaces **raw** (e.g. an item at content indent 4 with its closer written at the
  same indentation) stays literal source even though pulldown accepted the
  closer and produced a clean body — the raw range does not reveal the container
  content indent. `stream.rs`'s lexical scanner applies the same conservative
  rule, so the streamed rows and the full-document render agree (asserted
  byte-for-byte in that test). Top-level list items (content indent ≤ 3) and
  blockquotes dispatch.
- CHANGELOG-ready (fence wiring):
  - Rich markdown: a completed ```` ```latex ```` fence renders through the LaTeX
    engine in display mode, and ```` ```mermaid ````/```` ```graph ````/
    ```` ```flowchart ```` through the Mermaid engine; the fence label stays on
    the block, so the drawing is labelled and copy-text returns the diagram
    rather than its source.
  - Rich markdown fences fail closed: unknown or malformed info strings (`tex`,
    `math`, `latexish`, `dot`, `graphviz`, `plantuml`, a bare fence) are never
    reinterpreted, and a body over 16 KiB, a renderer error, a render that
    produces nothing (empty expression, `{}`, header-only graph), an unterminated
    fence and a stray "closing" line (four-space indent, trailing junk) all keep
    the original bounded source instead of empty or partial art.
  - Rich markdown: diagrams are published when the fence closes — the streaming
    layer shows the raw growing body while the fence is open, the closing fence
    replaces it once, and later chunks never move the published rows
    (`$$…$$`/`\[…\]` math is deliberately not wired).
- Tests: `crates/sexy-tui-rs/tests/mermaid_render.rs`, 11 tests. Run:
  `cargo test -p sexy-tui-rs --test mermaid_render` =>
  `test result: ok. 11 passed; 0 failed` (29 supported goldens including the
  semicolon/comment/quoted-label cases, 20 fail-closed messages, a deterministic
  1500-input token-soup fuzz test that must not panic or exceed the size caps,
  width invariant, wide-label alignment, size limits).
- Fixed while landing this row: `:::` class annotations parsed only two colons
  (so `A[Foo]:::highlight` failed), `BT`/`RL` were silently drawn as `TD`/`LR`,
  `---` drew an arrow head, a trailing `;` on the header line was rejected, and
  a CJK label pushed the box's right border one column right (the covered cell
  of a double-width glyph was emitted as a space).
- Fixed/extended by editor11: `%%` comments are now stripped anywhere on a line
  (outside quotes) instead of only at line start, `;` separates statements on a
  line and may share the header line, quoted labels may contain the closing
  delimiter, quoted `|link labels|` lose their quotes, and
  `subgraph`/`end`/`direction` report a named typed error instead of being
  parsed as node ids. Observed:

      flowchart LR; A[One] --> B[Two]; B --> C[Three] =>
      ┌─────┐    ┌─────┐    ┌───────┐
      │ One ├───▶│ Two ├───▶│ Three │
      └─────┘    └─────┘    └───────┘
      flowchart LR\n  A["a[b]c"] --> B[Two] =>
      ┌───────┐    ┌─────┐
      │ a[b]c ├───▶│ Two │
      └───────┘    └─────┘
      flowchart LR\n  A -->|"two words"| B =>
      ┌───┐two words  ┌───┐
      │ A ├──────────▶│ B │
      └───┘           └───┘
      flowchart LR\n  subgraph S\n  A --> B\n  end =>
      Err "dropped, line 2: `subgraph` statements are not supported"
