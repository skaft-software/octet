# Local idle model-switch probe, 2026-10-22

**Synthetic work-reduction evidence, not Pi parity or release approval.**
The latest release remains 0.9.6. The baseline production source is
`699ef867c194bf83fd69896ecede02b534b66a8b`; the candidate is the performance change
containing this record. Pin a published full candidate SHA before qualification;
this record does not supply one. See the [measurement contract](../../design/performance.md)
and [isolated Mac qualification recipe](../../maintainers/pi-v0.10-candidate.md).

## Boundary and setup

The ignored `profile_idle_model_switch` test uses the real generated Pi adapter
and Node process, with one reviewed synthetic factory. The factory records its
PID each time it executes and registers no-op `session_start` / `model_select`
handlers. Configuration preflight is excluded from the factory counter.
After startup, ten consuming `apply_reconfig(Reconfig::Model(...))` calls alternate
`gpt-audio-1.5` and `gpt-4o-mini`. Each call verifies the selected model. No provider
request, credential grant, or populated installed Pi setup is involved.

The timer is an `Instant` around reconfiguration, including final trace emission.
This is not the complete interactive picker, key delivery, model-select hook
completion, terminal paint, or first-editable-frame journey. Before includes the
old host's teardown, catalog/resources/session rebuild and replacement adapter
handshake. After uses guarded live selection and its durable configuration append.

Both platforms ran on one Intel i5-13600KF host (14 cores / 20 logical processors),
using Rust/Cargo 1.88.0, the dev profile with `CARGO_PROFILE_DEV_DEBUG=0`, and
`OCTET_STARTUP_TRACE=1`. Windows used Node 24.15.0; WSL2 Linux used Node 22.22.1.
Locked adapter dependencies were installed using
`npm ci --ignore-scripts --no-audit --no-fund`.

These are ten sequential switches inside one App per cell, **not ten independent
process trials**. Startup precedes the timed window. Disk/JIT caches were not
purged, trial order was before then after, and concurrent build activity was not
controlled. Do not estimate tail latency, compare the operating systems, or infer
a general speed ratio from these samples. Baseline timing used a Windows Git
archive with checkout line endings; subsequent Linux correctness suites used an
LF archive. This is another reason to treat counts, not timing ratios, as the
stronger evidence and rerun a matched release-profile campaign on the final pin.

## Results

Raw, ordered samples and capture hashes are retained in [samples.json](samples.json).
The median is the average of the two middle sorted observations; units below
are milliseconds. Factory counts include application startup.

| Platform | Before median (range) | After median (range) | Factory loads before → after |
| --- | ---: | ---: | ---: |
| Windows | 2847.399 (2797.540–3085.446) | 1.351 (1.198–2.116) | 11 → 1 |
| WSL2 Linux | 415.552 (391.639–451.358) | 3.562 (2.656–4.484) | 11 → 1 |

The host trace shows eleven handshake requests before and only the startup
handshake after. Adapter `factory.import.0` / `factory.registered.0` marks likewise
repeat before but not during after-switch windows. Adapter elapsed values restart
with each process; never subtract them from host elapsed values.

For the first Linux baseline switch, host `model-switch.begin` was 403794 µs,
`extensions.handshake.begin` 422354 µs, `extensions.handshake.ready` 788633 µs,
and `model-switch.end` 794785 µs. The separately measured process initialize step
and adapter marks attribute replacement-process work, not a slow no-op factory
body. Windows also has substantial pre-handshake teardown/rebuild work; do not
attribute its whole reduction to JavaScript import. The after trace instead shows
`model.select.begin` / `model.select.complete`, with no replacement handshake.

The executable regression test asserts exact PID/load-log preservation, session
identity, and shared resource identity, without a wall-clock threshold. Other
regressions cover durable selection/history, output/compaction settings, prompt
metadata, scoped/portable effort, Ultra observation, service tier, incompatible
tool capability fallback and validation failures. The interactive transition
checks draft/resource preservation and terminal ownership restoration while the
existing extension instance and command service remain alive.

## Reproduce

In a disposable checkout with locked dependencies and a working Node runtime:

```sh
export CARGO_PROFILE_DEV_DEBUG=0
OCTET_STARTUP_TRACE=1 cargo test --locked -p octet-coding-agent --lib \
  profile_idle_model_switch -j 3 -- --ignored --nocapture
unset OCTET_STARTUP_TRACE
cargo test --locked -p octet-coding-agent --lib idle_model_switch -j 3
```

For the before measurement, create a separate worktree at the baseline SHA and
copy **only the diagnostic harness**, not the candidate's production code or
regression cases, into it. This example takes the candidate test source as the
first argument and the baseline checkout as the second:

```sh
python3 - /absolute/candidate/crates/octet-coding-agent/src/app/bootstrap/tests/idle_model_switch.rs /absolute/baseline <<'PY'
import pathlib, sys
source = pathlib.Path(sys.argv[1]).read_text()
root = pathlib.Path(sys.argv[2]) / 'crates/octet-coding-agent/src/app/bootstrap/tests'
target = root / 'idle_model_switch.rs'
modules = root / 'mod.rs'
assert not target.exists()
assert 'mod idle_model_switch;' not in modules.read_text()
helper = source.split('\n#[tokio::test', 1)[0]
diagnostic = source[source.index('/// Diagnostic only:'):]
target.write_text(helper + '\n' + diagnostic)
modules.write_text(modules.read_text() + '\nmod idle_model_switch;\n')
PY
```

Install the baseline's locked adapter dependencies and run the same ignored
command from each worktree. Use distinct target directories, record the harness
addition separately from the production SHA, and retain raw stderr privately.
On Windows use equivalent PowerShell environment syntax. Normal suites must run
with tracing **unset**, because protocol/stderr assertions are not trace-aware.
When exporting Linux source on Windows, use `git -c core.autocrlf=false archive`
and fresh build outputs so byte-pinned suites and embedded documentation stay LF.

## Validation and outstanding gates

The Linux adapter baseline and candidate both passed **948 tests, 29 skipped**
(977 total); editor-native passed **194/194**. Native editor host builds and
**12 delegation tests** passed. All-feature workspace clippy with `-D warnings`
and formatting passed. Nine focused idle-switch tests passed on Windows and
Linux; the manual timing probe is ignored in ordinary suites.

The final `cargo test --locked -p octet-coding-agent --no-fail-fast` run reported
**2937 passed, 8 failed, 4 ignored** in the library, with exactly the same eight
failure names as the unchanged base. All integration targets passed, including
**26/26 startup/frame PTY tests**. The terminal-grant lifecycle regression and
the diagnostics-preservation PTY case also passed independently. The latter now
loads edited resources via explicit `/reload` before switching models, checking
that the retained snapshot's diagnostics survive rather than requiring an
implicit rescan on every selection.

Linux's `early_version` integration test initially lacked `strace`; extracting
strace 6.19 and its libunwind dependency into a dedicated local tools directory
resolved that prerequisite. Its focused and final suite reruns passed without
changing the test or installing system-wide packages.

The unchanged LF baseline coding-agent library suite reproduced **2928 passed,
8 failed, 3 ignored** after using an existing Python 3.12 installation with
`venv` and `ensurepip`. System Python 3.14 lacked `ensurepip`; the earlier
45-failure run was not a valid comparison. A stale CRLF embedded-document bundle
was also rebuilt, rather than suppressing its assertion.

The eight baseline failures are:

- `herdr::restore::tests::a_matching_record_plans_one_resume`
- `active_run_commands_and_steering_tests::ctrl_s_follow_up_keeps_attachment_recall_and_full_queue_keeps_draft`
- `active_run_commands_and_steering_tests::requeued_edited_live_steering_is_what_the_next_provider_request_carries`
- `idle_shell_and_clipboard_tests::active_clipboard_draft_handoff_drops_pending_read_before_replacement`
- `pi_composer_slot_tests::native_pi_custom_editor_host_keys_precede_terminal_consumers`
- `pi_composer_slot_tests::native_pi_custom_editor_refused_paste_retains_payload_and_undo`
- `pi_native_editor_service_tests::native_pi_editor_service_owns_unicode_cursor_undo_and_submit_callbacks`
- `queued_follow_ups_and_input_close_tests::queued_follow_ups_dispatch_only_after_completion_or_escape_settlement`

Except Herdr, these names are under `modes::interactive::tests`. The Herdr
assertion resolves an existing installed binary symlink to a different path;
the remaining failures concern editor/input behavior outside this performance
change. Reproduction on the base distinguishes them from new regressions, **not**
from release blockers. The full coding-agent command remains red; no failures
were skipped to obtain these results. Do not pin this as a qualified release
candidate until the outstanding gates are resolved. No populated Pi setup, real
Mac Cua Space, issue #587 resolution, or publication has been qualified by this
probe.
