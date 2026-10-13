# Octet end-to-end regression suite

One command runs real-binary interactive checks against a published octet build and
prints `PASS`/`FAIL`/`SKIP` per check:

```sh
scripts/e2e/run.sh                                   # default binary: ~/src/octet-release/bin/octet-latest
scripts/e2e/run.sh --binary path/to/octet            # any build
scripts/e2e/run.sh --checks core                     # U8 sweep checks only
scripts/e2e/run.sh --checks compat                   # Pi compat smoke only
scripts/e2e/run.sh --checks theme-direct,scroll-long # named checks
scripts/e2e/run.sh --list                            # list check names
scripts/e2e/run.sh --self-test                       # the suite's own terminal-model checks
```

Exit status is non-zero when any check fails. Other options: `--keep` (retain the
scratch home/workspace under the run directory), `--verbose` (evidence per check),
`--report report.json`, `--scratch DIR`, `--matrix DIR` (Pi matrix root, default
`~/src/octet-release/matrix`), `--timeout-scale S` for slow machines.

## What it drives

Every check spawns the **real binary** on a disposable PTY (120x40, `xterm-256color`,
`COLORTERM=truecolor`, light color scheme) with a scratch `HOME`, workspace, session
store, and extension directory. The PTY is observed through a bounded VT model
(`vt.py`: cursor addressing, erase/insert/delete, scroll regions, 24-bit SGR, OSC 52
clipboard, alternate screen, mouse modes). Input is sent as a terminal would send it,
including SGR mouse press/drag/release. The suite only ever signals processes it
forked itself: each session starts in its own session (`start_new_session`) and
shutdown targets that child's own process group under a liveness guard — never a
name or pattern match, `pkill` or `killall`.

A **mock provider** (`provider.py`) implements an OpenAI-compatible
`/v1/models` + `/v1/chat/completions` endpoint on loopback. No credentials, network,
or paid model is used. Replies are scripted per request content, stream chunk by
chunk, and can hold a turn open at a gate so "while busy" checks are deterministic
rather than timing-dependent. The three real Pi packages used by the compat smoke are
configured through `extensions/octet-pi-compat/configure.mjs --reviewed` into the
scratch extension directory; if Node, the adapter dependencies, or the matrix
packages are missing, those checks report `SKIP` with the reason. `compat-bundle`
loads the checkout's own bundled `extensions/` chain instead, needs only `node`,
and fails if the shipped bundle cannot start before any reviewed configure step.

## Checks and their U8 sweep lineage

| Check | U8 sweep origin | Asserts |
| --- | --- | --- |
| `startup` | sweep setup | splash, idle composer, clean Ctrl+D exit |
| `resume-continue` | #1 continue a long session | `--continue` restores the prior reply |
| `resume-picker` | #2 resume picker | bare `--resume` lists the named session, Enter restores it |
| `scroll-long` | #3 long transcript scroll | PageUp pins the viewport mid-stream, PageDown returns live, idle scroll reaches line 1 and back to line 200 |
| `selection-copy` | #4 mouse selection (U8-F2) | after `--continue`, a drag on a pinned middle row copies exactly that row through OSC 52 |
| `copy-slash` | #4 `/copy` | `/copy` puts the last assistant message on the clipboard |
| `theme-direct` | #6 direct theme selection (U8-F1) | `--theme Cards`, `/theme Still`, `/theme Cards` all apply and repaint; no "invalid theme" |
| `theme-picker` | #5/#6 theme picker | filtering to `Still` in `/theme` applies it immediately |
| `busy-slash` | #7 busy slash commands | `/model` and `/thinking` show a queued notice while streaming |
| `busy-extension` | #7 busy extension command | `/hello` queues while busy and runs at idle (legacy API 0.1 fixture) |
| `steer-enter` | #7 Enter steering | steering typed while busy reaches the provider and its reply renders |
| `followup-ctrls` | #7 Ctrl+S follow-up | the follow-up becomes a separate prompt after the run settles |
| `typing-latency` | #8 streaming typing latency | 15 draft echoes while streaming, max < 250 ms |
| `compat-off` | compat smoke | with the reviewed Pi material present but not enabled, no Pi command registers and no compat process runs |
| `compat-on` | compat smoke | `octet-pi-compat` runs with 3 real packages (`pi-ding`, `pi-powerline-footer`, `pi-hermes-memory`); `/ding`, `/powerline` and a memory command register; a prompt round-trip works |
| `compat-bundle` | U23 raw-bundle startup | with `--extension-dir <repo>/extensions`, the shipped `octet-pi-compat` bundle starts with zero reviewed factories, its picker row reports `running`, and a prompt round-trip works; `SKIP`s without `node` |

Known product failures are reported as `FAIL` and routed to the owning unit's
`STATUS/<id>.md` under `## R1 FEEDBACK` (for example U8-F1 direct theme selection is
owned by U14). The suite does not mask them; a release run is expected to be all
green once the owning units land.

## Limits (stated, not hidden)

- The PTY observer qualifies terminal-buffer visibility, mouse wire coordinates,
  OSC 52 payloads and 24-bit colors. It does not measure physical frame presentation
  or GPU compositing; U8's desktop screenshot limitation carries over.
- `typing-latency` measures time from writing bytes to the PTY until the character is
  visible in the observed screen, not end-to-end display latency.
- Tern `serve`/`ctl` (Tern 0.5.2 on this host) works headlessly, but octet negotiates
  Tern's native surface protocol inside a Tern pane, so Tern's `expect` sees no
  terminal rows and there is no clipboard readback; the suite therefore drives a raw
  PTY where every assertion has a wire-level source. Tern remains available for
  manual `tern serve`/`tern ctl run|tree|perf` sessions.
- The `compat-off`/`compat-on` checks trust `configure.mjs`'s reviewed capture for
  registration metadata; the runtime assertions are the extension panel, the composer
  command menu, a command execution, and a clean model round-trip. `compat-bundle`
  makes no registration claims: it asserts the raw bundle starts and reports
  `running`. Deep per-extension feature coverage stays U9's matrix.
- `--checks compat` requires `node`; the two reviewed-bundle checks also need the
  `--matrix` packages, while `compat-bundle` needs neither the matrix nor a reviewed
  capture. Missing prerequisites report `SKIP` with the reason.

## Layout

```
scripts/e2e/run.sh          one-command entry point
scripts/e2e/run.py          entry point
octet_e2e/vt.py             terminal model
octet_e2e/session.py        PTY session driver (keys, mouse, waits, OSC 52)
octet_e2e/provider.py       loopback mock provider + scripted replies
octet_e2e/fixtures.py       scratch homes, hello-world and Pi compat fixtures
octet_e2e/checks.py         core checks
octet_e2e/checks_pi.py      Pi compat on/off smoke
octet_e2e/runner.py         check registry, selection, reporting
octet_e2e/selftest.py       terminal-model and provider self-checks
```
