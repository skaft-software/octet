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

## Status log

- [in progress] Building the fixture: Pi 1.0.2 is installed in the scratchpad; picking packages.
- [in progress] Release build of `octet-coding-agent` (4 cores; slow).

## Next steps

1. Codify the fixture builder plus the eval runner under `scripts/pi-eval/`, reusing
   `scripts/e2e/octet_e2e` (PTY, VT model, mock provider).
2. Record the baseline (Pi vs octet, cold and warm) in `docs/benchmarks/`.
3. Hillclimb: single factory import (no double import from `--inspect` capture), hash
   once, a persistent jiti resolution cache, non-blocking warm-up, and overlapped warm prepare.
