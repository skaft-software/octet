# Historical evidence download

[Download compact evidence ZIP](historical-evidence.zip) ·
[ZIP SHA-256](historical-evidence.zip.sha256) · [manifest](manifest.json)

This is supporting evidence, not a new octet 0.7.0 benchmark or an official
leaderboard placement. The ZIP preserves the existing reports, provenance,
exclusions, limitations, sanitized metadata and runtime samples byte-for-byte.
**It is not the full task-trajectory/judge-response archive.**

## What the records support

| Separate measurement | Bounded result |
| --- | --- |
| Terminal-Bench 2.1, Ygg 0.6.2 vs published Codex 0.144.0; GPT-5.6 Sol/max; 89 tasks × 5 trials | Raw verifier 391/445 (87.87%) vs inferred pre-disqualification 371/445 (83.37%). Different dates and possible provider snapshots; not a matched controlled trial. |
| Token accounting for those task aggregates | Including Ygg's complete native usage, 15.1% fewer processed tokens **per raw success**, including failed spend. This is neither fresh-input-only nor adjudicated efficiency. |
| **Separate** macOS arm64 headless-runtime campaign; 9 runs; five-second settle + two-second sample | Ygg 0.6.3 RPC median peak direct-process RSS 7.52 MiB vs Codex CLI 0.149.0 app-server 39.22 MiB. No prompt/inference; not RAM during Terminal-Bench, not identical feature sets, no descendant accounting or PSS. |

The Ygg task audit was a GLM-5.3 Flash surrogate plus manual review, **not
Terminal-Bench maintainer adjudication**. Its primary 387/445 and strict 385/445
local-audit scores must not be equated with Codex's official 339/445 result.
Ygg 0.6.2 also had a documented timeout/cancellation accounting defect; the
campaign was not rerun after its fix. The complete-native token comparison
includes the retained post-timeout usage rather than concealing it.

Read the full [TB2.1 report](../../benchmarks/tb21-v0.6.2/README.md),
[token audit](../../benchmarks/token-efficiency-v0.6.2-2026-08-28.md), and
[runtime methodology](../../benchmarks/runtime-footprint-2026-08-29.md).
The [published Codex aggregate snapshot](codex-tb21-submission.json) matches the
original report's pinned source SHA-256; its upstream URL is in the manifest.
The historical reports' Ygg and third-party identities remain unchanged.

## Gaps that travel with the download

- No retained Claude Code comparison supports a score, token or RAM claim here.
- No current octet measurement, general speed superiority, full parity, or
  official ranking follows from these records.
- The roughly 292 MB Ygg campaign and 59 MB judge trees are not redistributed;
  public release requires privacy and benchmark-redistribution review. The
  included judge checksum index is not a substitute for those raw responses.
- Codex per-trial trajectories, exact provider snapshots, complete build
  attestation and a common official adjudication are unavailable here.
- The ZIP includes the **complete existing compact evidence sets**, not all
  underlying private/raw evidence. It cannot support fresh trajectory-by-
  trajectory adjudication. Source links to files outside the compact set require
  the repository; no campaign is executed by the exporter.

## Reproduce this package

From the repository root, Python 3.10+ standard library only:

```sh
python3 docs/assets/evidence/export.py --check
python3 docs/assets/evidence/export.py --write
python3 docs/benchmarks/tb21-v0.6.2/verify.py
```

The exporter reads only manifest-listed files, verifies their pinned hashes,
and uses sorted paths, fixed timestamps/permissions and uncompressed ZIP entries.
It never changes historical source files, contacts a provider or runs a benchmark.
Extracted files retain repository-relative paths, so the compact TB verifier
also runs within the extracted tree. `SHA256SUMS` covers all package members
except itself. Export reproduction uses the original repository checkout.
