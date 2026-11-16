# ANSI startup handoff audit

The application owns terminal IO; this is not an extension-adapter feature.
Raw-mode acquisition no longer clears the viewport. One first-frame transaction
owns visible terminal setup, alternate-screen entry, the resolved OSC 2 title,
chrome and the positioned hardware cursor. Nested renderer helpers share that
transaction; CSI 2026 itself is not a nestable terminal mode.

An unresolved first paint can coalesce until the existing 100 ms idle-poll
deadline. Native readiness, an edit, any published input, a dialog, an error or
close bypasses that deadline. Native readiness never waits for extension chrome.
This is a maximum budget for the provisional frame, **not** a minimum loading
time. Slow startup still paints its neutral editable composer; no provisional
model identity, extra animation or loading-time layout is introduced.

## Reproduction

Build two binaries outside the measured interval. On Linux with `strace`:

```sh
python3 scripts/bench-startup-handoff.py /path/to/before/octet-before target/ci-test/octet \
  --trials 5 --output /path/to/new-capture-directory
cargo build -p octet-coding-agent --profile ci-test --example startup-frame-audit
target/ci-test/examples/startup-frame-audit /path/to/new-capture-directory/octet-*.writes.json
```

Use different binary basenames. Each trial alternates binary order, uses fresh
credential-free homes and then repeats against the same homes. Cold means fresh
application state, **not** cold OS page cache. Both cells select an offline
manual loopback model and submit no inference. The extension-enabled cell runs
one API 0.4 executable fixture through initialization and session hooks; it does
not qualify Pi factory/package loading or an owner's extensions.

The runner redirects startup traces to separate stderr files and records every
stdout syscall via `strace`, following only octet's threads, not extension
protocol pipes. `*.writes.json` retains syscall timestamps and bytes. The Rust
VT100 auditor checks every physical write, including the first clear/entry,
complete resolved frame and application-handled edit. Strict auditing rejects
an unresolved first frame. For deliberately slow runs,
`--allow-initializing` additionally permits the unbranded editable composer,
never an empty clear or a regression after resolution. The PTY suite's gated
inventory tests separately verify pre-ready typing, paste, resize, refused
submission with preserved draft, later successful submission, Ctrl+C and Ctrl+D
restoration. No input or provider credential is inherited.

`frame.ready` is preparation, not output. The new first/editable/ready/settled
`*-written` phases follow completed ANSI transactions. `submission.ready` is
the first native admission boundary after extension startup settles;
`frame.settled-written` means loading chrome has cleared, not that the terminal
has displayed pixels or a tab heading. `phase_spawn_ms` aligns internal phases
to the externally observed `process.enter` stderr syscall; this includes the
small trace-write overhead. Native Tern pixels are outside this ANSI audit.

## Recorded result

[Numeric samples and binary hashes](startup-handoff.jsonl) retain all 40 runs:
five cold/warm pairs per binary, bare and extension-enabled, on Linux x86_64,
`ci-test` unoptimized builds, rustc 1.97.1. Strace and concurrent host work perturb
absolute times; these are not release-build latency claims. No runs are excluded.

| Cell | First frame before → after (ms, median) | Resolved frame before → after (ms) | Pre-ready transactions before → after |
| --- | ---: | ---: | ---: |
| Bare cold | 19.653 → 40.817 | 45.502 → 40.817 | 2 → 0 |
| Bare warm | 17.490 → 39.671 | 37.783 → 39.671 | 2 → 0 |
| Extension cold | 19.049 → 46.775 | 44.842 → 46.775 | 3 → 0 |
| Extension warm | 19.931 → 45.571 | 48.819 → 45.571 | 3 → 0 |

The resolved UI is the first paint in all 20 candidate captures; the strict
write-by-write VT100 audit passes all 20. The baseline auditor rejects the
unsynchronized initial clear. The candidate's native-ready first paint is not
held for the extension: its actual startup settles later (median submission
boundary 338.481 ms cold / 336.850 ms warm). Median complete-lifetime stdout
writes fall from 23–28 to 15–19, including the test edit and shutdown; edit
batching varies. No reduction in every wall-clock cell is claimed.

Baseline OSC 2 is already emitted **before** the resolved frame: median lead
1.203–1.454 ms across the four cells, outside synchronized output. The candidate
emits title and frame in the same syscall (0 ms separation). The reported
133 ms visual heading lag therefore cannot be attributed to a delayed octet
title emission from this evidence. No terminal implementation was changed.

## Auto appearance: removing a cosmetic readiness wait

After the coalesced ready frame, Auto appearance still awaited a missing OSC 11
reply for 120 ms before entering the first native submission boundary. Startup
now leaves late replies to the existing input owner. It resolves only pending
compiled-default Auto contrast, preserves the model accent and draft, and keeps
explicit contrast/file themes authoritative. There is no extra input reader,
network request, layout change or wait. Explicit interactive appearance selection
keeps its existing bounded probe; ordinary startup does not await it.

Reproduce with the binaries before and after that change:

```sh
python3 scripts/bench-startup-handoff.py /path/to/octet-auto-before target/ci-test/octet \
  --theme auto --trials 5 --output /path/to/new-auto-capture-directory
```

Auto mode deliberately sends **no** OSC 11 reply and waits for the recorded
`submission.ready` before typing, so the observation cannot accidentally shorten
the old wait by sending a key. Both binaries must expose that trace boundary.
[All 40 numeric samples and hashes](startup-auto.jsonl) retain five matched
cold/warm pairs per binary and activation cell, with the same fixture, build and
observer qualifications as above. All 20 new candidate captures pass strict
per-write VT100 auditing.

| Cell | Spawn → submission before → after (ms, median) | Prepared ready → submission before → after (ms) |
| --- | ---: | ---: |
| Bare cold | 169.475 → 45.392 | 123.173 → 1.346 |
| Bare warm | 166.790 → 45.927 | 122.809 → 1.424 |
| Extension cold | 335.154 → 342.012 | 291.405 → 291.382 |
| Extension warm | 292.571 → 337.307 | 243.084 → 294.794 |

The cosmetic timeout leaves the bare critical path. The extension cell remains
bounded by actual process initialization/hooks; its variation is retained, not
claimed as an improvement or hidden behind a longer first-paint delay. First
resolved paint remains 45–52 ms in these Auto medians. Late-reply tests verify
contrast changes preserve draft text, caret, revision and rendered text/geometry.
The gated PTY suite still covers slow discovery editing and cancellation.

## Owner real-terminal check (NOT RUN here)

1. Record the same pane geometry at high frame rate, bare and with the actual
   enabled Pi setup, once with fresh caches and once warm. Capture stderr with
   `OCTET_STARTUP_TRACE=1` and backend bytes with `OCTET_TUI_WRITE_LOG`.
2. Step shell → first UI frame: look for an erased blank pane, partial rails,
   wrong model identity, cursor jumps and welcome/footer geometry changes.
3. Align the first OSC 0/2 byte and CSI 2026 closing fence with the terminal's
   own received-byte/render/title events. If the title bytes arrive with the
   frame but the heading appears later, investigate terminal/multiplexer title
   throttling, synchronized-output handling and presentation independently.
4. Hold local discovery/extension initialization, type and paste immediately,
   resize, press Enter, and cancel with Esc/Ctrl+C/Ctrl+D. Check prompt text is
   preserved when submission is refused, editing stays live, no prompt is sent
   twice, and the shell's keyboard/cursor/mouse modes return on exit. Repeat
   direct and through the owner's multiplexer. These terminal pixels and real
   Pi packages are not established by the offline executable fixture.
