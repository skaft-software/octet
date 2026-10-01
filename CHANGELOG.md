# Changelog

## [Unreleased]

- Render octet's shell natively inside Tern. In a Tern pane
  (`TERM_PROGRAM=tern`; `OCTET_TUI_TERN=0` disables, `=1` forces) octet opens a
  Tern Surface Protocol surface from its render thread and draws its transcript
  (prompt cards, reasoning, tool cards with native diffs, shell output,
  outcomes, notices, compaction), composer and context meter with Tern's native
  components, wearing the resolved octet theme as the surface palette. A live
  surface replaces the pane's ANSI grid, so every other terminal keeps the
  existing renderer untouched. Terminal → program TSP messages are consumed by
  the input owner so they never reach the editor. Add `crates/octet-tern` (the
  TSP wire schema, APC framing, tty session, theme projector, scene builders)
  and document the protocol and mapping in `docs/tern.md`.
- Show `/hotkeys` as grouped key/description tables with readable key names
  (`Ctrl+B`) instead of raw binding ids; unbound ids are listed last. In Tern,
  Markdown reports such as `/changelog` and `/hotkeys` are typeset natively.
- Tern: draw user prompts as native cards so the fill never comes out ragged,
  keep native rendering past the second turn (turn-usage rows no longer reuse a
  node id), and stop a held Esc from leaking a Tern message into the editor.

