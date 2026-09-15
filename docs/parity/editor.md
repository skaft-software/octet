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

## Roadmap #349 Alt+Up queue editing — Landed (keymap layer)

- `crates/octet-coding-agent/src/tui/keymap.rs`: Alt+Up returns
  `InputAction::EditQueued`; Escape while a turn is active returns
  `InputAction::DispatchQueued`; Enter-with-modifier returns
  `InputAction::Queue`; Alt+Up is a reserved extension shortcut. The shell
  (`modes/interactive.rs`, another worker) already consumes these variants.
- Tests: `queued_controls_are_one_shot_and_slash_escape_keeps_ownership` plus
  the updated queue/dispatch expectations.

## 2c.3 LaTeX rendering — Blocked (missing primitive)

Upstream `packages/tui/src/latex.ts` (1394 lines: symbol tables, a
`LatexParser`, fraction/operator/matrix layout nodes, and `renderLatex`) has no
Rust equivalent in the workspace. A faithful port is a dedicated deliverable;
a partial port would misrender unsupported input instead of returning
`undefined`. Not attempted here.

## 2c.4 Mermaid box-drawing diagrams — Blocked (missing primitive)

Upstream
`packages/coding-agent/src/modes/interactive/components/mermaid.ts` delegates
the actual diagram layout to the external `grok-mermaid` package
(`render` → `MermaidArt`) and then re-emits styled rows as Markdown code spans.
No Rust Mermaid graph-layout engine exists in the workspace; the row requires a
new primitive (Mermaid source → styled box-drawing art with width/warning
metadata).
