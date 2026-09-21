# Pi runtime evidence harness (historical)

> **Historical reference:** the Pi bridge, `scripts/bench-pi-runtime.py`, and
> its contract test were removed from the current product. The commands and
> proposed gates below describe the archived fixture capture, are not runnable
> in this checkout, and are not current release requirements. See the
> [local reduction qualification](../qualification/v0.8.0-reduction-rc.md).

This is a reproducible, credential-free **harness**, not a published performance
result. It exercises the checked-in Pi compatibility fixture and writes bounded
`octet.pi.runtime.evidence.v1` JSON plus a threshold-derived
`octet.pi.runtime.decision.v1` decision. It exists to establish the evidence
shape, to catch regressions, and to score a capture against documented limits
while the API 0.3 runtime manager is still being built.

The current capture is
[`pi-runtime-v0.8-fixture/`](pi-runtime-v0.8-fixture/README.md): candidate
`00e3ca3e…`, `status: pass`, **release approval blocked** (the adapter is still a
hermetic fixture). Read the decision and the gates before quoting any number.

## Run

Use an immutable candidate identifier: normally the exact Git commit of a clean
candidate checkout or an immutable build digest. The value is recorded verbatim,
so it must not contain a credential, local path, or other private identifier.

```console
python3 scripts/bench-pi-runtime.py \
  --candidate 0123456789abcdef0123456789abcdef01234567 \
  --repetitions 9 \
  --sample-interval-ms 20 \
  --max-resource-samples 64 \
  --output ./artifacts/pi-runtime/linux-amd64
```

The script uses only the Python standard library and passes Node argument vectors
directly to `subprocess`; it does not execute a shell command. It creates a
fresh temporary HOME/XDG configuration tree, retains only a small allowlist of
locale/time/PATH variables, does not read normal octet/Pi configuration, does not
inherit provider credentials, and never launches or contacts a model/provider.
It requires a local Node executable because the measured compatibility fixture is
Node-based. It makes no package-manager or network request.

`results.json` and `SHA256SUMS` are written to the output directory. The
artifact includes the candidate identifier; script and bridge SHA-256 values;
checked-in fake-Pi package identity/integrity; fixture source and lock
fingerprints; Node/Python/platform/hardware metadata; parameters; raw bounded
resource samples; per-profile median/p95 summaries; the measured thresholds; and
the derived decision.

Run its contract test with:

```console
python3 -m unittest discover -s scripts/tests -p 'test_bench_pi_runtime.py'
```

## Decision, thresholds, and publication

`release_decision.status` is derived from measurements, not hardcoded:

- `fail` — at least one documented threshold is exceeded.
- `incomplete` — a threshold metric is `unavailable` on this platform, or fewer
  than `5` per-profile repetitions were recorded.
- `pass` — every threshold is measured and within its limit, over at least `5`
  repetitions per profile.

`release_decision.release_approval` is deliberately separate. It stays `false`
while any release gate is unmet, so a fixture capture can never approve a
release. The gates record what a candidate run still needs: a checked-in adapter
backed by the real aggregate plan/evidence seam, separately retained inference
server identity/resources, the minimum repetition count, and explicit review of
Linux and macOS candidate runs. A PID snapshot cannot satisfy attribution; this
harness cannot supply the cross-platform release-review receipt.

| Threshold metric | Default limit | Meaning |
| --- | --- | --- |
| `aggregate_startup_overhead_median_ms` | 250 ms | `pi_aggregate` median startup readiness − `no_extension` median startup readiness |
| `aggregate_startup_readiness_p95_ms` | 1000 ms | `pi_aggregate` startup readiness p95 |
| `aggregate_first_activation_p95_ms` | 1500 ms | `pi_aggregate` first activation p95 |
| `aggregate_warm_call_p95_ms` | 250 ms | `pi_aggregate` warm call p95 |
| `aggregate_restart_readiness_p95_ms` | 1500 ms | `pi_aggregate` process-replacement readiness p95 |
| `aggregate_peak_rss_delta_kib` | 262144 KiB | `pi_aggregate` − `no_extension` p95 peak RSS in the active-extension phase |

These are fixture-regression limits for the checked-in driver, not product
targets or a substitute for the [performance contract](../design/performance.md).
Override one limit for a run with a repeatable `--threshold NAME=VALUE` flag;
unknown names, non-numeric values, and negative values fail closed with exit
code 2. Removing a limit is not possible, only tightening or loosening it, and
`--threshold` overrides are recorded inside the artifact's threshold rows.

Publish a committed, self-describing capture with `--publish`:

```console
python3 scripts/bench-pi-runtime.py \
  --candidate "$(git rev-parse HEAD)" \
  --repetitions 5 --sample-interval-ms 25 --max-resource-samples 16 \
  --publish --output docs/benchmarks/pi-runtime-v0.8-fixture
```

`--publish` refuses to overwrite existing evidence, requires the output to be
inside `docs/benchmarks/`, requires a full lowercase 40-character commit or
64-character digest candidate, omits the nonessential CPU brand string, and
writes `results.json`, a generated `README.md` with the method, thresholds and
gates, and `SHA256SUMS`. Follow the
[publication boundary](README.md#publication-boundary) before publishing any
capture by hand; timings are one wall-clock capture and vary per run and host.

## Profiles and attribution

Every invocation performs the same number of repetitions for these profiles:

| Profile | Current fixture meaning |
| --- | --- |
| `no_extension` | Minimal checked-in idle JSON-RPC process; baseline for bridge-overhead attribution. |
| `legacy_eager` | One Pi compatibility bridge initialized during startup. |
| `lazy` | Baseline starts first; the bridge starts only for first activation. |
| `shared_workspace` | One bridge is reused across two synthetic session journeys. |
| `pi_aggregate` | Two ordered Pi sources load through one bridge and one fake Pi `ExtensionRunner`. |

Each run retains startup readiness, first activation, warm call, and process
replacement readiness timings. `shared_workspace` additionally retains reuse
time. `process_restart_readiness_ms` is deliberately a process replacement;
it is not a claim of manager-owned hot reload. The `release_decision` computes a
startup-median delta only from `no_extension` and `pi_aggregate`, so it does not
mislabel the other fixture profiles as a production baseline.

The profile names model the intended lifecycle shapes, but this driver is not an
API 0.3 runtime-manager adapter. In particular it does not establish production
lazy activation, cross-workspace sharing, reload policy, FD limits, or
multi-session governance.

## Resources and platform limits

Resource samples cover the root process and its descendants, and each process
phase retains no more than `--max-resource-samples` samples (1–256). Linux reads
`/proc` for RSS, PSS where `smaps_rollup` is available, cumulative CPU ticks,
threads, and file descriptors. macOS uses `ps` for RSS and an instantaneous CPU
percent. The Darwin field list is probed once: macOS 27 (Darwin 27) rejects the
`thcount` keyword, so thread count and the other unavailable columns are recorded
as `null` instead of being estimated, and RSS/process counts stay measured.
Other platforms return unavailable resource fields.

Agent-process measurements are always separate from inference-server resources.
The default result says no inference server was launched or contacted. If an
already-running, independently managed inference process must be documented,
pass `--inference-pid PID`; the harness takes one separate process-tree snapshot
without connecting to, configuring, or stopping that process. That snapshot is
not release evidence and leaves the `inference_attribution` gate unmet. Portable
GPU collection is intentionally unavailable; attach a platform-specific collector
and document its method, version, cadence, and attribution before making a GPU
claim.

## What is still required for release evidence

A candidate-release campaign needs a checked-in runtime-manager adapter that
uses the real aggregate plan/evidence seam, a clean immutable candidate build,
and separately retained Linux and macOS runs. It must state exact model/server
identity and digest when inference is included, retain agent and inference
resources separately, use a defined cold/warm/cache policy, and document all
failures. This fixture has no model, server, credentials, provider requests, or
GPU result, so those fields are explicitly unavailable rather than inferred.

Before publishing any capture, follow the [publication boundary](README.md#publication-boundary):
review the candidate field and hardware metadata, replace unneeded host/path
identity with stable placeholders, remove secrets rather than redacting them,
record the sanitation, and recompute `SHA256SUMS`. Retain raw samples and failed
trials; do not publish only summaries.
