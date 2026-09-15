# Editor/keybinding parity execution evidence (§2b rows 2b.1–2b.6)

Upstream read-only reference verified at
`/Users/achumukundan/github/earendil-works/pi` @
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391` (`git rev-parse HEAD`). No upstream edits,
no vendored TypeScript, no commits, existing workspace `target`.

## Behavioral source

Read upstream `packages/tui/src/keybindings.ts` (registry, `defaultKeys`,
`KeybindingsManager.rebuild`, `getConflicts`, `getResolvedBindings`),
`packages/coding-agent/src/core/keybindings.ts` (additive `KEYBINDINGS` overlay,
`useWindowsKeybindings`, `platform === "win32"` undo default, legacy-name
migration, `keybindings.json` load), `kill-ring.ts`, `undo-stack.ts`,
`word-navigation.ts`, `components/editor.ts` (kill/yank/yank-pop, fish-style undo
coalescing, `deleteToStartOfLine`/`deleteToEndOfLine`, `deleteWordBackwards`/
`deleteWordForward`, `jumpToChar`), `tui-alt-screen.ts` (`OSC133_ZONE_PREFIX`,
`OSC133_PROMPT_START`, `scrollToPrompt`) and `tui-plan.md` §wheel/focus rows.

## Status (work in progress)

- 2b.1 configurable namespaced JSON keybindings: in progress (`keybindings` child module of `tui::keymap`).
- 2b.2 undo/redo with coalescing, 2b.3 kill-ring/yank/yank-pop, 2b.4 word/line
  deletion + jumps: in progress in `sexy-tui-rs` `text_editor`.
- 2b.5 OSC 133 A/B/C zones + prompt jumps: zone index primitive in progress;
  viewport wiring is view-owned.
- 2b.6 focus reporting: keymap translation + interaction-reset model in
  progress; `?1004h` enablement is terminal-backend-owned.

Observed commands and per-item status are appended below as they are run.

## editor3 adoption log

- `$(date -u +%Y-%m-%dT%H:%M:%SZ)` ADOPT: editor2 partial state found via
  `git status --porcelain -- crates/sexy-tui-rs crates/octet-coding-agent/src/tui/keymap.rs`.
  Modified: `crates/octet-coding-agent/src/tui/keymap.rs`,
  `crates/sexy-tui-rs/src/text_editor.rs`, `crates/sexy-tui-rs/tests/images_current.rs`;
  untracked: `crates/sexy-tui-rs/src/text_editor/{kill_ring.rs,prompt_zones.rs,undo.rs}`.
- `cargo check -p sexy-tui-rs 2>&1 | tail -40` => 3 errors:
  E0599 `TakeWhile<I,P>::next_back` (prompt_zones.rs), E0004 non-exhaustive
  `apply_action` for the 12 new `TextEditAction` variants. Fixing as first job.

- `$(date -u +%Y-%m-%dT%H:%M:%SZ)` FIX COMPILE: `prompt_zones.rs` `prompt_at_or_before`/
  `previous_prompt` now use `.iter().rev().find(...)` instead of unsupported
  `TakeWhile::next_back`. `text_editor.rs` `apply` now covers all 12 new
  `TextEditAction` variants; added `mod word_nav` + `find_word_backward`/
  `find_word_forward` (port of `word-navigation.ts`), `insert_character`
  (fish-style coalescing), `insert_atomic`, `move_word`, `jump_to_char`,
  `delete_word`, `delete_to_line_edge`, `yank`, `yank_pop`, `undo`, `redo`,
  `push_undo`, `restore`, `reset_history`, `logical_line_bounds`.
  Command: `cargo check -p sexy-tui-rs 2>&1 | tail -30` => Finished, 0 errors.

- `$(date -u +%Y-%m-%dT%H:%M:%SZ)` ROW 2b.2/2b.3/2b.4 landed in `text_editor.rs`
  (+ `word_nav.rs`). Fixed editor2's wrong `kill_ring.rs::rotate_cycles_newest_to_oldest`
  expectation (rotation matches upstream). Tests added:
  `fish_style_undo_coalesces_word_runs_and_splits_on_space`,
  `undo_and_redo_round_trip_and_new_edits_clear_redo`,
  `kill_ring_yank_and_yank_pop_cycle_entries`,
  `consecutive_word_kills_accumulate_into_one_entry`,
  `word_moves_and_jumps_stay_on_grapheme_boundaries`,
  `line_edge_kills_merge_lines_and_are_undoable`, plus 4 `word_nav` tests.
  Command: `cargo test -p sexy-tui-rs --lib text_editor` =>
  `test result: ok. 40 passed; 0 failed`.
  Found+fixed 3 real bugs during red/green (forward jump skipped self; delete anchors).

- `$(date -u +%Y-%m-%dT%H:%M:%SZ)` ROW 2b.1 landed: new module
  `crates/octet-coding-agent/src/tui/keymap/keybindings.rs` (namespaced
  registry, `default_definitions` with win32/WSL/darwin platform defaults,
  `use_windows_keybindings`, conflict reporting, `keybindings.json` load with
  BOM strip, legacy-name migration + ordering, `matches` via canonical
  `key_event_id`/`normalize_key_id`). Wired as `pub mod keybindings;` in
  `keymap.rs`. Compiles: unused-import-only warnings earlier from transitive
  deps. BLOCKED (test run only): `cargo test -p octet-coding-agent` cannot
  build because ANOTHER WORKER left `crates/octet-agent/src/tools/bash.rs:1391`
  with an unclosed delimiter (outside my paths; recorded, not fixed).
- `$(date -u +%Y-%m-%dT%H:%M:%SZ)` ROW 2b.5 landed in `sexy-tui-rs`:
  `text_editor/prompt_zones.rs` (OSC133 A/B/C parse at line start, BEL and ST
  terminators, `PromptZones::scan`, `previous_prompt`/`next_prompt`/`jump`).
  Covered by its 4 unit tests in the passing `sexy-tui-rs` run above. Viewport
  wiring is view-owned (not in my paths).
- ROW #349 is already present in `keymap.rs` (Alt+Up -> `EditQueued`,
  Escape-while-active -> `DispatchQueued`, `InputAction::Queue`, reserved
  extension shortcut for Alt+Up) and tui3 already consumes them in
  `modes/interactive.rs`.

## editor3 CHANGELOG-ready bullets

- Add namespaced, configurable JSON keybindings with conflict reporting and
  platform (win32/WSL/darwin) defaults (`tui::keymap::keybindings`).
- Add editor undo/redo with fish-style word-run coalescing, an Emacs-style
  kill ring with yank/yank-pop, word/line deletion, and forward/backward
  character jumps (`sexy_tui_rs::TextEditor`).
- Add OSC 133 prompt-zone parsing and prompt-jump indexing
  (`sexy_tui_rs::PromptZones`).
- Translate terminal focus-in/focus-out events into semantic input actions.
- Add the Alt+Up queued-message editing binding and Escape-while-active
  dispatch semantics.

## editor3 verification

- `cargo check -p sexy-tui-rs` => Finished (0 errors).
- `cargo test -p sexy-tui-rs --lib text_editor` => 40 passed; 0 failed.
- `cargo check -p sexy-tui-rs` after all edits => Finished (see below).
- octet-coding-agent tests currently CANNOT build: `modes/interactive.rs` has
  the pre-existing external breakage `commands::Command::Fast(_)` not covered
  (another worker's `commands.rs` variant) AND must now cover the two new
  `InputAction::FocusGained`/`FocusLost` variants (tui3-owned file). Recorded,
  not fixed (outside editor3 paths).

- `$(date -u +%Y-%m-%dT%H:%M:%SZ)` ROW 2b.6 landed (cooperative): tui3 already
  wired `apply_focus_transition` for `InputAction::FocusGained`/`FocusLost` in
  `modes/interactive.rs` (lines 434/1927/1928), so the keymap translation is the
  missing half. Verified `cargo test -p octet-coding-agent --lib keymap`:
  `test result: ok. 31 passed; 0 failed` including all 10
  `tui::keymap::keybindings::tests::*` and
  `tui::keymap::tests::focus_transitions_translate_independently_of_key_state`.
  `cargo check -p octet-coding-agent --lib` is green.

## Final observed results (editor3)

- `cargo check -p sexy-tui-rs` => `Finished` (0 errors, 0 warnings from
  `text_editor*`).
- `cargo test -p sexy-tui-rs --lib` =>
  `test result: ok. 190 passed; 0 failed; 0 ignored` (includes the 40
  `text_editor` tests and 9 new kill_ring/undo/word_nav/prompt_zones tests).
- `cargo test -p octet-coding-agent --lib keymap` =>
  `test result: ok. 31 passed; 0 failed; 0 ignored` (includes all 10
  `tui::keymap::keybindings::tests::*` and the focus + queued-control tests).
- `cargo check -p octet-coding-agent --lib` => `Finished` (0 errors).

## Row status

- 2b.1 keybindings: LANDED (tests run).
- 2b.2 undo/redo coalescing: LANDED (tests run).
- 2b.3 kill-ring/yank/yank-pop: LANDED (tests run).
- 2b.4 word/line deletion + jumps: LANDED (tests run).
- 2b.5 OSC133 zones + prompt jumps: LANDED primitive (tests run); viewport
  wiring is view-owned.
- 2b.6 focus reporting + focus-out reset: LANDED (keymap translation tests run;
  tui3's `apply_focus_transition` is the consumer).
- #349 Alt+Up queue editing + Escape-while-active: LANDED (keymap tests run).
- 2c.3 LaTeX: BLOCKED. Exact missing primitive: a Rust port of
  `packages/tui/src/latex.ts` (`renderLatex`, 1394 lines: symbol tables +
  `LatexParser` + fraction/operator/matrix layout). No Rust equivalent in the
  workspace.
- 2c.4 Mermaid box-drawing diagrams: BLOCKED. Exact missing primitive: a
  terminal Mermaid graph-layout engine. Upstream depends on the external
  `grok-mermaid` package (`render` -> `MermaidArt`); nothing equivalent exists
  in the workspace.

START 2026-09-15T15:43:44Z editor5 alive

## editor5 ROW 2c.3 LaTeX port — LANDED (in progress notes)

2026-09-15T15:43Z..16:20Z editor5.

- Files: `crates/sexy-tui-rs/src/rich_text/latex/mod.rs` (new, port of the
  1394-line upstream `packages/tui/src/latex.ts`: symbol tables, `LatexParser`,
  fraction/operator/matrix layout), `crates/sexy-tui-rs/src/rich_text/latex/tables.rs`
  (new, 224 symbols + 88 relation commands + 32 named operators + sub/superscript,
  blackboard, negated, accent tables generated mechanically from upstream),
  `crates/sexy-tui-rs/src/rich_text/mod.rs` (`pub mod latex;`),
  `crates/sexy-tui-rs/tests/latex_render.rs` (new behavioral goldens).
- API: `sexy_tui_rs::rich_text::latex::render_latex(&str, RenderLatexOptions) -> Option<String>`
  — same contract as upstream `renderLatex` (`undefined` => `None`, never a panic).
- Commands run + observed output:
  - `cargo test -p sexy-tui-rs --test latex_render` =>
    `test result: ok. 11 passed; 0 failed` (144-case upstream
    `packages/tui/test/latex.test.ts` corpus + 5 fail-closed cases + 19
    display-mode/box-drawing cases captured from the upstream implementation).
  - Differential oracle: ran the real upstream `latex.ts` under Node 26 type
    stripping with only `utils.ts`'s `visibleWidth` shimmed (upstream tui deps
    are not installed), captured `(source, display, output)`; 278 hand-built
    edge cases + 600 randomized token-soup cases produced **zero** divergences
    from the Rust port (`cargo test -p sexy-tui-rs --test _latex_diff` ok).
    Harness lives in /tmp (not committed).
- Observed box-drawing output (from the oracle, asserted in the committed test):
  `\begin{pmatrix}a & b \\ c & d\end{pmatrix}` display =>
  `⎛ a │ b ⎞\n⎝ c │ d ⎠`;
  `x = \frac{-b \pm \sqrt{b^2-4ac}}{2a}` display =>
  `    -b ± √(b²-4ac)\nx = ──────────────\n          2a`;
  `\sum_{i=1}^{n} i = \frac{n(n+1)}{2}` display =>
  ` n      n(n+1)\n ∑  i = ──────\ni=1       2`;
  `\begin{pmatrix}界&a\\b&c\end{pmatrix}` display =>
  `⎛ 界 │ a ⎞\n⎝ b  │ c ⎠`.
- Port bug found + fixed by the oracle: the single-line
  `NEGATIVE_SPACING_COMMANDS` set was mis-parsed by the table generator
  (`\!` fell through to "unsupported"); `\det\!\left(...\right)` now renders.

START 2026-09-15T16:28:34Z editor6 alive

START 2026-09-15T16:50:13Z editor7 alive

START 2026-09-15T17:13:27Z editor8 alive

## editor8 — rows 2c.3 (finished) + 2c.4 (landed) — evidence

2026-09-15T17:13Z.. (editor8). Scope: `crates/sexy-tui-rs/**`,
`docs/parity/editor.md`, this file. No commits, no branch changes.

### Row 2c.3 LaTeX — finished, tested, documented

Files touched:
- `crates/sexy-tui-rs/src/rich_text/latex/mod.rs` — module docs rewritten
  (Contract / Supported / Fails-closed / Known difference / Tests), new
  `pub const MAX_LATEX_NESTING_DEPTH = 64`, `LatexParser` gained `depth`
  (propagated into `render_nested`), `parse_sequence` split into a
  guard + `parse_sequence_inner`, and `!self.supported` early-exits in
  `parse_command`, `parse_required_argument_value` and the
  `parse_sequence_inner` loop.
- `crates/sexy-tui-rs/tests/latex_render.rs` — 11 -> 16 tests: added
  `DISPLAY_LAYOUT_CORPUS` (34 captured display goldens), `INLINE_LAYOUT_CORPUS`
  (11), `UNSUPPORTED_COMMANDS` (10), `operator_limits_stack_over_their_operator`,
  `matrix_environments_draw_their_delimiters`,
  `display_layout_corpus_matches_reference_renderer`,
  `inline_layout_corpus_matches_reference_renderer`,
  `unsupported_commands_fail_closed_without_panicking`; rewrote
  `nesting_is_bounded_and_never_panics` around `MAX_LATEX_NESTING_DEPTH`.

Port bug fixed (this is the substantive change, not just tests): the recursive
parser had **no depth bound**, and its argument path re-entered `parse_command`
on the same unconsumed token after a failure, so `\frac{` x N recursively grew
with the input length. `cargo test -p sexy-tui-rs` was RED because of it:
`crates/sexy-tui-rs/tests/_latex_stress.rs::deep_braces` aborted the whole test
binary with `fatal runtime error: stack overflow` (SIGABRT). Measured:
`{`x500/1000 -> None (ok), `{`x2000 -> abort; `\frac{`x300 -> None, x500 ->
abort; `rav` recursion depth reached 469 with input depth 500 while the
parse_sequence depth counter read 64, which is how the second recursion path was
found.

Observed after the fix (`cargo test -p sexy-tui-rs --test _latex_stress
-- --nocapture`):

    braces depth=10000 => None
    env depth=2000 in 1.869458ms => None
    wide 16000 bytes in 2.381166ms => Some(15999)
    frac depth=10000 => None
    2000 fractions in 43.20075ms => Some(21996)
    test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

Observed (`cargo test -p sexy-tui-rs --test latex_render`):

    running 16 tests
    test case_environments_use_brace_delimiters ... ok
    test default_options_are_inline ... ok
    test matrices_align_columns_and_draw_delimiters ... ok
    test matrix_column_width_is_cell_correct_for_wide_and_combining_glyphs ... ok
    test inline_layout_corpus_matches_reference_renderer ... ok
    test unsupported_and_malformed_input_fails_closed ... ok
    test display_mode_stacks_operator_limits ... ok
    test operator_limits_stack_over_their_operator ... ok
    test display_mode_draws_stacked_fractions ... ok
    test unsupported_commands_fail_closed_without_panicking ... ok
    test matrix_environments_draw_their_delimiters ... ok
    test upstream_suite_failures_render_nothing ... ok
    test observed_upstream_output_matches_reference_renderer ... ok
    test display_layout_corpus_matches_reference_renderer ... ok
    test upstream_suite_corpus_matches_reference_renderer ... ok
    test nesting_is_bounded_and_never_panics ... ok

    test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

Observed box-drawing output newly asserted as goldens (captured from the
reference renderer, see the oracle note below):

    \prod_{i=1}^{n} a_i        -> " n\n ∏  aᵢ\ni=1"
    \oint_C \frac{dz}{z}       -> "   dz\n∮  ──\nC  z"
    \bigcap_{i \in I} B_i      -> " ⋂  Bᵢ\ni∈I"
    \max_{1 \le i \le n} x_i   -> " max  xᵢ\n1≤i≤n"
    \frac{\partial f}{\partial x} -> "∂ f\n───\n∂ x"
    \begin{bmatrix}a&b\\c&d\end{bmatrix} -> "⎡ a │ b ⎤\n⎣ c │ d ⎦"
    \begin{Bmatrix}a&b\\c&d\end{Bmatrix} -> "⎧ a │ b ⎫\n⎩ c │ d ⎭"
    \begin{vmatrix}a&b\\c&d\end{vmatrix} -> "│ a │ b │\n│ c │ d │"
    \begin{Vmatrix}a&b\\c&d\end{Vmatrix} -> "║ a │ b ║\n║ c │ d ║"
    \begin{pmatrix}1&2&3\\4&5&6\\7&8&9\end{pmatrix}
      -> "⎛ 1 │ 2 │ 3 ⎞\n⎜ 4 │ 5 │ 6 ⎟\n⎝ 7 │ 8 │ 9 ⎠"
    \int\limits_0^1 f(x)\,dx   -> "1\n∫ f(x) dx\n0"
    \int\nolimits_0^1 f(x)\,dx -> "∫₀¹ f(x) dx"

Differential oracle (re-run for this row, faithful this time): upstream
`packages/tui/src/latex.ts` copied to `/tmp/ed8/latex.ts` together with its real
`utils.ts` (`visibleWidth` backing `get-east-asian-width` from
`/opt/homebrew/node_modules`), driven by a Node runner. Corpora: 232 curated
real-LaTeX cases + 2626 randomized token-soup cases (astral code points
removed) + the 55 new golden cases = **2913 cases, 0 divergences**. The first
shim I used counted combining marks as width 1, which produced 2 false
divergences (`\frac{\hat{x}}{\vec{y}}`); with the real `visibleWidth` they
disappear — it was a harness bug, not a port bug. The 3000-case randomized run
including astral characters has exactly 11 divergences, all on inputs containing
non-BMP code points (JS splits UTF-16 surrogate halves; the port indexes
`char`s).

Fail-closed inventory stated honestly: `\cfrac`, `\genfrac`, `\cancel`,
`\phantom`, `\hspace`, `\xrightarrow`, `\verb`, `\def`, `\usepackage`,
`tikzpicture`, unknown commands, unbalanced groups and nesting past 64 all
return `None`. `\substack{i=1\\j=2}` renders as plain text rows and
`\text{a \textbf{b}}` renders `a b` — that is the reference behaviour, so the
port matches it rather than rejecting it.

### Row 2c.4 Mermaid — landed (bounded self-contained subset)

Files touched:
- `crates/sexy-tui-rs/src/rich_text/mermaid.rs` — module docs rewritten
  (Contract / Supported / Fails-closed / Bounds / Output and consumers / Tests);
  fixed `:::` class annotations (only two of the three colons were consumed, so
  `A[Foo]:::highlight` failed), rejected `BT`/`RL` instead of silently drawing
  them as `TD`/`LR`, made `---` draw a plain connector with no arrow head,
  accepted a trailing `;` on the header line, and added `row_to_line` so the
  covered cell of a double-width glyph is not emitted (a CJK label used to push
  a box's right border one column right).
- `crates/sexy-tui-rs/tests/mermaid_render.rs` (new) — 10 tests: 22 supported
  goldens, 16 fail-closed messages, width invariant, wide-label alignment,
  directed/undirected heads, link labels, LR vs TD layout, size limits, typed
  error variants.

Observed (`cargo test -p sexy-tui-rs --test mermaid_render`):

    running 10 tests
    test error_variants_are_typed ... ok
    test undirected_links_have_no_arrow_head ... ok
    test link_labels_sit_on_the_connector ... ok
    test directed_links_point_at_their_target ... ok
    test wide_labels_keep_box_borders_aligned ... ok
    test unsupported_input_fails_closed_with_a_typed_error ... ok
    test layouts_place_the_same_graph_differently ... ok
    test size_limits_fail_closed ... ok
    test supported_graphs_render_the_expected_box_drawing ... ok
    test every_rendered_row_fits_the_reported_width ... ok

    test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

Observed diagram output (pasted from the renderer, now golden-asserted):

    flowchart LR / A[Start] --> B[Done]
      ┌───────┐    ┌──────┐
      │ Start ├───▶│ Done │
      └───────┘    └──────┘
    graph TD / A[Start] --> B[Done]
      ┌───────┐
      │ Start │
      └───┬───┘
          │
          │
          ▼
      ┌──────┐
      │ Done │
      └──────┘
    flowchart TD / A[In] --> B{Valid?} ; B --> C[Store] ; B --> D[Reject]
      ┌────┐
      │ In │
      └──┬─┘
         │
         └─┐
           ▼
      ┌────────┐
      │ Valid? │
      └────┬───┘
           │
          ┌┘───────────┐
          ▼            ▼
      ┌───────┐   ┌────────┐
      │ Store │   │ Reject │
      └───────┘   └────────┘
    flowchart LR / A -->|start| B ; B --> C
      ┌───┐start  ┌───┐    ┌───┐
      │ A ├──────▶│ B ├───▶│ C │
      └───┘       └───┘    └───┘
    flowchart LR / A[One] --- B[Two]        (undirected: no head)
      ┌─────┐    ┌─────┐
      │ One ├────│ Two │
      └─────┘    └─────┘
    flowchart LR / A[界_x] --> B[y界]        (wide glyphs aligned)
      ┌──────┐    ┌─────┐
      │ 界_x ├───▶│ y界 │
      └──────┘    └─────┘

Fail-closed observed messages (exact strings asserted):
`dropped, unsupported diagram type: "pie"` / `"sequenceDiagram"` / `"BT"` /
`"RL"` / `"XY"`, `dropped, expected a graph or flowchart header`,
`dropped, line 2: expected a link, found "-- text --> B"`,
`dropped, line 2: node lists with \`&\` are not supported`,
`dropped, line 2: expected a link, found "S"` (subgraph),
`dropped, cycle through node "A"`,
`dropped, link A --> C spans 2 layers; only links between adjacent layers are supported`,
`dropped, line 2: unbalanced node label opened with \`[\``,
`dropped, line 2: trailing link without a target node`,
`dropped, line 2: unterminated link label`,
`dropped, diagram has more than 64 nodes`,
`dropped, line 2: label wider than 48 cells`,
`dropped, source is 51796 bytes, limit is 16384`.

Not modelled (stated, not pretended): shape outlines (diamonds/stadiums draw as
boxes), link stroke styling (`-.->`/`==>` draw a solid connector), subgraphs,
`BT`/`RL`, upstream's style-span + warnings channels and the upstream
"unrendered diagram" fallback text. No consumer is wired yet
(`crates/octet-coding-agent` has no `rich_text::mermaid` call site), so the
embedding component still has to map `Err` to that fallback.

### Row 2b.5 addendum

`docs/parity/editor.md` now records the exact view-side change required
(`view/viewport.rs` + `view.rs::scroll`/`scroll_lines` + `tui/keymap.rs`); no
view file was touched by this worker.

### Scratch harnesses on disk (untracked, throwaway)

`crates/sexy-tui-rs/tests/_latex_probe.rs`, `_latex_stress.rs`,
`_latex_debug.rs`, `_latex_diff.rs` (398 KB, from editor5), `_mermaid_debug.rs`.
Root's `git rm --cached` + `.gitignore` means they can never be committed again.
Only `_latex_stress.rs` is still useful (bounded stress evidence, quoted above);
the rest are disposable. My own probes (`_latex_ed8_probe.rs`,
`_mermaid_ed8_probe.rs`) were deleted. The valuable output of the 398 KB
`_latex_diff.rs` blob has been replaced by the bounded corpora in
`tests/latex_render.rs` and by the 2913-case `/tmp` oracle run recorded above.

### CHANGELOG-ready bullets

- Fix `sexy-tui-rs` LaTeX rendering so pathological nesting fails closed: the
  recursive parser now stops at `MAX_LATEX_NESTING_DEPTH` (64) and stops
  descending as soon as input is unrenderable, instead of exhausting the thread
  stack and aborting the process (`cargo test -p sexy-tui-rs` was red on
  `_latex_stress`).
- Add captured behavioral goldens for LaTeX operator limits, stacked fractions,
  roots and every supported matrix/cases/aligned environment (16 tests in
  `tests/latex_render.rs`), and verify the port against the upstream reference
  renderer over 2913 differential cases with zero divergences outside
  JavaScript's UTF-16 surrogate handling.
- Add a self-contained, bounded Mermaid `graph`/`flowchart` renderer
  (`sexy_tui_rs::rich_text::mermaid::render_mermaid`) that emits box-drawing
  diagrams for `TD`/`TB`/`LR` graphs with labels, branches and edge labels; it
  fails closed with a typed `MermaidError` for `BT`/`RL`, subgraphs, inline
  link labels, other diagram types, cycles, layer-skipping edges and oversized
  input, and aligns box borders correctly for CJK labels (10 tests in
  `tests/mermaid_render.rs`).
