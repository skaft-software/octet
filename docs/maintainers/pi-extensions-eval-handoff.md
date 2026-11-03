# Pi extensions eval + startup hillclimb: running handoff

Branch: `claude/admiring-feynman-2sqpfb`, based on `release/v0.10.0-candidate` (`c610a671`).
Read `docs/maintainers/pi-v0.10-perf-handoff.md` first; that has the startup analysis this
work starts from. This file is updated at every milestone so work can resume at any point.

## Goal (from the maintainer)

1. Build a holistic Pi-extensions eval that measures whether octet is Pi compatible
   *in production*: real npm Pi packages, the shipped `octet-pi-compat` bundle in
   auto-mirror mode, the real octet binary on a PTY, and real Pi side by side.
2. Startup is the first metric to hillclimb: cold and warm, typeable and full UI.
3. Once the eval exists, hillclimb startup (handoff items 1-5) without weakening
   trust, review or integrity fences.

## Design

- Fixture: a pinned, production-shaped Pi home. Managed Pi (default 1.0.2) at
  `<agent>/install/releases/<v>`, packages in `<agent>/npm/node_modules`, and
  `settings.json` `packages: ["npm:..."]`, all from a lockfile in the eval directory.
  A sentinel extension sets a footer status once `session_start` arrives, which gives
  a "full UI" readiness signal comparable between Pi and octet.
- Legs: Pi, then octet, alternating on fresh HOMEs (cold), then a second launch (warm).
- Metrics: startup (typeable_s, full_ui_s); per-package load status; registration
  parity against Pi; startup errors; exit code and terminal restore.
- Output: JSON report plus a scoreboard; `--baseline report.json` prints deltas
  for the hillclimb.

## Scope

- Parity reference: **Pi 1.4.0** (`@earendil-works/pi-coding-agent@1.4.0`). Pi 1.5.0 is
  explicitly out of scope (maintainer decision, 2026-11-02). Keep the adapter's
  `PI_SUPPORTED_RANGE` (`>=1.0.2 <1.5.0`) unchanged.
- Process: work runs in parallel background workers, and eval-building never blocks the
  perf work. The claude-api build-eval contract is reused for the output layout only
  (`results.jsonl`, `traces/`, `errors.jsonl`, `_state.json` under
  `.claude/hillclimb/pi-extensions/<variant>/`).

## Status log

- Release build of `octet-coding-agent` in progress (4 cores, thin LTO; slow).
- Fixture: Pi package layout confirmed. Pi installs packages with npm into
  `<agentDir>/npm` (see `dist/core/package-manager.js`, `installNpm`/`installNpmBatch`), and a
  managed install lives at `<agent>/install/releases/<v>` plus `install/current-version`.
  A plain combined `npm install` of the candidate packages fails on peer conflicts, so
  use Pi's own installer semantics.
- **Blocker:** the sandbox's auto-mode classifier refuses to execute downloaded
  third-party code (the Pi CLI and npm Pi packages). The eval, by design, runs both. It
  needs a disposable sandbox where those packages may be executed. Do not run the
  downloaded third-party code on the maintainer's host. Other work can proceed here.
- Workstreams briefed, then stopped by an interrupt before writing anything (relaunch
  on request):
  1. Eval harness at `scripts/pi-eval/`: pinned `packages.lock.json` (Pi 1.4.0 plus
     about 25 real packages), `fixture.py`, `run.py` (alternating Pi/octet legs, cold and
     warm, on PTY via `scripts/e2e/octet_e2e`, stderr to a log for `OCTET_STARTUP_TRACE`), a
     sentinel extension setting status `PI-EVAL-READY` on `session_start` as the full-UI
     signal, parity metrics (package load status, command Jaccard), and `selftest.py`.
  2. Adapter perf (`extensions/octet-pi-compat` only): #2 hash once plus a stat-fingerprint
     cache for the single pre-import check; #3 persistent jiti resolution cache; #4 bounded
     or low-priority transform warm-up.
  3. Host plus adapter (worktree, branch `perf/pi-single-import`): design doc
     `docs/design/pi-single-import.md`, then #5 (overlap warm prepare) and #1 (single import
     via contributions-at-handshake), with no fence weakening.

## Single-import workstream result (2026-11-02)

- Design: `docs/design/pi-single-import.md` (on this branch).
- **#1 is not contained.** The host needs a manifest's contributions before spawn (tool
  clashes, catalog, flags into `initialize`). Adopting the capture process as the live
  runner gives factories a different context than the live load, so that option is
  rejected. The viable path is a generic, manifest-opted "contributions at handshake"
  primitive, with the envelope check and clash handling re-run before activation and the
  capture flow kept as fallback. This is a staged protocol change; not implemented yet.
- **#5 is smaller than the handoff assumed.** On an 8-factory fixture, warm prepare takes
  about 0.3 s and runner boot about 0.27 s, so most of the ~2 s warm prepare is repeated
  source walks (#2), not boot. Overlapping boot needs a pre-spawn/held-initialize host
  primitive; this is phase 2.
- **Pending merge:** commit `3da9f2e3` in worktree `.claude/worktrees/agent-a172e93c67afbf9d8`
  (branch `perf/pi-single-import-v2`). On a catalog-cache hit, `configure()` reuses the
  preparation digests. It overlaps #2 in `configure.mjs`; merge it after the adapter
  workstream lands. Its tests: cold-start 9/9, mirror/source-review 41/41.

## Adapter workstream result (2026-11-02, items #2-#4, landed on this branch)

- #2: `lib/source-fingerprints.mjs` is a persisted (dev, ino, size, mtimeNs, ctimeNs) → digest
  cache. The O_NOFOLLOW open, regular-file, size and read-race checks still run on every
  file; only read+hash is skipped on a fingerprint match. Files touched within 3 s are
  never recorded. `configure()` carries the preparation digests forward. The runner's
  single pre-import check is the integrity gate.
- #3: `lib/jiti-resolve-cache.mjs` is a persisted resolution map hooked into jiti 2.7.0
  `jitiResolve` by a single exact-anchor rewrite of the locked dist. It falls back to
  stock jiti on a version or anchor mismatch, or with `OCTET_PI_JITI_RESOLVE_CACHE=0`.
  Entries are validated by stat fingerprints of the resolved file, its parent dirs,
  node_modules and package.json; failures are never cached. Synthetic bench
  (8 extensions, 1208 TS files, warm transforms): import 1.35-1.5 s → 0.25-0.30 s.
- #4: transform warm-up runs in a nice-15 child, with workers = min(6, max(1, cores-2)).
- Tests: adapter suite 1039 pass / 118 fail / 30 skip after vs 1035 / 118 / 30 before. The
  failing test names are identical (pre-existing; the sampled cause is no managed Pi in an
  isolated HOME). New `test/startup-once.test.mjs`. The release owner re-ran startup-once,
  pi-mirror-cold-start and source-review: 16/16.
- Release-owner review decisions:
  - Accepted dropping configure's post-capture re-hash when digests are carried. The
    runner's pre-import check against the prepared digests remains the gate, and capture
    registrations are consent metadata.
  - Accepted cache trust at the same level as jiti's transform cache, which already holds
    executable code in the same owner-only directory, so poisoning these adds no new power.
  - Superseded `3da9f2e3` (worktree branch `perf/pi-single-import-v2`). The adapter version
    covers both cache hit and miss. Not merged.
- Open: the 118 pre-existing adapter failures need triage before release (not caused by this work).

## Eval landed (`b7aa1235`) and pilot (1 rep; not yet a claim)

| leg | Pi 1.4.0 full UI | octet full UI | octet/Pi |
|---|---|---|---|
| cold | 26.6 s | 29.3 s | 1.10x |
| warm | 25.2 s | 4.5 s | 0.18x |

octet is typeable at ~0.03 s. Parity: 23/23 packages and 98/98 commands. Cold octet is
dominated by `extensions.pi_mirror.prepare` (26.3 s); the runner loads in ~3.2 s.
Caveats:
- Pi's warm leg barely beats its cold leg (Pi 1.0.2 was 5.1 s warm), which may be a
  harness artifact. A fairness check is in progress.
- The e2e suite's legacy custom credential no longer yields a model on this binary
  (`scripts/e2e` startup check fails with "Unknown model"). The eval writes an explicit
  `models` list.
- Pi 1.4.0 refuses duplicate tool names, so pi-pretty, pi-memory and pi-web-search were
  dropped from the fixture; octet loads them anyway (behavioural difference).
- octet skips pi-background-tasks' `anthropic-attribution.js` `session_start` callback,
  with an issue warning. `packages_parity` does not count skipped handlers yet.

In flight:
- Measurement worker (main tree): Pi-warm fairness, then a 5-rep `baseline` (adapter at
  `7a917bd4`, via a worktree) vs `v1` (current), written to `.claude/hillclimb/pi-extensions/`.
- Cold-prepare worker (worktree, branch `perf/pi-cold-prepare`): holds timing runs until
  `<scratchpad>/MEASURE_DONE` exists. **The coordinator must create that file when
  measurement finishes.**

## Measured baseline (5 reps, fair harness): supersedes the pilot

The pilot's Pi warm 25.2 s was a harness artifact (per-leg TMPDIR wiped Pi's jiti and
compile caches). See `docs/benchmarks/pi-extensions-2026-11-02/`.

| | Pi 1.4.0 | octet baseline | octet v1 |
|---|---|---|---|
| cold full UI | 25.67 s | 28.18 s (1.10x) | 29.36 s (1.14x) |
| warm full UI | 2.54 s | 3.76 s (1.48x) | 2.22 s (**0.88x**) |

Warm is now below Pi. Cold is still above, and 25-27 s of it is mirror prepare. Finding:
the host's `env_clear()` means `OCTET_PI_COMPAT_CACHE` never reaches the adapter, so its
caches live in `<extension dir>/.cache` inside the installed bundle. Check that this is
writable in packaged installs; otherwise the fallback is `~/.cache/octet-pi-compat`.
`MEASURE_DONE` has been created; the cold-prepare worker may time now.

## v2 measured (5 reps): octet faster than Pi 1.4.0 cold and warm

Full UI: cold 22.06 ± 0.25 s (0.86x Pi), warm 2.26 ± 0.05 s (0.89x Pi). Parity is 1.0
and every launch exits cleanly. The ratios use the baseline run's Pi medians; re-run
side by side before release.
In flight: `perf/pi-native-esm` (worktree) skips Babel for already-ESM JS bundles (about
12 s of the 19 s cold prepare is pi-lens's 6 MB bundle) without breaking shim module
identity or the pre-import integrity gate.
Also open: exit takes about 2 s against Pi's 0.18 s.

## v3 landed (`39d8c0f8`): native ESM for reviewed plain-ESM factories

Design: `docs/design/pi-native-esm.md`. jiti already tries a native import first; it failed
only because bundles import Pi package names, and jiti then ran Babel over the whole file.
`lib/native-esm.mjs` adds a `module.registerHooks` resolve and load hook scoped to reviewed
factory closures:
- aliases resolve to the same shim file URLs, so module identity is preserved;
- each served closure file is checked against its reviewed sha256, and a mismatch is refused;
- a jiti-wrapper-name prelude is joined onto line 1, so line numbers are kept;
- anything unusual falls back to jiti before evaluation;
- disable with `OCTET_PI_NATIVE_ESM=0`.

Worker numbers (3 reps, baseline Pi medians): cold full UI 17.43 s (0.68x), warm 2.31 s.
Adapter suite 1046 pass / 118 fail; the failing names are identical to the baseline.

Residuals:
- pi-subagents (imports CJS `yaml`) stays on jiti.
- Inert `module`/`exports` prelude objects could flip UMD-style checks.
- A native evaluation error re-runs the entry on jiti, as jiti's own native-first behaviour does.
- Release evidence: a side-by-side 5-rep v3 run (Pi alternated) is in progress.

## v3 side by side (release evidence, `docs/benchmarks/pi-extensions-2026-11-02/v3`)

Cold full UI: 25.62 s (Pi) vs **17.20 s (octet), 0.67x**. Warm: 2.34 s vs 2.26 s, 0.97x, a
tie within noise. Parity is 1.0 and all 20 launches were clean.

Workstreams recorded at the time (their unpushed cloud work was unavailable on continuation):
- `perf/pi-warm`: warm target <=1.6 s. Profiles the runner's 1.76 s and the 0.40 s warm
  prepare. Candidates: V8 code cache for jiti-evaluated code, lazy imports, and prefetching
  the next factory while preserving order. It creates `<scratchpad>/WARM_DONE` when timing is done.
- `perf/pi-exit`: exit 2.06 s vs Pi 0.16 s. It waits on `WARM_DONE` before release builds.

## Continuation: review integrity and synthetic warm work

The v3 runner had batched reviewed-source checks before any factory registered. An
only-then-running earlier factory could modify a later reviewed entrypoint or relative
import. A failed native probe could likewise modify reviewed sources before jiti fallback.
The runner now rechecks at each ordered import boundary and after a failed probe, including
cold combined capture. Mutation regressions cover shim and installed routes and continued
loading of healthy factories. These checks do not create an OS privilege boundary.

The jiti resolution cache now hashes the complete ordered alias table instead of repeating
it in every key; its private cache format advances to v2 and repopulates on first use. On
an Apple M3 with Node 22.20.0, nine alternating synthetic trials of 23 generated TypeScript
factories measured median prepare plus inspect subprocess time of 423.41 ms before and
393.83 ms after. Both variants included the review repair. This is not a full-UI or Linux
23-package result. Method, samples' limitations and reproduction instructions are in
`extensions/octet-pi-compat/test/benchmarks/README.md`.

An independent focused adapter suite passed 50 tests; the built native-editor host suite
passed 194. Running the 132 ordinary adapter test files in bounded groups produced 1,304
passes, 60 failures and 30 skips; the same 60 failure names reproduced on the clean base
under the same isolated HOME, mostly without an installed Pi release. The Pi eval self-test
passed 17 tests. A worktree debug octet binary was built; optimized builds timed out.

Synthetic PTY tests with a single generated factory observed normal shutdown delivery and
no surviving test child. A deliberately hung startup prevented the shutdown callback from
being observed on both base and candidate; this does not explain the historical Linux exit
or qualify cleanup of arbitrary process trees. The <=1.6 s warm goal and an improvement to
the real-workload exit remain unverified.

## Next steps

1. Reproduce the pinned 23-package Pi 1.4.0 benchmark in a disposable Linux sandbox with
   the matching optimized binary; alternate baseline and candidate runs.
2. Profile warm full-UI startup and trace the slow exit through session-end hooks, request
   drain, shutdown acknowledgement and process-tree cleanup. Do not shorten deadlines,
   skip hooks or weaken cleanup merely to improve the measurement.
3. Run the remaining installed-Pi and applicable Rust qualification in the appropriate
   environment, and retain baseline failures and any skips explicitly in release evidence.
