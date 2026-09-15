# Qualification candidate: composer-ownership-full

## Scope and ownership contract

This change splits the former monolithic composer implementation while keeping
`crate::tui::composer::*` imports and the existing public composer API stable.
The ownership boundary is:

- `composer/attachments.rs`: extension classification, modality/size admission,
  atomic media batches, path-reference chips, attachment payloads, chip deletion,
  restoration, and ledger draining.
- `composer/paste.rs`: conservative shell-token/path parsing, explicit dropped
  paths, large-paste classification, and the absolute-path versus slash-command
  lexical handoff.
- `composer/picker.rs`: inline mention/path query interpretation, bounded
  workspace indexing, and immediate-directory path candidate discovery.
- `composer/composition.rs`: deterministic chip resolution into ordered
  `InputPart` values and transcript/display projections.
- `composer_surface.rs`: rendering, geometry, cache invalidation, and footer
  layout only; it does not admit attachments or drive picker events.

`composer.rs` is now a narrow facade that re-exports the former symbols. This
preserves callers in `view.rs`, `keymap.rs`, `interactive.rs`, and overlays
without widening the crate's public TUI module boundary.

## Preserved behavior

The split retains the existing behavior and limits:

- PNG/JPEG/GIF/WebP image and WAV/MP3/FLAC/Opus/AAC media classification;
- image and audio byte caps (`5 MiB` and `20 MiB`), modality checks, and provider
  audio-format gating;
- atomic media admission: all files are prepared before any ledger mutation,
  with input order and repeated paths preserved;
- explicit quoted, escaped, local `file://`, and `~/` path parsing, with a
  malformed or mixed prose payload rejected as an implicit attachment request;
- PDF path-reference chips, large-paste chips, Unicode-safe chip deletion, and
  right-to-left token replacement;
- ordered composition of text, expanded pasted text, path references, and media,
  including ledger draining and steering restoration payloads;
- mention substring ranking and bounded, hidden-file-aware path completion.

The focused fixtures cover extension and paste classification, atomic/ordered
batches, explicit path replacement, ordered composition, and mention/path
completion. The integration fixture compiles the sibling owner handoffs without
making the private TUI module public.

## Residual ownership and handoffs

Picker ownership is intentionally only split for inline composer discovery. The
model, resume-session, extension, and subagent picker event loops, secret-input
handling, fetching, mutation, cancellation, confirmation, and provider/session
flows remain in `tui/pickers.rs` and their existing callers. Fully relocating
those drivers would require edits outside the authorized paths, especially
`pickers.rs`, `view.rs`, keymap, interactive mode, and host integration; those
edits were not performed.

The existing view/keymap handoff remains authoritative:

1. keymap decides whether a path-looking input is distinct from a slash command;
2. view routes explicit paste text to `AttachmentLedger` and large text to its
   ledger chip;
3. overlays ask the composer facade for inline candidates;
4. selecting a media/PDF candidate calls the ledger, while selection and popup
   lifecycle remain view-owned;
5. submission calls `compose`, and interactive mode owns provider execution.

No event loop, secret-input path, model/session/extension/subagent flow, provider
capability contract, or public module declaration was changed.

## Verification boundary

All automated checks are **UNRUN** by constraint: tests, formatter, build/check,
lint, and git/worktree checks were not executed. The new Rust fixtures and source
organization are implementation evidence only, not a physical acceptance result.
Physical TUI acceptance remains required for explicit paste/drop admission,
Unicode chip deletion, path/mention popup handoff, picker-driver ownership,
provider modality errors, steering restoration, and ordered media submission.
