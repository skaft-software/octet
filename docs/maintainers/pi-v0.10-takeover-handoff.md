# octet v0.10.0 Pi compatibility: takeover handoff

Updated 2026-11-03. Read this first, then `docs/maintainers/pi-usability-brief.md` (the rules,
fixtures and screen-stability acceptance) and `docs/maintainers/pi-extensions-eval-handoff.md`
(the history: eval, startup benchmarks, earlier decisions).

## Goal and bar

The maintainer's own Pi 1.4.0 setup must be fully usable in octet for the v0.10.0 launch.
Extensions just work, and there is no visible jank:

- the Pi footer is right from the first frame, with no swap or flicker;
- prompts behave as in Pi;
- extension problems appear as short, tidy notices, never log dumps;
- the user's own octet choices are not overridden.

Parity is for launch only; after launch there is no ongoing upstream-tracking work. Pi 1.5.0
is out of scope.

Rules that never bend:

- **Lean core.** Rust stays Pi-agnostic. Pi-specific behaviour lives in
  `extensions/octet-pi-compat`; the host gets only generic primitives.
- **No weakening.** Do not weaken trust, review, source integrity (including the per-import
  recheck added in `852ca760`), fences, credentials, process cleanup or test deadlines.
- **Proof on the real fixture.** Every fix needs a regression test, plus evidence from the
  real fixture.
- **No slop.**

## Where the code is

- `release/v0.10.0-candidate` is at `852ca760`. The coordinator's branch
  `claude/admiring-feynman-2sqpfb` is `852ca760` plus the following, all pushed:
  - review fixes, reviewed and tested (97/97 focused tests; the full adapter suite shows no
    new failures):
    - `b2dabcfb`: warm-up child path with spaces;
    - `6d5b16b1`: native-ESM shared-file scope;
    - `1817ff8b`: resolve-cache package.json walk;
    - `365bf0b1` and `9455a916`: cache temporaries;
    - `2f630cc7`: realpath alias targets, the Windows separator;
    - `ab41b86d`: refuse foreign-owned or loose cache directories;
    - `4eb053a8`: files using `module.exports` stay on jiti;
  - `afc77ecb`: `scripts/pi-eval/maintainer_fixture.py`, the fixture mirroring the
    maintainer's real setup;
  - docs: the brief, this handoff, and the safety copies below.
- **Not yet merged.** These are worker branches. They exist as local branches in the
  original container, and as patch copies in `docs/maintainers/pi-usability-wip/`
  (`<K>-commits/`: `git am`; `<K>-uncommitted.patch`: `git apply`).

| K | Branch | State | Next action |
|---|---|---|---|
| M | `fix/pi-model-precedence-r2` (2 commits) | **Done.** Precedence is CLI > in-session > saved octet > Pi default > built-in, with one notice; the RPC thinking gap is closed. 11/11 tests; the `--lib` failing-name list matches the base. Fixture check: octet stays on its saved model across /reload. | Review and merge. Open: RPC `set_model` doesn't set `model_explicit`. |
| P | `fix/pi-before-prompt-r2` (2 commits) | **Done for the reproduced cause.** `before_agent_start` rethrew any truthy `error.code` (SQLITE_BUSY, ENOENT), failing the whole prompt: `extension prompt composition failed: ... before_prompt hook failed`. Fixed to match Pi (report, continue). Separately, tool-guard budgets no longer count time spent in permission dialogs (pi-permission-system denied after 10 s). | Review and merge. See the open gaps below. |
| R | `review/pi-compat-r2` | All 8 commits are merged. | Unfixed findings are listed below. |
| D | `fix/pi-diagnostics-ux-r2` (3 commits) | **Partly done.** Grouped, path-free notices of at most 4 lines, named by package, severity split, repeats deduplicated; host one-line "did not start"; wording for queued actions without jargon. Unit-tested. | Build the binary; check the real fixture (a throwing factory, a hung handler, /reload); run the 20 s flicker windows; add a host line-budget test. |
| F | `fix/pi-footer-r2` (4 commits plus about 110 lines uncommitted) | **In progress.** Fixed: the Pi setup was not mirrored when cwd = HOME (the `~` session bug). Added `scripts/pi-eval/flicker.py`, a frame-by-frame checker. Latest commit: a footer placeholder drawn in place until the extension footer paints. | Finish fidelity (the model display name; the context and cost segments), the cached placeholder, and the 20 s windows at cwd=workspace and cwd=HOME. Identify the footer owner: probably `pi-agent-extensions` or Pi's built-in footer. |
| S | `perf/pi-warm-exit-r2` (2 commits plus about 130 lines) | **In progress.** Diagnosed: an idle exit took 2.04 s (Pi 0.10 s) because shutdown queued behind a still-running pi-lens `session_start`, and the host's 2 s timeout killed the process with no shutdown hooks run. Commit `4116c3d8` makes quit stop waiting. Warm: octet 2.38 s vs Pi 3.20 s (target 1.6 s or less). | Verify the exit fix: abort the in-flight hook's signal, detach it, run session_end in order, never concurrently with the same factory's session_start; add tests. Then port only the V8 code cache from `eedffd48` if it measures better. Then the flicker windows, the autoload:false zero-cost check, and the full suite. The salvaged stdin-EOF patch stays only if it measurably helps. |
| J | `eval/pi-jank` (10 commits plus 45 lines) | **In progress.** One-off I/O comparison with Pi 1.4.0 on the PRIMARY fixture: screens frame by frame, provider requests, extension API traffic, timings. | Deliver `docs/benchmarks/pi-jank-<date>/README.md`: a ranked list of every visible difference, each with its root cause and owner. This is the launch checklist. |

## Decisions the maintainer has made

- **Model and thinking:** the user's saved octet defaults outrank Pi's defaults. Pi defaults
  only fill gaps and are never persisted (implemented in M).
- **Other Pi settings while Pi compat is on: "Pi settings win"** for the session; they are
  never persisted. This covers steeringMode, terminal.showTerminalProgress, tuiMode,
  markdown.mermaid and compaction. **Not implemented yet.**
  - It conflicts in spirit with the model rule above. Treat the model/thinking rule as the
    explicit exception, and confirm with the maintainer if unsure.
- **Codemode:** when Pi has `defaultTools: ["+codemode"]` / `codemode.mode: on`, enable
  octet's `octet-codemode` extension for the session. **Not implemented yet.** Without it,
  the provider request differs from Pi's: octet has no `codemode` tool.

## Open gaps (unowned or not fixed), ranked

1. **Pi settings.** Implement "Pi settings win" and the codemode decision (above). Settings
   octet currently ignores:
   - `steeringMode: all`: octet delivers queued messages one at a time;
   - `terminal.showTerminalProgress`: no OSC 9;4 progress bar outside Tern;
   - `defaultTools` / codemode;
   - `tuiMode`, `markdown.mermaid`, `compaction`: ignored, but they currently match by default.
2. **Noisy notice at every start on the fixture:**
   `reload failed: session-only Pi defaults were not applied; ... thinking max is not supported by ...`.
   Shown on every start, it is jargon and truncated. Re-check it with M merged and the
   wording polished per D. Also `mock/mock-1 is unavailable` (the fixture's Pi provider name
   `mock` vs octet's `custom`): a fixture artifact, but the wording problem is real.
3. **before_agent_start limits:**
   - The host's fixed 5 s `PROMPT_RPC_DEADLINE` (`crates/octet-coding-agent/src/extensions.rs:172`)
     fails a prompt whose handler shows a dialog.
   - A legitimately slow handler (over 2 s) is skipped and its system-prompt contribution is
     dropped. Pi waits forever.
   - The adapter's `fields()` rejects extra result keys, where Pi ignores them.
   - Decide a generic host primitive, e.g. a hook asks for an extended deadline while it
     waits on the user.
4. **The host hides the error cause:** it formats `{error}` at `interactive.rs:13572`. Show a
   one-line root cause (owner: D).
5. **Prompt before READY** waits about 11 s for extension session start (the host waits up
   to 12 s). Pi cannot accept a prompt before its extensions load at all. Decide the UX:
   queue visibly, or block the composer.
6. **Cache-location findings (R #4, #5).**
   - #4: a pre-warmed cache in a read-only or packaged install is ignored, so the first
     launch is cold.
   - #5: if no private cache directory is usable, jiti falls back to the shared
     `TMPDIR/jiti`, which is a poisoning risk. Disabling fsCache there is safer.
7. **A spurious `.../agent/npm/skills: missing` diagnostic** from discovery
   (`lib/pi-setup.mjs`). Untraced.
8. **Prelude semantics** (R #3): native `module.exports === exports` is false (jiti: true).
   Low.
9. **The pi-background-tasks `anthropic-attribution` session_start** skip could not be
   reproduced on the fixture. Re-check on a large resumed session.
10. **/reload mid-stream:** Pi refuses it ("Wait for the current response to finish"); octet
    queues it. Keep octet's behaviour, but the notice must be polished (D did the wording).
11. **Adapter test baseline:** 118 to 122 failures exist on the base in this sandbox.
    - 58 are `installed_pi_unavailable` (no managed Pi in the test HOME).
    - 38 need a built native editor binary.
    - About 20 are RPC timeouts that are flaky under load.

    Triage them on a quiet machine with a managed Pi installed, and record them explicitly in
    the release evidence.

## Measured so far (side by side vs Pi 1.4.0, 5 reps)

- 23-package fixture (`docs/benchmarks/pi-extensions-2026-11-02/`):
  - cold full UI: octet 17.20 s vs Pi 25.62 s (0.67x);
  - warm full UI: 2.26 s vs 2.34 s (a tie);
  - octet is typeable at about 0.03 s.
- PRIMARY fixture (S's baseline, under `flock -x`):
  - warm full UI: octet 2.36–2.41 s vs Pi 3.20 s;
  - idle exit: 2.04 s vs 0.10 s (fix in progress);
  - exit after a prompt: 0.06 s vs 0.09 s.
- Re-measure after merging; `852ca760` added per-import rechecks.

## Environment and harness (lessons that cost hours)

- **PRIMARY fixture:** the maintainer's real Pi setup, with 29 packages: 13 autoload and 16
  `autoload: false`. Settings: ghostty-dark (shipped by `pi-agent-extensions`), fullscreen,
  codemode, steeringMode all, max thinking. Build it with
  `python3 scripts/pi-eval/maintainer_fixture.py [cache]`. A secondary 23-package fixture
  comes from `scripts/pi-eval/fixture.py`.
- **Running third-party Pi code:** the Pi CLI and npm Pi packages may only be run in a
  disposable sandbox, never on the maintainer's host.
- **Worktrees need a REAL copy** of `extensions/octet-pi-compat/node_modules`.
  - Missing: octet never loads extensions; it looks like "the footer never appears".
  - Symlinked: Node loads two typebox and shim instances, giving bogus identity failures.
- **Disk:** fixture homes are about 600 MB to 3 GB.
  - Hardlink `install/` and `npm/`; keep one home at a time and delete it after each run.
  - Use `CARGO_INCREMENTAL=0` with debug info off.
  - Delete `target/` when done. A full disk stopped work once.
- **4 cores:** wrap fixture runs and builds in `flock -s scratchpad/measure.lock`, and
  timing runs in `flock -x`. Report the load average with any timing.
- **Pi warm runs** need a shared TMPDIR between a rep's cold and warm legs (Pi's jiti cache
  lives in TMPDIR). The eval handles this.
- **Usage limits** stopped every worker several times. Commit after every verified step,
  and keep patch copies pushed.
- **The release binary** `target/release/octet` matches `852ca760`'s Rust. Rust changes need
  a new release build (about 15 min on 4 cores).

## Suggested order for the next agent

1. Merge M and P after reviewing them; re-run the focused tests and the full adapter suite.
2. Finish S's exit fix, the biggest visible win, then merge.
3. Finish F (footer fidelity plus the no-swap placeholder) and D (real-fixture and flicker
   checks), then merge both.
4. Implement the Pi-settings and codemode decisions.
5. Finish J's ranked jank list. Treat it as the launch checklist: every "unusable" or
   "ugly" item is fixed or explicitly accepted by the maintainer.
6. Re-run the side-by-side benchmark and the 20 s flicker windows on the PRIMARY fixture,
   then record the release evidence.
