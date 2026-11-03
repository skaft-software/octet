# Synthetic warm resolution benchmark

This is **not** the 23-package Linux full-UI benchmark. For that campaign and its
readiness/isolation requirements see [the Pi eval](../../../../scripts/pi-eval/README.md).
The <=1.6 s full-UI target is not established by this fixture.

## Change and cost model

The jiti resolution cache previously repeated the serialized alias table in every
resolution key. The current adapter table has 34 entries and is several KiB. A
warm launch reads/parses that repeated text, builds another copy for every lookup,
and hashes the resulting long strings for Map membership. No module resolution
is saved by repeating those bytes.

`lib/jiti-resolve-cache.mjs` now represents the exact ordered table (including
all target paths, not only names) by its SHA-256 digest, memoized with the existing
WeakMap. Other key inputs, filesystem dependency validation, successful-resolution
requirements and stock-jiti fallback are unchanged. The private cache schema/file
moves to v2; v1 is ignored, so the first launch repopulates the resolution map.
No source checks, routing, factory order, native fallback or lifecycle behavior
change. This uses the same SHA-256 collision-resistance assumption as the other
startup identities, not a permission boundary.

## Reproduce

Supply two adapter directories with their exact locked dependencies already
present. The baseline should include the ordered review-integrity fixes too,
so the experiment isolates the resolution-key change.

```sh
node extensions/octet-pi-compat/test/benchmarks/warm-resolution.mjs \
  /absolute/baseline/octet-pi-compat \
  /absolute/candidate/octet-pi-compat \
  /absolute/new-evidence-directory 9
```

The script refuses an existing output directory. It copies the adapters into
equal-length paths, creates an isolated HOME and separate workspace, and generates
23 synthetic TypeScript factories with 24 tiny relative helpers each. It executes
no downloaded extension factories and does not read the user's Pi setup. Each
trial runs real `auto-configure.mjs` then `runner.mjs --inspect`; the latter loads
the reviewed mirror without a Rust host, lifecycle hooks or terminal. It verifies
catalog reuse, ordered identical metadata, all 23 commands and zero failed factories.
One cold prime and a second fingerprint-settling prime per variant are excluded;
measured order alternates. The 3.1-second wait leaves the existing source-fingerprint
racy window; it does not alter that guard. Per-trial output, traces, cache files,
environment, module hashes and medians remain under the evidence directory.

## Local observation, 2026-11-02

Node 22.20.0, Apple M3 arm64, Darwin 27.0.0. Nine alternating warm trials per
variant, with both variants including the review-integrity repair on `ef973be6`:

| Boundary (median ± IQR, ms) | Baseline | Candidate |
| --- | ---: | ---: |
| auto-configure spawn to exit | 151.88 ± 6.58 | 154.28 ± 3.21 |
| inspect runner spawn to exit | 267.44 ± 14.01 | 238.99 ± 6.60 |
| sum of these two subprocess durations | 423.41 ± 21.91 | 393.83 ± 5.01 |

The summed median decreased 29.59 ms (7.0%); prepare itself did not improve.
There were 581 cached resolutions in both variants. The resolution file shrank
from 3,714,240 to 532,103 bytes; summed key lengths fell from 3,320,320 to 296,215
characters. An initial runner-only CPU profile attributed about 9 ms to
`readPersisted`, 8.6 ms to `hook`, and 3.8 ms to `key`; its single sampled run is
bottleneck evidence, not a timing distribution.

No tests/builds were run by this worker during timing, but the host was **not
idle**: process snapshots showed unrelated System/Library processes at up to
151.7% CPU and browser/octet/SSH activity. Alternation and the retained samples
limit, not eliminate, this interference. This is a small synthetic startup
improvement, not evidence of a material real-package/full-UI win. The next
qualification step remains profiling and alternating the pinned 23-package Linux
fixture in a disposable sandbox with the matching native binary.

Regression coverage is `test/jiti-resolve-cache.test.mjs` (bounded key work,
complete ordered alias identity, cold/warm distinct targets, corrupt-cache
fallback) plus the existing startup-once shadowing-invalidation and review/native/
mirror suites. The bounded-key test fails on the baseline. An initial benchmark
harness attempt incorrectly placed cwd at HOME, making its `.pi` an untrusted
project resource; its zero-factory assertion failed. The recorded timings above
are from the corrected separate-workspace run, not that invalid attempt.
