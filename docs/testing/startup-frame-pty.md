# Startup frame PTY lane

`startup-frame-pty` is a small Unix regression lane for octet's primary-screen
startup boundary. It runs the compiled `octet` binary under a controlled PTY and
checks the bytes and emulated terminal state rather than relying on a human
terminal or a live provider.

Run it from the repository root:

```bash
scripts/test-startup-frame-pty.sh
```

The lane uses Cargo's existing target directory. Set `CARGO_TARGET_DIR` before
running it when a shared build cache is required.

## What it covers

The real-binary contract runs twice, with `--mouse auto` and `--mouse app`:

- a 96x18 PTY is seeded with two `OCTET_PTY_STALE_STARTUP_*` rows before octet
  starts;
- the initial splash frame and the first ready frame are parsed through
  `vt100`; the visible stale rows must be gone;
- a controlled resize to 64x12 must produce a synchronized full redraw with
  `CSI 2J` and `CSI 3J`;
- Ctrl-D is the only supplied input. It must exit successfully and restore
  cursor visibility, bracketed paste, mouse modes, and termios state;
- no primary-screen scenario may enter an alternate screen. App mouse capture
  is required only for `--mouse app`.

The separate `legacy-inline` test drives the explicit `sexy-tui-rs` inline
scrollback compatibility path through the same PTY capture. It confirms that
its first paint preserves the pre-existing viewport in native history rather
than clearing saved lines, that shortening the transient fixture removes its
visible rows, and that it also avoids alternate-screen mode.

The expected normalized contracts and row fixtures are in
`crates/octet-coding-agent/tests/fixtures/startup-frame-pty/`. The harness is
`crates/octet-coding-agent/tests/startup_frame_pty.rs`.

## Model-discovery startup regression (unreleased)

The discovery tests hold a loopback `/models` response open while the real binary
starts, in both mouse modes. Before releasing the response they require:

- a synchronized startup frame, with terminal line echo disabled;
- visible typing, backspace, bracketed paste, and resize, without provisional
  model branding;
- Enter retaining the draft rather than submitting a provider request;
- Ctrl-C exiting within the existing shutdown bound and restoring terminal modes,
  even when the response is never released.

The successful-discovery case then releases the gate and checks that the same
draft survives into the resolved model frame. These are bounded regression
assertions, not latency distributions or a claim of faster provider discovery or
large-session replay. The composer is editable before submission is ready.

## Isolation and safety

Each real-binary run creates a disposable HOME, workspace, and session store,
with no inherited credentials. Ordinary startup cases use an empty API key,
`auto_discover: false`, and `http://127.0.0.1:9/v1/` as their unreachable base
URL. They run with `--offline`, `--no-context-files`, and `--no-tools`, submitting
no prompt. API-wait/plain-prompt cases use a gated loopback chat fixture instead
of a live model.

Only the discovery cases omit `--offline` and enable `auto_discover` against the
gated loopback fixture. No inference is submitted. The normal independent,
unauthenticated GitHub update check may also run; its success or failure is not
part of the test's assertions. No user credentials or live model are used.

The lane requires Unix `openpty` support. It gives the child a controlling TTY
(`setsid` plus `TIOCSCTTY`) so the resize signal and terminal-size handling
match an interactive shell.

## Optional v0.6.7 comparison

An explicitly selected local v0.6.7 binary can be compared without making it a
default test dependency:

```bash
OCTET_STARTUP_FRAME_BASELINE=/absolute/path/to/ygg-v0.6.7 \
  scripts/test-startup-frame-pty.sh --nocapture
```

The harness first verifies that the selected binary reports `0.6.7`. It then
prints the normalized byte/frame delta and requires lifecycle behavior that is
unrelated to the startup fix (synchronized resize replay, restoration,
alternate-screen policy, and mouse policy) to remain compatible. The expected
startup clear/stale-row correction is intentionally allowed to differ.
