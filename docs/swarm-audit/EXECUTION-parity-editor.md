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
