# Activity current candidate

**Issue:** #346 readable Thinking/Working activity during held API waits
**Recorded baseline:** `73a80ada0c85b66e443c320703f03fa4924a1ae2`
**Candidate commit:** pending

## Source candidate

`reasoning_render.rs` now keeps activity styling foreground-only. Known dark
profiles use a contrast-safe light model-family baseline and a narrow darker
sweep; known light profiles use a dark baseline with the inverse lighter sweep.
The rainbow phase is constrained to the same profile direction. Unknown
backgrounds retain the existing neutral fallback because source code cannot
qualify contrast against both black and white without a real background sample.
The grapheme loop, marker/retry label phase, no-color fallback, and renderer's
monotonic 80 ms schedule are unchanged.

The deterministic renderer regression covers `Thinking`, `Working`,
`Compacting context`, and `Waiting for network` across TrueColor and ANSI256,
dark surfaces `(38,38,38)` and `(64,64,64)`, light surfaces `(245,245,245)`
and `(224,224,224)`, model-family choices, all label frames, and rainbow
strengths 0/50/100. It requires at least 4.5:1 contrast and rejects background
SGR. These are representative fixture surfaces, not evidence about an
arbitrary user's real transparent/composited terminal.

## Real-event-loop qualification

`tests/activity_wait_pty.rs` uses a fresh private HOME/workspace/session tree and
an ephemeral loopback HTTP server. The server sends response headers, emits no
provider-token events, and gates the finite SSE body. The test independently
holds ordinary `Working` and manual `Compacting context` responses, samples
frame count/palette changes, exercises local keyboard input and resize, cancels,
checks no duplicate request/stale activity, and verifies PTY line-discipline
restoration through the retained PTY master after the child exits. The parent
must not provide a terminal: the fixture opens its own PTY, gives the child a
session/controlling-terminal boundary, and retains the master for post-exit
inspection; an unwind guard kills and reaps an unfinished child. It also records
dark/light ordinary waits and a no-color static fallback. It does not use
credentials, personal sessions, live providers, or fault injection into a real
network.

## Qualification repair

The frozen run reached shutdown but called `tcgetattr` on the parent-held PTY
slave after the child session exited. macOS can revoke that slave at the
controlling-terminal session boundary, producing `ENOTTY` even though the PTY
master still exposes the terminal mode state. The repair reads the master while
preserving the existing `ICANON | ECHO` restoration assertion; it does not skip
or weaken restoration on `ENOTTY` and does not use the test runner's stdin.

## Checks

- Prior static checkpoint: `rustfmt --edition 2021 --check crates/octet-coding-agent/src/tui/view/reasoning_render.rs` — exit 0.
- Prior static checkpoint: `rustfmt --edition 2021 --check crates/octet-coding-agent/tests/activity_wait_pty.rs` — exit 0.
- Prior static checkpoint: `git diff --check` — exit 0.
- Cargo/rustc, the repaired binary PTY test, and post-repair formatter checks are **pending**: coding roots may not run them.

## Remaining qualification

No installed candidate or physical terminal transparency observation has been
made. After build admission, run the exact inherited-target commands recorded in
`RESULT.md`, including the focused renderer tests, this PTY test, and the
released held-activity PTY regression. Record exits, frame counts, idle bounds,
terminal sizes, and any delayed-success/timeout/transport-failure evidence
before treating #346 as qualified.
