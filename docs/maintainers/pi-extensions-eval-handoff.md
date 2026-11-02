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
  needs either a permission rule allowing it in this environment, or running the eval
  on a maintainer machine. Everything not executing downloaded code can proceed here.
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

## Next steps

1. Resolve the execution blocker (permission rule, or run legs locally).
2. Relaunch the three workstreams above.
3. Record the baseline (Pi 1.4.0 vs octet, cold and warm, >=5 alternating reps) in `docs/benchmarks/`.
4. Hillclimb on octet warm and cold full_ui_s relative to Pi; target below 1.0x on both.
