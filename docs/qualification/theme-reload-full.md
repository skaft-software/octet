# Theme reload qualification

## Purpose

This document records the qualification boundary for bounded active-theme
reload. It is a qualification of the scheduling/state engine, not a claim that
the end-to-end interactive feature is already enabled.

## Contract

An active file theme may generate reload work only when all of these hold:

1. the process is the interactive TUI;
2. the shell currently owns a `ThemeSource::File`;
3. a real `notify` watcher is attached nonrecursively to that file's parent;
4. the callback sends only bounded events through a nonblocking bounded queue;
5. the active path matches after lexical (not filesystem) normalization;
6. a debounce interval has elapsed; and
7. the shell is at an idle prompt boundary with no run or modal in progress.

The callback never reads or compiles theme content. The loader must remain
`OctetTheme::reload()` (`crates/octet-coding-agent/src/tui/theme.rs:589-601`),
which preserves the existing bounded, no-follow, regular-file check in
`read_theme_file_bounded` (`theme.rs:1615-1630`).

## Implemented qualification

`crates/octet-coding-agent/src/tui/theme_reload.rs` provides:

- a 64-entry `sync_channel` and a bounded per-tick drain;
- a 4096-byte path boundary;
- lexical normalization without `canonicalize` or symlink following;
- active-file filtering and parent-directory, nonrecursive watch specs;
- a 200 ms default debounce, capped at two seconds;
- interactive-only and file-source-only guards;
- idle-only request admission;
- generation/token invalidation for cancelled or stale completions;
- coalesced save bursts;
- last-good retention for invalid, unsafe, and other load failures;
- compiled-default fallback for missing and broken sources.

The isolated state tests in `tests/theme_reload_full.rs` cover filtering,
debounce/coalescing, busy-to-idle admission, mode/source guards, bounded
handoff/draining, cancellation, path changes, and broken/unsafe failure
classification.

## End-to-end status

The following acceptance items remain **not qualified in this checkout**:

| Acceptance item | Status | Reason |
| --- | --- | --- |
| Active edits update the running TUI | Blocked | No `notify` dependency and no interactive wiring |
| Atomic replacement/rename is observed | Engine-ready | Backend and shell adapter still required |
| Symlink/FIFO swaps are rejected | Loader-ready | Must be exercised through the existing secure loader |
| Reloads occur only at idle | Engine-qualified | Frontend must call the idle API at the prompt boundary |
| Print/plain mode stays inert | Engine-qualified | Frontend must not construct the adapter |
| No active file theme stays inert | Engine-qualified | Engine returns no watch spec without a path |
| Removed/broken source falls back safely | Engine-qualified | Frontend must install the fallback decision |
| Invalid source preserves last-good theme | Engine-qualified | Frontend must retain the non-applying decision |

`notify` is absent from `crates/octet-coding-agent/Cargo.toml` and
`Cargo.lock`. `crates/octet-coding-agent/src/tui/mod.rs` and
`crates/octet-coding-agent/src/modes/interactive.rs` are read-only under this
task. `HANDOFF.md` contains the exact dependency, watcher, and idle-loop
integration handoff.

The current `docs/themes.md` statement that arbitrary theme-file customization
is disabled is unchanged. The feature should therefore remain dormant until a
file `ThemeSource` can actually be selected by the frontend.

## Verification

Tests, builds, and other repository checks were not run because the task
explicitly prohibited shell/test/build/git tools. This document records the
unrun state rather than inferring a pass.
