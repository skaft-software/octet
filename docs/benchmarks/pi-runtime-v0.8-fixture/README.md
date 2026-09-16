# Pi runtime fixture evidence — 00e3ca3e561fc807491931712b93e534c952cf59

Bounded, offline, credential-free capture from
[`scripts/bench-pi-runtime.py`](../../../scripts/bench-pi-runtime.py). It runs the checked-in
Pi compatibility fixture only: no model or provider request, no network call, no inherited
credentials, and a temporary HOME. These are fixture representations of the lifecycle
profiles, not a production runtime-manager measurement.

## Reproduction

```console
python3 scripts/bench-pi-runtime.py --candidate 00e3ca3e561fc807491931712b93e534c952cf59 --repetitions 5 --sample-interval-ms 25 --max-resource-samples 16 --publish --output docs/benchmarks/pi-runtime-v0.8-fixture
```

The method is deterministic (fixed profiles, fixed fixture identities, bounded samples);
wall-clock timings still vary per run and host, so the recorded numbers are one capture.

## Measured thresholds

| Metric | Observed | Limit | Unit | Status |
| --- | --- | --- | --- | --- |
| `aggregate_first_activation_p95_ms` | 0.564 | 1500.0 | ms | pass |
| `aggregate_peak_rss_delta_kib` | 5776.0 | 262144.0 | KiB | pass |
| `aggregate_restart_readiness_p95_ms` | 53.952 | 1500.0 | ms | pass |
| `aggregate_startup_overhead_median_ms` | 4.245 | 250.0 | ms | pass |
| `aggregate_startup_readiness_p95_ms` | 51.612 | 1000.0 | ms | pass |
| `aggregate_warm_call_p95_ms` | 0.221 | 250.0 | ms | pass |

`status: pass` over 5 repetition(s) per profile
(minimum 5); release approval
`blocked`.

## Release gates

- `runtime_manager_adapter` (unmet): observed 'hermetic_fixture'; requires a checked-in adapter backed by the real aggregate plan/evidence seam.
- `inference_attribution` (unmet): observed 'no inference process was launched or sampled'; requires separately retained inference server identity and resources.

## Profiles

| Profile | Startup median/p95 ms | First activation median/p95 ms | Warm call median/p95 ms | Restart readiness median/p95 ms | Active peak RSS p95 KiB |
| --- | --- | --- | --- | --- | --- |
| `no_extension` | 45.582 / 80.156 | 0.125 / 0.353 | 0.073 / 0.149 | 46.247 / 66.74 | 46560.0 |
| `legacy_eager` | 49.813 / 51.327 | 0.574 / 0.625 | 0.196 / 0.247 | 49.819 / 51.554 | 52144.0 |
| `lazy` | 39.838 / 42.777 | 49.88 / 56.233 | 0.338 / 0.413 | 48.9 / 50.733 | 51552.0 |
| `shared_workspace` | 49.645 / 50.847 | 0.578 / 0.623 | 0.204 / 0.218 | 49.637 / 53.316 | 51696.0 |
| `pi_aggregate` | 49.827 / 51.612 | 0.549 / 0.564 | 0.198 / 0.221 | 49.941 / 53.952 | 52336.0 |

## Method and limits

- Driver: `hermetic_fixture` (reload semantics: `process_restart`).
- API evidence version: `0.3`; bridge SHA-256 `e79a68ba8c4d35f0…`.
- Pi runtime: `checked_in_fake_pi` @earendil-works/pi-coding-agent 0.84.4.
- Platform: Darwin 27.0.0 arm64.
- Samples: interval 25 ms, at most
  16 per process, raw samples bounded.
- Linux records `/proc` RSS/PSS/CPU ticks/threads/FDs. Darwin uses `ps` for RSS, an
  instantaneous CPU percent and, where the local `ps` supports the keyword, threads; PSS and
  FD count stay unavailable rather than estimated.
- Agent process trees are always separate from inference resources; this capture launched no
  inference server, so no GPU or model-server claim is present.
- Sanitization: nonessential CPU brand string omitted; measured samples, thresholds and limits unchanged.
- Nothing here approves a release or claims production lazy activation, cross-workspace
  sharing, reload policy, FD limits or multi-session governance.
