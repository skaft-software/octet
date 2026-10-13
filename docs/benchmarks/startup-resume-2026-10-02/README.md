# Offline startup and complete native-history resume

Local comparison on 2026-10-02 of the release-integration baseline
`a23543de6a1e99681623254072cbb1a3e975a498` and this PR's implementation.
The scope is interactive readiness with manual model inventories and complete
synthetic history replay, not inference or terminal-emulator painting.

## Results

Median spawn-to-complete-ready-frame times, in milliseconds:

| Models | User/assistant pairs | Before | After | Reduction |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 0 | 18.132 | 18.193 | −0.3% |
| 100 | 0 | 18.385 | 18.601 | −1.2% |
| 1,000 | 0 | 21.924 | 19.044 | 13.1% |
| 1 | 1,000 | 264.561 | 240.401 | 9.1% |
| 1 | 5,000 | 1,234.154 | 1,118.082 | 9.4% |

Small inventories show no improvement in this campaign. At 1,000 models,
median base-catalog construction (`catalog.base` → `catalog.codex`) fell from
7.571 to 4.291 ms. At 5,000 pairs, median hydration (`history.hydrate` →
`frame.ready`) fell from 102.944 to 50.882 ms, a 50.6% reduction. These phase
intervals come from in-process diagnostics; they are **not** the complete ready
clock. Rendering, output, and PTY draining still dominate large-history readiness.
The 5,000-pair ready capture contains 13,053,557 bytes for both executables.

## Method and correctness

- Apple M3, 16 GiB RAM; macOS 27.0 arm64; Python and compiler versions retained
  in the artifacts below. Both executables use Cargo's native `release` profile
  and default crate features. Existing dependency caches were reused; the baseline
  executable was saved before implementation and changed crates were rebuilt.
- One discarded warmup per executable per cell, then nine measured trials per
  executable per cell in alternating order: 90 measured samples, ten warmups.
- Separate disposable homes and workspaces per executable; synthetic parent-chain
  sessions; no inherited credentials, configuration, extensions, or live providers.
  The model inventory is manual, with discovery disabled. No inference is submitted.
- 120×40 Unix PTYs, dark truecolor appearance, Auto mouse policy, native primary
  screen. Readiness ends when the synchronized ready-frame fence reaches the PTY
  observer **after all history**, not when a composer or `frame.ready` trace appears.
- Every historical user/assistant marker must appear exactly once in order.
  Each trial verifies raw-mode application-handled editing, successful shutdown,
  and exact terminal-mode restoration. All retained samples passed.
- No failed samples or latency-based exclusions occurred in this campaign.
  Development/smoke runs and an earlier candidate campaign are not pooled with
  this final rebuilt-candidate campaign. Only the ten stated warmups are excluded.

These are warm-file-cache, offline, extension-free, local PTY results. They do
not measure real-emulator paint, live provider admission, cold-cache startup,
peak memory, or arbitrary histories. Saved colors, Markdown semantics, Auto
background replies and startup typing, and session validation have separate
regression tests. No validation or recovery path was bypassed for these timings.

## Reproduce

Build each source snapshot with the same compiler and flags, preferably using
separate target directories to avoid cross-checkout artifact reuse:

```sh
cargo build --release --locked --offline -p octet-coding-agent --bin octet -j 2
python3 scripts/bench-startup-resume.py /path/to/before/octet /path/to/after/octet \
  --trials 9 --models 1 100 1000 --turns 0 1000 5000 > /tmp/startup-resume.jsonl
python3 -m unittest discover -s scripts/tests -p 'test_bench_*.py'
```

Run the harness from the candidate checkout. Fixture generation and correctness
inspection are outside the measured ready interval; the observer scans new bytes
with bounded overlap so it does not add history-sized rescans per read.

## Retained evidence

- [`results.jsonl`](results.jsonl): exact script output, all per-trial ready/edit
  timings, startup phase timestamps, correctness results, binary SHA-256s, and
  runtime platform/dimension fields. The final record contains all five cells.
- [`source-manifest.json`](source-manifest.json): base commit, candidate source
  file SHA-256s, binary identities, compiler/Cargo versions, hardware and build
  command. The candidate hashes identify code before commit without embedding
  a self-referential commit ID.
- [`before-build.log`](before-build.log),
  [`candidate-library-build.log`](candidate-library-build.log), and
  [`after-build.log`](after-build.log): baseline, changed-library, and final
  candidate compilation logs.
- [`SHA256SUMS`](SHA256SUMS): checksums of this evidence package.

Publication sanitation replaces only the worktree prefix in compilation logs
with `$WORKTREE`. The structured results contain no home/workspace paths or
session contents and are unchanged. No numeric measurements were altered.
