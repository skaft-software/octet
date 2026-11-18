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
- a controlled resize to 64x12 must clear and replay the canonical frame inside
  synchronized output, with both `CSI 2J` and `CSI 3J`;
- Ctrl-D is the only supplied input. It must exit successfully and restore
  cursor visibility, bracketed paste, mouse modes, and termios state;
- no primary-screen scenario may enter an alternate screen. App mouse capture
  is required only for `--mouse app`.

The separate `legacy-inline` test drives the explicit `sexy-tui-rs` inline
scrollback compatibility path through the same PTY capture. It confirms that
its first paint preserves the pre-existing viewport in native history rather
than clearing saved lines, that shortening the transient fixture removes its
visible rows, and that it also avoids alternate-screen mode.

Screen-wait failures include the rendered screen, byte count, and a bounded raw
terminal tail. The startup and shutdown budgets are unchanged.

The expected normalized contracts and row fixtures are in
`crates/octet-coding-agent/tests/fixtures/startup-frame-pty/`. The harness is
`crates/octet-coding-agent/tests/startup_frame_pty.rs`.

<a id="model-discovery-startup-regression-unreleased"></a>

## Model-discovery startup regression

The discovery tests hold a loopback `/models` response open while the real binary
starts, in both mouse modes. Before releasing the response they require:

- a synchronized startup frame, with terminal line echo disabled;
- an immediately editable, neutral draft: typing, backspace, bracketed paste,
  and resize must paint without provisional model branding or composer rails;
- Enter retaining the draft rather than submitting a provider request;
- Ctrl-C exiting within the existing shutdown bound and restoring terminal modes,
  even when the response is never released.

The successful-discovery case then releases the gate and checks each completed
synchronized frame: composer rails stay absent until the welcome, permissions,
resolved model footer, and both rails appear together. The same edited draft
must survive that ready frame after resize. It then submits that draft and
requires exactly one loopback chat request with the intended text and model.
These are bounded regression assertions, not latency distributions or a claim of
faster provider discovery or large-session replay. The composer is editable
before submission is ready. Setup panels, credential prompts, and actionable
errors retain their input owners; quiet startup hides routine chrome, not setup.

Native Tern has complementary in-memory protocol/tree regressions in
`view/tern_tests.rs`: the real native editor is registered and focused before
readiness, accepts a fenced draft edit, remains unframed through resize and
identity resolution, and gains its rule and model controls in the same frame as
the welcome. Separate assertions retain credential/setup/error owners. The ANSI
readiness and lifecycle unit tests also check no pending rails, editable drafts,
and atomic ready chrome. These tests qualify projections, not desktop paint.

## Compact welcome regression

`real_octet_compact_welcome_survives_narrow_resize` checks both fullscreen and
inline startup, then resizes through 80, 40, 37, 36, 24 and back to 80 columns.
Composed VT cells must retain one exact 8×2 byte mark and one version label;
permissions and the changelog command must remain readable as details
move below the mark. Shutdown must restore termios and balance synchronized
frames. These are offline real-binary checks, not native Tern pixel acceptance.

The separate `view/welcome_card_tests.rs` unit module covers compact custom
themes, text/row-budget breakpoints, ASCII/no-colour output, and placeholder
geometry matching the painted card without hidden materialization.

## Static short-pane regression

Both mouse modes also run at 48x8 and 80x8 with truecolor enabled. The composed
VT screen must retain the permission disclosure and draft cursor row through
typing, backspace, and bracketed paste. After the edit, a 2.35-second observation
window must have no decorative frames and no `CSI 2J`/`CSI 3J` replay. The normal
resize contract above requires synchronized canonical clear/replay.

## Fullscreen mouse regression

The same harness also qualifies fullscreen gestures through the real binary:

```sh
cargo test --locked -p octet-coding-agent --test startup_frame_pty fullscreen
```

It checks proportional wheel bursts, scrollbar track presses and captured drags,
visible text highlighting, copy-on-release via OSC 52, draft preservation, and
terminal restoration. Coverage includes a fresh default fullscreen launch and
inline → fullscreen → inline → fullscreen transitions. Live transitions must
recompute `auto` capture, retain explicit `app` capture, and never capture for
`terminal` or `off`. The transition fence requires the resumed renderer's
completed synchronized frame as well as the intended terminal modes: leaving
fullscreen during suspension alone does not establish that input is ready.
The live-switch fixture keeps automatic reload enabled: a successfully applied
`/settings tui` write must not trigger redundant transcript hydration and reset
its viewport. Idle application acknowledges the exact checked write; active-run
application retains each receipt in order until idle ownership returns. Failed
switches and independent external edits remain visible to normal reload. These
receipt boundaries also have isolated persistence/idle/active unit regressions.
A fixture clipboard helper prevents macOS tests from replacing the user's local
clipboard. These are byte/VT regressions, not physical
mouse/touchpad or terminal clipboard-permission acceptance.

Link-click coverage runs both fullscreen and captured inline surfaces with an
OSC 8-capable profile and an Apple Terminal profile without OSC 8 support. It
requires one HTTP(S) opener handoff on stationary release, drag-copy instead of
activation, and no unsupported OSC 8 controls on the latter profile's wire.
The opener and clipboard helpers remain inert local fixtures, not native browser
or clipboard acceptance.

## Quiet automatic reload

The reload-details fixture opens the empty prompt catalog before writing its
external prompt. It sends no command during lifecycle ownership: resource
hydration dismisses the report, and a completed idle frame gates the next single
catalog request. The original 15-second reload deadline includes the loaded
catalog observation, capped at the original two-second request budget. There
are no command retries or deadline extensions. Explicit reload/details retain
their five-second budgets and use fresh completed-frame observations.
The existing 1.3-second initial observation remains a time-based baseline wait,
not a protocol receipt proving the first watcher scan completed.

## Explicit forced reload

```sh
python3 scripts/test-reload-pty.py --binary /absolute/path/to/octet
```

This separate Unix harness does not build or install anything. With disposable
HOME, workspace and session storage, it checks `/reload --force` against an
unchanged executable, an atomic replacement, a retargeted launch symlink, and a
retry after candidate-probe refusal, a missing binary, and an exec failure caused
by a removed private shebang interpreter. Each successful force must bootstrap a
fresh app in the same PID, resume exactly the original session, show its success
notice, accept editor input, and restore terminal modes on exit. Failures must
show a stage-specific error with a retry action, keep the editor usable, and
allow a successful retry. Neither outcome is written to session history. The
watcher is disabled so no automatic pass can substitute for the explicit command. Reports include the tested binary hash;
no live provider is called. Running-worker detachment is covered independently by
`running_host_worker_refuses_plain_reload_and_force_detaches_it` in the interactive
resource-reload tests. To test an upgrade from an older binary into a new build,
pass `--binary /path/to/old/octet --replacement-binary /path/to/new/octet --cases atomic symlink retry`;
the report identifies both images by hash. This does not qualify Windows or an
attended desktop.

## Isolation and safety

Resource-discovery fixtures contribute canonical directories: macOS temporary
paths can cross the `/var` symlink, which resource admission correctly rejects.
Resolving the fixture's existing directory keeps reload assertions independent
of that safety boundary without weakening production validation.

Each real-binary run creates a disposable HOME, workspace, and session store,
with no inherited credentials. Ordinary startup cases use an empty API key,
`auto_discover: false`, and `http://127.0.0.1:9/v1/` as their unreachable base
URL. They run with `--offline`, `--no-context-files`, and `--no-tools`, submitting
no prompt. API-wait/plain-prompt cases use a gated loopback chat fixture instead
of a live model.

Model-switch cases explicitly set `reload = false`: they test preservation of an
explicitly loaded resource diagnostic across compatible model changes, not across
an unrelated background resource reload that rehydrates the transcript. The
configured-model startup case and dedicated live-reload cases keep the watcher
enabled. This isolation does not qualify diagnostic retention across automatic
resource reloads. Optional model-switch traces are saved before visual assertions,
so a failed assertion does not discard the captured terminal stream.

Only the discovery cases omit `--offline` and enable `auto_discover` against the
gated loopback fixture. After discovery resolves, the successful case submits
only to that fixture and inspects its recorded request. The normal independent,
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
unrelated to the rendering fixes (synchronized resize frames, restoration,
alternate-screen policy, and mouse policy) to remain compatible. Startup
clear/stale-row correction intentionally differs from the old baseline;
structural resize retains canonical saved-line clear/replay.

## Native mark click regression

`real_octet_mark_click_blinks_then_settles_without_replay` clicks the default
mark in a truecolor fullscreen PTY, observes two full-height eye closures with
an unchanged baseline, and verifies the
canonical settled mark, unchanged cursor, no history clear, and no continued
animation frames for over five seconds after settling. Unit tests cover bounded repeated-click rainbow strength,
settlement, stale resize geometry, reduced motion, lower colour capabilities,
and preservation of explicitly themed hues. These are terminal-byte/VT checks,
not attended visual acceptance.

Blinks ease through partial-height blocks in the two upper eye cells. On a single
click the right eye trails the left by 80 ms, and each eye holds closed for 80 ms
before a slower reopening. Unit tests check the exact single-click shape and
offset, fixed baseline and six full-height bars, and canonical rest geometry.
At the renderer's 40 ms cadence, faster clicks produce more blinks, slower clicks
reduce the rate, and in-flight blinks finish without resetting on a click.
Blinking is click-only: the welcome does not schedule idle animation.

Rainbow regressions check left-to-right colour travel, seamless wrapping,
continuity of hue and blend across clicks, faster cycling at higher click cadence,
and deceleration as clicks slow or stop. Cycling speed is bounded at three
revolutions per second; no frame queue or physics runtime is used.
