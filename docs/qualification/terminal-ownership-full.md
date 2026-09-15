# Terminal ownership qualification

Status: **UNRUN**. This document records the qualification contract; it is not
an execution report. No command, build, test, Git, network, or physical-terminal
check has been run for this candidate.

## Ownership boundary

| Responsibility | Owner |
| --- | --- |
| Public compatibility facade and startup OSC 11 gate | `tui/terminal.rs` |
| Capability probing and renderer-profile mapping | `tui/terminal/capabilities.rs` |
| Raw mode, keyboard enhancement, panic, and restoration | `tui/terminal/lifecycle.rs` |
| Render-only writes, synchronized frames, diagnostics, and image anchors | `tui/terminal/backend.rs` |
| OSC 11 filtering, bounded replay, and shared event ownership | `tui/terminal_input.rs` |
| Coordinated Unix shutdown and conventional signal status | `tui/terminal/signal.rs` |

The backend must not start a blocking input loop. Startup probing and interactive
input must use the same `TerminalInput` owner. SSH transport is not a color-depth
signal: remote `TERM`/`COLORTERM` remain authoritative.

## Qualification cells

| Cell | Intended evidence | Status |
| --- | --- | --- |
| Deterministic capability policy, including ANSI256/SSH separation | Unit tests in the terminal capability child module | **UNRUN** |
| Idempotent raw-mode, keyboard-enhancement, panic, and restoration cleanup | Lifecycle unit tests and PTY restoration checks | **UNRUN** |
| Render-only backend, CRLF normalization, synchronized-frame depth, and clear rendition reset | Backend unit tests | **UNRUN** |
| Image-anchor validation and bounded opaque protocol output | Backend/image unit tests | **UNRUN** |
| Fragmented OSC 11 replies, delayed replay, cancellation, and shared input | `terminal_input` tests | **UNRUN** |
| Ctrl-C/Unix signal cleanup and `128 + signal` status | `sigterm_shutdown.rs` and signal tests | **UNRUN** |
| Startup frame, resize, retained primary screen, and native scrollback behavior | `startup_frame_pty.rs` and PTY suites | **UNRUN** |
| Non-TTY startup and absence of interactive controls | `terminal_ownership_full.rs` | **UNRUN** |
| Ghostty ANSI256 local qualification | Physical observation | **UNRUN** |
| Terminal.app local qualification | Physical observation | **UNRUN** |
| Ghostty-to-SSH Ubuntu qualification | Physical observation with exact host/terminal identities | **UNRUN** |

The deterministic integration test only exercises the non-TTY help boundary; it
does not stand in for PTY or physical qualification.

## Required evidence when execution is permitted

Record the exact command, package/features, platform, terminal emulator and
version, `TERM`, `COLORTERM`, color mode, dimensions, and exit status for each
cell. Preserve raw PTY transcripts and terminal-attribute observations where
applicable. A missing physical observation remains **UNRUN**, not pass.
