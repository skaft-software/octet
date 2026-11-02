# Pi extensions eval: Pi 1.4.0 vs octet, 2026-11-02

Recorded with `scripts/pi-eval/run.py` (fairness fixes included) on a 4-core Linux box.
The fixture is Pi 1.4.0 with 23 pinned npm packages (fixture hash `384e0fadbac1c289`), 5 reps.
Both variants used the same octet binary, built from `release/v0.10.0-candidate` at `7a917bd4`.
- `baseline`: the octet-pi-compat adapter at `7a917bd4`, before the startup perf work.
- `v1`: the adapter with items #2-#4 (hash once, persistent resolution cache, niced warm-up).
  Its octet/Pi ratios use the baseline's Pi medians, because the Pi harness is unchanged.

Full UI, median ± IQR (s):

| variant | target | cold | warm |
|---|---|---|---|
| baseline | Pi 1.4.0 | 25.67 ± 0.73 | 2.54 ± 0.39 |
| baseline | octet | 28.18 ± 1.00 | 3.76 ± 0.20 |
| v1 | octet | 29.36 ± 1.50 | 2.22 ± 0.15 |
| v2 | octet | **22.06 ± 0.25** | **2.26 ± 0.05** |

octet/Pi: cold 1.10x → 1.14x (v1, within noise) → **0.86x** (v2); warm 1.48x → 0.88x (v1) → **0.89x** (v2).
- `v2`: v1 plus the cold-prepare fix (`ef09590b`). The warm-up now covers ESM JS bundles at full width and overlaps the routing scan. Cold prepare went from 25.3 s to 19.2 s. octet is typeable at
about 0.03 s in every leg. Package and command parity are 1.0, and every launch exits cleanly.
Cold octet is about 90% `extensions.pi_mirror.prepare` (25-27 s), which is the next target.
Traces are not committed; regenerate them with the eval.

Open item: octet's exit takes about 2.06 s against Pi's 0.18 s on the warm leg (11x). This is
not startup, but the eval tracks it.
