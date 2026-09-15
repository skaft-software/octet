# Qualification candidate: #381 prompt-history-current

## Scope and implementation contract

Issue #381 requests shell-like Up/Down recall of sent prompts while the current
interactive TUI session is alive. The implementation keeps a process-local
`ShellState.prompt_history` with a hard bound of 100 entries
(`MAX_PROMPT_HISTORY_ENTRIES`). It records a nonempty composed submission only
after `Agent::prompt`/`prompt_without_tools` succeeds
(`crates/octet-coding-agent/src/modes/interactive.rs:5817-5881`), so it is not
cross-session persistence, durable session history, or terminal scrollback.
The local capture and bounded trim are in
`crates/octet-coding-agent/src/tui/view.rs:3316-3342`.

## Key behavior at document boundaries

- With the ordinary editor focused, no extension autocomplete result visible,
  no panel/overlay/tool prompt, and the run idle, Up at absolute composer
  cursor 0 recalls the newest retained prompt. Repeated Up walks toward older
  entries; the oldest entry clamps (no wrapping).
- While browsing, Down walks toward newer retained prompts. Down from the
  newest retained prompt exits browsing and restores the pre-browse draft,
  including an empty draft. Repeated boundary presses do not submit or wrap.
- Outside an active history traversal, Up/Down retain normal multiline editor
  movement. In particular, arrows away from the absolute text boundary do not
  start history browsing. Active runs also do not enter history traversal.
- Recalled text is an editable copy. Ordinary edits clear the traversal state;
  stored entries are not mutated. Enter is still the separate submit action.

The precedence is implemented by `apply_edit` at
`crates/octet-coding-agent/src/tui/view.rs:3427-3449`: a visible host path/
mention menu claims Up/Down first; only then can idle prompt history consume the
arrow. Slash-command popup actions are translated before `EditAction`, and
extension autocomplete, panels, overlays, and tool prompts retain their own
ownership. This preserves the documented picker behavior in
`docs/commands.md:77-96` and `docs/terminal.md:38-54`.

## Draft, media, and paste fidelity

History entries retain the exact composer projection (`display_text`), including
collapsed-paste/attachment masks, plus the attachment payloads associated with
those masks. Before the first history transition, the draft snapshot takes the
exact display text, byte cursor, and pending attachment ledger; returning past
the newest entry restores all three (`view.rs:1144-1237`). Recalled entries are
restored as independent copies, and `drain_composed` resolves their chips back
to ordered input parts and payloads rather than sending literal placeholder
text (`view.rs:4137-4151`). Explicit paste remains explicit attachment consent;
ordinary typed paths remain text, consistent with `docs/media.md:5-38`.

History navigation itself only edits/restores the composer and renders it. The
interactive loops apply `InputAction::Edit` and render at
`interactive.rs:337-365` and `interactive.rs:1518-1524`; only the separate
`InputAction::Submit` path drains a composed input (`interactive.rs:424-425`).
Thus merely pressing Up/Down never sends anything. While work is active, Enter
keeps its existing follow-up queue behavior and Ctrl+S keeps its existing
steering-at-the-next-model-boundary behavior; history is deliberately gated to
idle state. These queue/steering controls take precedence over history and are
not reinterpreted as recall.

The two named media helpers, `media_kind_for_path` and `prepare_media`, are the
identical media helpers already integrated in
`crates/octet-coding-agent/src/tui/composer.rs:44-58,539-592`. They are existing
overlap, not new prompt-history work and not evidence of a separate history
feature.

## Source regression fixtures

The current view tests use `InteractiveShell::test_shell` and cover:

- `prompt_history_repeats_with_bounds_and_restores_an_empty_draft`
- `prompt_history_keeps_multiline_motion_away_from_text_boundaries`
- `prompt_history_editing_does_not_mutate_the_recalled_original`
- `prompt_history_preserves_collapsed_paste_masks_and_payloads`
- `prompt_history_restores_the_draft_cursor_and_payload_at_newest_boundary`
- `prompt_history_is_bounded_to_recent_successful_prompts`

The fixture assertions cover repeated navigation, oldest/newest non-wrapping,
normal multiline motion, edit/resubmit isolation, collapsed-paste payload
association, exact cursor/draft restoration, empty-draft restoration, and the
100-entry bound.

## Verification and acceptance boundary

All automated checks are **UNRUN** in this source-only phase: Rust tests,
formatter, build/check, the lone Rust verifier, and the separate non-Rust
verifier. No command or physical TUI/provider test was executed. The listed
source and test fixtures are implementation evidence only; source-code files
are **not** a physical acceptance result. Physical interactive acceptance is
still required, including actual picker ownership, active queue/steering
precedence, no implicit submission, multiline boundary behavior, attachment
payload fidelity, and current-session-only lifetime. No source commit was made.
