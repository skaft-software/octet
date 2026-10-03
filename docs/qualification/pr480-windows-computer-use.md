# PR 480: native Windows computer-use setup qualification

## Scope and build

Agent-driven computer use exercised the real terminal UI, using the reviewed
extension handlers and screenshots before/after input. This is not a human
acceptance sign-off, a release receipt, or qualification of macOS/Linux parity.

- PR head: `a23543de6a1e99681623254072cbb1a3e975a498`, checked against the
  remote PR ref. A separate worktree preserved the existing dirty checkout.
- Native x64 Windows `10.0.26200.0`, Windows Terminal `1.24.12741.0`,
  PowerShell `5.1.26100.9444`, Python 3.14, Rust 1.88.0.
- `cargo build --locked -p octet-coding-agent --bins` succeeded with MinGW-w64
  on `PATH`; the tested debug frontend reports octet 0.8.2. The local fix changes
  Python, not the native binary, and was reloaded with an explicit source root.
- Python/driver state was isolated in paths containing spaces and Unicode
  (`.local/New User ü`, `.local/Cancel User ü`, and `.local/Offline Index ü`).
  **Native octet was not isolated:** its config and installed extensions use the
  Windows known-folder profile, irrespective of `USERPROFILE`/`HOME`. Existing
  credentials were not moved or replaced. A fresh-account frontend/provider
  wizard remains untested.

## Observed results

Local source/test/README edits after launch caused the frontend to refuse reload
with “extension runtime source changed”. Restarting against the final reviewed
source passed **Check status** and restored **Ready**; restart after source edits.

| Path | Result |
| --- | --- |
| Official `extension install octet-computer-use` | HTTP 404: the 0.8.2 extension release asset is unpublished. |
| Install a locally built baseline archive, then list extensions | Installed and listed 0.8.2 successfully. This does not validate public distribution. |
| `/extensions` activation and setup | Menu opened; fresh setup showed live Python/venv/package-install progress, installed Driver 0.33.0 and 24 bundled theme variants. Spaces/Unicode in the Python state path worked. |
| Baseline post-setup status | Incorrectly reported “Needs a non-elevated target” and recommended setup again, despite working desktop actions. |
| Patched fresh setup and **Check status** | Self-check OK; header **Ready**; report says Windows needs no separate grant and names elevated-window/secure-desktop restrictions. |
| **Set up again** | Completed without a session collision; driver binary modification time and theme-file count stayed unchanged. |
| Decline optional Jev | Setup completed and reported that computer use was unaffected. |
| Dedicated Jev setup, then Esc at the secret prompt | Optional SDK installed; report explicitly said no API key was set. No credential was entered or live Jev request sent. |
| Running-step cancellation | Baseline UI said cancelled while pip remained alive. Patched setup returned to **Not set up** and the fixture's pip processes were absent one second later. Native tests also pin process handles and verify both parent and child terminate on cancellation/timeout. |
| Unreachable index and recovery | A private `pip.ini` pointing at a loopback timeout produced an actionable setup/index failure, not an internal error. Removing that fixture override and retrying in the same frontend completed with **Ready**, reusing neither an unfinished request nor a broken installation. |
| Windows Terminal input | Background text was refused. Several PostMessage keys/chords reported an unverified effect without changing the display; returned press-key foreground escalation and fresh observations were needed. Foreground typing/navigation worked. |
| Update from a patched local archive | Failed: Windows has no atomic directory exchange. The previous installation remained byte-identical; the candidate was tested with `--extension-dir`, not installed as an update. |
| Cursor | Direct runtime reports no available agent-cursor overlay. Theme files are not evidence of a visible personalized cursor. |

## Fix and regression evidence

Windows `check_permissions` rejects the macOS-only `prompt` argument. With
empty arguments the live driver returned `uia: true`, `post_message: true`,
`elevated: false`, and an unavailable integrity-token reading. The latter is
not evidence that desktop automation is unavailable.

The wrapper now dispatches Windows to its own argument-free probe, interprets
explicit interface booleans, and leaves incomplete/refused/failed responses
unknown. Target elevation and secure-desktop restrictions remain separate from
session readiness. Four probe tests and one post-setup menu regression cover
this change. Provisioning now checks the SDK request cancellation token and uses
the bundle's existing process-tree supervision while the request is active:
on Windows it starts suspended, assigns a kill-on-close job, resumes, polls
cancellation, and reaps on cancellation or timeout. Three more regressions
cover pre-cancelled requests, UTF-8/detached stdin, and real Windows parent/child
termination. The combined permission/menu/provisioning/stdin selection ran
**34 tests, all passing**. This is not qualification of the Windows Jev recipe.

Full native extension-suite results:

- Unmodified baseline: **353 tests**, 17 skips, 1 failure, 2 errors.
- Patched source: **361 tests**, 17 skips, the same 1 failure and 2 errors.
- Both symlink-fixture errors are `WinError 1314` on this non-elevated account.
  The failure is a macOS archive fixture expecting Unix executable mode bits
  from a Windows file. These were reproduced on the baseline, not suppressed.

From `extensions/octet-computer-use`, with `PYTHONPATH=.;vendor`, reproduce:

```console
python -m unittest discover -s tests -v
python -m unittest tests.test_computer_use.WindowsSessionProbeTests tests.test_computer_use.PermissionProbeTests tests.test_computer_use.LinuxSessionProbeTests tests.test_computer_use.OptionsMenuTests -v
python -m unittest tests.test_setup_cancel tests.test_driver.ProbeIsolationTests tests.test_driver.HostedStdinTests -v
```

## Remaining gates and retained local evidence

A polished public Windows new-user path is **not complete**. It still needs
published version-matched assets, a safe supported extension-update design,
and a disposable Windows account for native first-run/config/provider testing.
The cancellation/retry result covers the Windows pip/venv subprocess path, not
the macOS host download or Linux desktop helpers. Elevated targets, UAC/lock
screens, Windows ARM64, conhost, live paid providers/Jev, and other desktop
platforms were not exercised.

The tester retained ignored local evidence under `.local/`: build log
`pr480-cua-build.log`, `final-asset-check.json` (final asset HEAD request: HTTP
404), suite logs `computer-use-baseline-tests.log` and
`computer-use-tests.log`, final source-matched fixture archive
`octet-computer-use-candidate-0.8.2.tar.gz` (not installed over the baseline),
targeted logs `computer-use-permission-menu-tests.log`,
`computer-use-cancel-tests.log` and `computer-use-focused-tests.log`,
`patched-live-status.json` (public status tool: self-check OK, permissions
granted, cursor unavailable), and `setup-ui-evidence.jsonl`
with action results and screenshots in `cua-control/`. Useful captures include
61 (failed update), 113 (Ready menu), 117 (status), 131 (repeat setup), 150–152
(secret prompt and cancellation report), 188–196 (fresh patched setup),
269–272 (baseline cancellation with remaining processes), and 304–325
(patched cancellation, index failure, retry and Ready), and 365/367 (final-source
restart, status self-check and Ready). The patched process
query is retained in `cancel-processes-patched.json`. These local artifacts are
not committed or published, and their retention is not a release approval.
