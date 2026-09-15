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

> **SUPERSEDED 2026-09-15 (editor5–editor11).** Both 2c.3 and 2c.4 are landed
> and tested; see the editor8/editor11 sections below and the landed sections
> in `docs/parity/editor.md` (2c.3 at line ~142, 2c.4 at line ~187 at HEAD
> `00e3ca3e`). The two bullets above are kept only as the original editor3
> record and must not be quoted as current status. A verifier reading this
> file's older line numbers should treat them as history.

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

### editor8 final verification runs (observed, not claimed)

`cargo test -p sexy-tui-rs` (whole crate, detached run, log
`/tmp/ed8/crate_test.log`) => `EXIT=0`:

    unittests src/lib.rs            -> test result: ok. 190 passed; 0 failed
    tests/_latex_debug.rs           -> ok. 1 passed
    tests/_latex_diff.rs            -> ok. 1 passed
    tests/_latex_probe.rs           -> ok. 1 passed
    tests/_latex_stress.rs          -> ok. 5 passed   (was SIGABRT before this row)
    tests/_mermaid_debug.rs         -> ok. 1 passed
    tests/images_current.rs         -> ok. 6 passed
    tests/latex_render.rs           -> ok. 16 passed
    tests/mermaid_render.rs         -> ok. 10 passed
    tests/pi_tui_render.rs          -> ok. 27 passed
    tests/rich_rendering.rs         -> ok. 4 passed
    Doc-tests sexy_tui_rs           -> ok. 1 passed

`cargo check --workspace --all-targets --locked` (log
`/tmp/ed8/workspace_check.log`) => `Finished dev profile ... in 2.58s`, `EXIT=0`.

Base revision for this work: `9c43111d` (wave 7). My files were swept into the
parent's `7be2dc96` "wave 8" checkpoint commit; I ran no git write commands
myself.

Still open for someone else: no UI consumer calls
`rich_text::mermaid::render_mermaid` or `rich_text::latex::render_latex` yet
(the markdown/rich renderer path in `crates/octet-coding-agent` owns the
`$$…$$`/```` ```mermaid ```` fences), and row 2b.5 needs the view wiring
described above.

START 2026-09-15T17:53:09Z editor9 alive

START 2026-09-15T17:55:33Z editor11 alive

## editor11 — row 2c.3 completion pass (in progress)

2026-09-15T17:55Z → (editor11). Scope: `crates/sexy-tui-rs/**`,
`docs/parity/editor.md`, this file.

Adoption notes: the briefing's C3 item (`docs/parity/editor.md:125-141` still
says 2c.3/2c.4 "Blocked") is already **fixed at HEAD `00e3ca3e`** — that text
only survives in this audit file's editor3 "Row status" block (lines ~142-149,
superseded below) and in `docs/parity/README.md:93-94` ("In progress", not an
editor-owned path). `docs/parity/editor.md` now has "Landed" sections at
lines 142 and 187 with module paths and test names; no stale claim remains in
the owned doc. I re-verified the code/tests rather than trusting that.

### Complete-by-construction differential sweep (new, this pass)

Built a corpus that mechanically enumerates *every* entry of the upstream
tables in `packages/tui/src/latex.ts` (read from the local upstream checkout at
`/Users/achumukundan/github/earendil-works/pi` @ `8a7b0c03`):
224 symbols, 88 relation commands, 32 named operators, 11 limit operators, 16
display-limit symbols, 30 negated symbols, 18 accents, 30 plain wrappers, 12
spacing commands, 4 negative-spacing commands, 6 ignored commands, 12 size
commands, 7 blackboard letters — each in inline and display mode, plus every
matrix/align/cases/equation environment with 1..3 columns, 1..3 rows, empty
cells, CJK cells, fractions/roots inside cells, nested environments, composed
`\left...\right`, and 36 malformed/fail-closed inputs.

Observed (scratch harnesses under `/tmp/ed11`, not committed):
- upstream oracle: `node --experimental-strip-types /tmp/ed8/oracle.ts
  /tmp/ed11/cases_gap.json /tmp/ed11/expected_gap.json`
- Rust probe: `cargo test -p sexy-tui-rs --test _latex_ed11_probe -- --nocapture`
- compare: `node /tmp/ed8/compare2.mjs ...` => `total=1061 divergences=0`
- per-table-entry corpus (403 cases): `total=403 divergences=0` — the three
  `a\negmedspaceb` / `a\negthickspaceb` / `a\negthinspaceb` inputs are `None`
  in *both* (the command name runs into the following `b`, so the reference
  refuses them); every other entry renders its reference glyph.

New committed behavioral test: `tests/latex_render.rs::TABLE_GOLDENS` (407
goldens: one per table entry + 7 spacing edge cases) asserted by
`every_symbol_table_entry_renders_its_reference_glyph`; the three malformed
negative-spacing inputs joined `UNSUPPORTED_COMMANDS`. Observed:

    cargo test -p sexy-tui-rs --test latex_render
    running 17 tests
    ...
    test every_symbol_table_entry_renders_its_reference_glyph ... ok
    test result: ok. 17 passed; 0 failed; 0 ignored; 0 filtered out

Box-drawing output re-observed this pass (`_latex_ed11_demo.rs`, deleted after
capture):

    x = \frac{-b \pm \sqrt{b^2-4ac}}{2a}  =>
        -b ± √(b²-4ac)
    x = ──────────────
              2a
    \sum_{i=1}^{n} i = \frac{n(n+1)}{2} =>
     n      n(n+1)
     ∑  i = ──────
    i=1       2
    \lim_{x \to 0} \frac{\sin x}{x} = 1 =>
         sin x
    lim  ───── = 1
    x→0    x
    \begin{pmatrix}1&2&3\\4&5&6\\7&8&9\end{pmatrix} =>
    ⎛ 1 │ 2 │ 3 ⎞
    ⎜ 4 │ 5 │ 6 ⎟
    ⎝ 7 │ 8 │ 9 ⎠
    \begin{cases}x^2 & x \ge 0 \\ -x & x < 0\end{cases} =>
    ⎧ x² if x ≥ 0
    ⎩ -x if x < 0
    \begin{pmatrix}界&a\\b&c\end{pmatrix} =>
    ⎛ 界 │ a ⎞
    ⎝ b  │ c ⎠
    \cfrac{1}{x} => None  (typed fail-closed; upstream `undefined`)

## editor11 — final verification, Mermaid completion, doc/CHANGELOG evidence

2026-09-15T17:55Z → 2026-09-15. Finished LaTeX first, then Mermaid.

### Row 2c.3 LaTeX — final state (no production-code change needed this pass)

The port already implements every command the upstream reference implements; the
only addition this pass is coverage: `tests/latex_render.rs` now has 17 tests
including `TABLE_GOLDENS` (407 goldens, one per upstream table entry) asserted
by `every_symbol_table_entry_renders_its_reference_glyph`, and three malformed
`\negmedspace`-style inputs joined `UNSUPPORTED_COMMANDS`. `docs/parity/editor.md`
2c.3 updated with the sweep + counts. No claimed gap is left open; unsupported
syntax returns `None` (upstream `undefined`) exactly as listed in the module docs.

### Row 2c.4 Mermaid — production-code completion this pass

`crates/sexy-tui-rs/src/rich_text/mermaid.rs`:
- `%%` comments are stripped anywhere on a line (outside quoted labels), not
  only at line start.
- `;` separates statements on one line and may share the header line
  (`flowchart LR; A --> B; B --> C`).
- quoted node labels may contain the closing delimiter (`A["a[b]c"]`,
  `A["100%% done"]`, `A["a;b"]`); quoted `|link labels|` lose their quotes.
- `subgraph`/`end`/`direction` now return the named typed error
  (`` `subgraph` statements are not supported ``) instead of being parsed as
  node ids; module docs gained the new Supported/Fails-closed rows and a
  "Not modelled" note (HTML entities are not decoded).
- `tests/mermaid_render.rs`: 10 -> 11 tests (7 new supported goldens, 4 new
  fail-closed messages, `random_token_soup_never_panics_and_stays_within_limits`
  = 1500 deterministic inputs that must not panic or exceed the documented caps).
- Scratch probe (`_mermaid_ed11_probe.rs`) deleted; no `_*.rs` scratch file was
  added to git.

Observed output (from the committed goldens; full run pasted below):

    flowchart LR; A[One] --> B[Two]; B --> C[Three]
    ┌─────┐    ┌─────┐    ┌───────┐
    │ One ├───▶│ Two ├───▶│ Three │
    └─────┘    └─────┘    └───────┘

    graph LR
      A[Alpha] --> B[Beta]
      A --> C[Gamma]
      B --> D[Delta]
      C --> D
    ┌───────┐    ┌──────┐     ┌───────┐
    │ Alpha ├───▶│ Beta ├────▶│ Delta │
    └───────┘│   └──────┘ │   └───────┘
             │            │
             │   ┌───────┐│
             └──▶│ Gamma ├┘
                 └───────┘
                  (committed `lr_diamond` golden, 7 rows)

### HARD GATES

- No network access, no new dependency: the Mermaid engine is unchanged in that
  respect (self-contained; `grok-mermaid` deliberately not added).
- No unbounded work: LaTeX bounded by `MAX_LATEX_NESTING_DEPTH` (64) and the
  early unsupported-exit; Mermaid bounded by `MAX_MERMAID_*` plus the 1500-case
  token-soup test.
- Clipboard/OAuth/trust/CBOR/unix-socket gates untouched; no changes outside
  `crates/sexy-tui-rs/**`, `docs/parity/editor.md`,
  `docs/swarm-audit/EXECUTION-parity-editor.md`.
- No git write commands run (no commit/branch/reset/stash/checkout). Scratch
  probes lived under `/tmp/ed11` and `tests/_*.rs` were deleted.

### 2b.5 view-side change (recorded, NOT edited — view/ is another worker's path)

Unchanged from the note in `docs/parity/editor.md:114-131`, re-verified against
HEAD `00e3ca3e`: `crates/octet-coding-agent/src/tui/view.rs:4350` is
`pub fn scroll`, `:4392` is `pub fn scroll_lines`, and
`crates/octet-coding-agent/src/tui/view/viewport.rs:46`/
`view/transcript_cache.rs:147` are the `max_scroll_from_bottom`/
`rendered_transcript` anchors. Exact change needed: (1) build
`PromptZones::scan(&*transcript_lines(state, width))` whenever the row cache is
rebuilt and keep it on `ShellState`; (2) add
`scroll_to_previous_prompt`/`scroll_to_next_prompt` next to `scroll`/
`scroll_lines` that convert `PromptZones::previous_prompt`/`next_prompt` for the
current visible top/bottom row into `scroll_from_bottom` with the existing clamp;
(3) bind the actions in `tui/keymap.rs`. `sexy-tui-rs` side is landed and tested
(`text_editor/prompt_zones.rs`, 5 tests).

### Out of this worker's paths (hand-off)

- `docs/parity/README.md:93-94` still lists 2c.3/2c.4 as "In progress" — a
  second stale ledger copy (not in the editor-owned path list). Someone who
  owns `docs/parity/README.md` should flip both rows to "Landed" with the
  module paths.
- No UI consumer calls `render_latex`/`render_mermaid` yet; the markdown/rich
  renderer in `crates/octet-coding-agent` owns the `$$…$$`/```` ```mermaid ````
  fences.

### CHANGELOG-ready bullets

- Cover every upstream LaTeX symbol/relation/operator/accent/wrapper table entry
  with captured goldens (`TABLE_GOLDENS`, 407 cases) and verify the port against
  the reference renderer over a complete-by-construction sweep of 1061 inputs
  with zero divergences (`cargo test -p sexy-tui-rs --test latex_render`:
  17 passed).
- Extend the self-contained Mermaid renderer with Mermaid-compatible statement
  handling: `%%` comments anywhere on a line, `;`-separated statements
  (including on the header line), quoted labels containing the closing
  delimiter, and quoted link labels; `subgraph`/`end`/`direction` now fail
  closed with a named typed error (`cargo test -p sexy-tui-rs --test
  mermaid_render`: 11 passed, incl. a 1500-input token-soup robustness test).
- Document rows 2c.3/2c.4 as landed-with-evidence in `docs/parity/editor.md`
  (module paths, test names, oracle sweeps, observed box-drawing output).

### Final observed runs (this pass)

    $ cargo test -p sexy-tui-rs          # log /tmp/ed11/crate_test.log
    EXIT=0
    unittests src/lib.rs        -> test result: ok. 190 passed; 0 failed
    tests/_latex_debug.rs       -> ok. 1 passed
    tests/_latex_diff.rs        -> ok. 1 passed
    tests/_latex_probe.rs       -> ok. 1 passed
    tests/_latex_stress.rs      -> ok. 5 passed
    tests/_mermaid_debug.rs     -> ok. 1 passed
    tests/images_current.rs     -> ok. 6 passed
    tests/latex_render.rs       -> ok. 17 passed
    tests/mermaid_render.rs     -> ok. 11 passed
    tests/pi_tui_render.rs      -> ok. 27 passed
    tests/rich_rendering.rs     -> ok. 4 passed
    Doc-tests sexy_tui_rs       -> ok. 1 passed

    $ cargo check --workspace --all-targets --locked   # log /tmp/ed11/workspace_check.log
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 32.75s
    EXIT=0       # the only warning is pre-existing: octet-coding-agent test
                 # "host_ownership_full" `missing_docs` (not this worker's file)

    $ cargo check -p sexy-tui-rs --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.39s

### Mermaid — observed output for the newly supported syntax (probe transcript)

From `tests/_mermaid_ed11_probe.rs` (scratch, deleted after capture; the same
strings are asserted in the committed goldens):

    == two_statement_semicolon: OK
    ┌─────┐    ┌─────┐    ┌───────┐
    │ One ├───▶│ Two ├───▶│ Three │
    └─────┘    └─────┘    └───────┘
    == inline_comment: OK            (flowchart LR\n  A[One] --> B[Two] %% trailing comment)
    ┌─────┐    ┌─────┐
    │ One ├───▶│ Two │
    └─────┘    └─────┘
    == label_with_brackets: OK       (A["a[b]c"] --> B[Two])
    ┌───────┐    ┌─────┐
    │ a[b]c ├───▶│ Two │
    └───────┘    └─────┘
    == quoted_two_word_link_label: OK (A -->|"two words"| B)
    ┌───┐two words  ┌───┐
    │ A ├──────────▶│ B │
    └───┘           └───┘
    == multi_branch: OK
    ┌───────┐
    │ Start │
    └───┬───┘
        │
        │
        ▼
    ┌───────┐
    │ Check │
    └───┬───┘
        │ yes
        │──────────┐
        ▼          ▼
    ┌──────┐   ┌──────┐
    │ Save │   │ Drop │
    └───┬──┘   └───┬──┘
        │          │
        └┐─────────┘
         ▼
    ┌────────┐
    │ Report │
    └────────┘
    == subgraph_after: ERR dropped, line 3: `subgraph` statements are not supported
    == direction_stmt:  ERR dropped, line 2: `direction` statements are not supported
    == end_statement:   ERR dropped, line 3: `end` statements are not supported
    == unterminated_quoted_label: ERR dropped, line 2: unterminated quoted label opened with `[`
    == quoted_label_mismatch:    ERR dropped, line 2: quoted label opened with `[` is not closed by `]`
    == self_loop: ERR dropped, cycle through node "A"
    == html_entity: OK (emitted literally — see "Not modelled")
    ┌───────────┐    ┌─────┐
    │ a &amp; b ├───▶│ Two │
    └───────────┘    └─────┘

### Final runs after every edit (fresh logs)

    $ cargo test -p sexy-tui-rs                 # /tmp/ed11/crate_test_final.log
    EXIT=0 : lib 190, latex_render 17, mermaid_render 11, pi_tui_render 27,
             rich_rendering 4, images_current 6, doc-tests 1, _latex_stress 5,
             _latex_debug/_latex_diff/_latex_probe/_mermaid_debug 1 each
    $ cargo check --workspace --all-targets --locked   # /tmp/ed11/workspace_check_final.log
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 38.82s, EXIT=0

START 2026-09-15T18:10:41Z editor11 alive (followup: fence wiring)

START 2026-09-15T18:21:36Z editor12 alive

## editor12: fence-dispatch consumer audit + empty-render fix (2026-09-15T18:4x Z)

ADOPT (never revert): predecessor's uncommitted `rich_text/latex/mod.rs` doc note was
kept and its broken intra-doc link fixed (`super::MAX_DIAGRAM_FENCE_BYTES` ->
`super::markdown::MAX_DIAGRAM_FENCE_BYTES`; the const lives in `markdown`, not
`rich_text`).

STATE FOUND: the fence consumer was already committed at HEAD in
`crates/sexy-tui-rs/src/rich_text/markdown.rs` (`render_diagram_fence`, called from
`Frame::Code` close for completed fences only) plus tracked goldens
`crates/sexy-tui-rs/tests/rich_fences.rs`. The parent brief ("no consumer") was stale;
wave-10 df2e8980 predates this session by ~3 minutes.

$ cargo test -p sexy-tui-rs --test _ed12_edges -- --nocapture   (/tmp/ed12/edges_before.log)
Found a real gap: a fence whose renderer SUCCEEDS but emits nothing collapses the block.
    latex "" / "\n" / "   \n" / "{}"    -> code "\n"        (source lost / empty block)
    mermaid "graph LR\n" (header only)  -> code "\n"        (source lost / empty block)

FIX `markdown.rs::render_diagram_fence`: after the match, `if rendered.trim().is_empty() {
return None; }` — an empty render is a failed render, so the original bounded source stays
in the plain code block. Module docs + `Frame::Code` comment updated to say so.

TESTS `rich_fences.rs`: +`renders_that_produce_nothing_keep_the_original_source` (7 bodies,
each compared byte-for-byte against the same body in a `rust` fence) and
+`streaming_keeps_failed_diagram_fences_as_source` (failed fence streams raw source, no
partial diagram, stable rows after the close).

$ cargo test -p sexy-tui-rs --test rich_fences                            (/tmp/ed12/rich_fences.log)
running 10 tests ... test result: ok. 10 passed; 0 failed
(was 8; +2 regression tests; existing latex/mermaid/streaming goldens all still pass)

CHANGELOG-ready: "Rich markdown: a fenced `latex`/`mermaid`/`graph`/`flowchart` block that
renders to nothing (empty expression, `{}`, header-only graph) now keeps its original source
instead of collapsing into an empty code block."

## editor12 verification runs (2026-09-15T18:28:09Z)

Files changed (all in exclusive paths):
- crates/sexy-tui-rs/src/rich_text/markdown.rs  (empty-render guard + docs)
- crates/sexy-tui-rs/tests/rich_fences.rs       (+2 tests, now 10)
- crates/sexy-tui-rs/src/rich_text/latex/mod.rs (adopted predecessor doc note, fixed intra-doc link)
- docs/parity/editor.md                          ($2c.4 "No consumer is wired yet" replaced with the landed consumer)

$ cargo test -p sexy-tui-rs --test latex_render --test mermaid_render   (/tmp/ed12/renderers.log)
running 17 tests -> test result: ok. 17 passed; 0 failed
running 11 tests -> test result: ok. 11 passed; 0 failed

$ cargo test -p sexy-tui-rs                                            (/tmp/ed12/crate_test.log)
lib 190 passed; _ed11_fence_probe 1; _ed11_probe 1; _ed11_stream_probe 1;
_ed12_edges 3; _latex_debug 1; _latex_diff 1; _latex_probe 1; _latex_stress 5;
_mermaid_debug 1; images_current 6; latex_render 17; mermaid_render 11;
pi_tui_render 27; rich_fences 10; rich_rendering 4; doc-tests 1.
0 failed anywhere (0 lines matching "test result: FAILED").

$ cargo check -p sexy-tui-rs --all-targets --locked                      (/tmp/ed12/crate_check.log)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 33.17s  => GREEN

$ cargo check --workspace --all-targets --locked                        (/tmp/ed12/workspace_check.log)
RED: 4 errors, all in crates/octet-coding-agent/src/app/bootstrap.rs
(let chains "only allowed in Rust 2024" x3 at :5611/:5612/:5884, E0027 missing
field `readiness` at :6667). bootstrap.rs is dirty from concurrent tui13/tui14
edits outside this worker's paths; NOT touched here. sexy-tui-rs itself checks
and tests green.

CHANGELOG-ready: "Rich markdown fences: ```latex```/```mermaid```/
```graph```/```flowchart``` blocks that render to nothing (empty
expression, `{}`, header-only graph) now keep their original source instead of
collapsing into an empty code block; the `docs/parity/editor.md` 2c.4 consumer
note is updated to match the landed fence dispatch."

START 2026-09-15T18:29:32Z editor12b alive

START 2026-09-15T19:02:46Z editor12c alive
