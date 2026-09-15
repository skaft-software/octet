---
name: interactive-testing
description: Test and debug octet's TUI in a controlled PTY or tmux session. Use for TUI behavior checks and interactive release smoke tests.
version: 0.1.0
required-tools:
  - read
  - bash
tags:
  - maintainer
  - tui
---
# Testing octet's interactive mode

Run repository commands from the repo root.

## Prefer the deterministic PTY lane

For anything that can be asserted on bytes or emulated terminal state, use the
existing lane instead of driving a human terminal:

```sh
scripts/test-startup-frame-pty.sh
```

Harness: `crates/octet-coding-agent/tests/startup_frame_pty.rs`; fixtures and
normalized contracts: `crates/octet-coding-agent/tests/fixtures/startup-frame-pty/`.
Read [docs/testing/startup-frame-pty.md](../../../testing/startup-frame-pty.md) for
what it covers (startup recovery, resize redraw, Ctrl-D teardown, alternate-screen
guarantees). It runs the compiled binary under a `vt100` emulator and needs no
live provider.

Other PTY lanes follow the same shape, for example
`crates/octet-coding-agent/tests/activity_wait_pty.rs`.

## Manual tmux session

Use tmux only when the question needs a real terminal or a human-visible
judgement.

```sh
tmux new-session -d -s octet-test -x 100 -y 30
tmux send-keys -t octet-test "cargo run -p octet-coding-agent --bin octet -- --mouse auto" Enter
sleep 3 && tmux capture-pane -t octet-test -p          # capture the startup frame
tmux send-keys -t octet-test "Say exactly: ok" Enter
tmux send-keys -t octet-test Escape                    # special keys; C-o for ctrl+o
tmux kill-session -t octet-test
```

Rules:

- Start the TUI from `/tmp` when smoke-testing a packaged release so it cannot
  resolve workspace files; use the absolute path to the release binary.
- Submit at least one prompt and wait for the model reply before calling an
  interactive smoke test passed. Startup alone is not a pass.
- Extended keys (`Shift+Enter`, `Ctrl+Enter`) need the tmux configuration in
  [docs/tmux.md](../../../tmux.md).
- Never leave a tmux session, child process, or stray browser running.

## Reporting

Record the exact command and the observed output. Capture issues as parity or
work-queue rows (`docs/parity/README.md`, `docs/swarm-audit/WORK-QUEUE.md`) rather
than treating every TUI observation as a regression.
