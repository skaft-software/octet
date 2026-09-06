# Benchmarking octet

This directory contains reproducibility evidence. A result is publishable only
when its losses, environment, binary identity, raw outputs, and any adjudication
exclusions are retained.

The current methods below cover [optional telemetry](#optional-agent-telemetry),
[systems measurements](#systems-measurements), and [usability checks](#usability-checks).
The [Pi runtime fixture](#pi-runtime-fixture-evidence) is hold-only, not a published
performance result. For planning, see the [project](https://github.com/orgs/skaft-software/projects/5).

## Historical results

These are historical Ygg artifacts, not octet 0.7.0 results:

- [Terminal-Bench 2.1 evidence package](tb21-v0.6.2/README.md)
- [Frozen v0.6.2 control fingerprint](baseline-v0.6.2.md)
- [Reconciled failure report](failure-report-v0.6.2-2026-08-28.md)
- [Complete token-efficiency audit](token-efficiency-v0.6.2-2026-08-28.md)
- [Scoped runtime-footprint comparison](runtime-footprint-2026-08-29.md)

The pinned [Harbor adapter](../../evaluation/harbor/README.md) reproduces historical
Ygg 0.6.2 only. It is not an octet 0.7.0 evaluation adapter or campaign.

## Optional agent telemetry

Enable telemetry explicitly; normal sessions do not create it:

```console
octet --telemetry ./artifacts/run.jsonl --model <model> "<task>"
```

`--telemetry` also accepts a relative path, resolved from the invocation
directory. `OCTET_TELEMETRY` and `telemetry = "..."` in `~/.octet/config.toml`
are equivalent configuration layers. The file is created with owner-only
permissions and contains bounded JSONL records under `octet.telemetry.v1`.

Telemetry records:

- `run_started` — opaque session/run identity, model/endpoint/protocol,
  context limit, input byte counts and a SHA-256 input identity; no prompt text.
- `model_request_started` and `model_request_finished` — logical turn,
  attempts, wall latency, TTFT, generation time, context occupancy, output
  byte counts, stop reason, and provider usage.
- `provider_retry` — retry number, bounded sanitized diagnostic, and backoff.
- `tool_started` and `tool_finished` — tool name, hashed arguments, elapsed and
  result sizes, repeated-call count, status, known built-in state changes, and
  a conservative no-progress streak. Arguments and results are not retained.
- `tool_policy_decision` — hashed tool-call identity, allowed/denied effect
  admission, stable denial code when denied, and effective capability/limit
  values with their source layers. Raw command arguments, workspace paths, and
  shell paths are not retained; shell resolution is only the non-correlating
  `configured`, `system_bash`, `path_bash`, `sh_fallback`, or `unavailable`
  selection label.
- `compaction_started` and `compaction_finished` — reason and durable outcome.
- `candidate_rejected`, `steering_delivered`, `follow_up_delivered`, and
  `delegation_updated` — control-flow accounting.
- `run_finished` — terminal status and aggregate request/tool counters.

Usage semantics are explicit: `uncached_input_tokens` is the provider's
standard-rate input bucket. `cache_read_tokens` and `cache_write_tokens` are
disjoint additions; `cache_write_1h_tokens` is a subset of cache writes.
`provider_input_tokens` is the three disjoint prompt buckets' sum.
`reasoning_tokens` is a subset of output. `total_tokens` is octet's canonical
normalized sum, not a promise that an overlapping or omitted provider wire
`total_tokens` was preserved. Records with usage include `usage_scope`:
`request`, `operation`, or `run_cumulative`; never sum cumulative snapshots.

Telemetry is an observer, not a wire capture. It does not currently expose
provider request IDs, exact response-header timing for compaction/gate calls,
or raw context bodies. Those limitations must be stated in reports.

Do not ask users to enable telemetry for a report. If they volunteer diagnostics,
`octet --telemetry ./octet-telemetry.jsonl` produces a redacted operational trace;
they should inspect it before sharing. See [voluntary diagnostics](#voluntary-diagnostics)
for the sharing boundary.

## Systems measurements

`scripts/bench-systems.py` uses only the Python standard library and real OS
process measurements. It reports medians and p95s over repeated runs, best
available RSS/PSS, CPU samples, direct-process concurrency totals, and parsed
octet telemetry:

```console
python3 scripts/bench-systems.py \
  --binary ./target/release/octet \
  --repetitions 9 \
  --command sessions='./target/release/octet --offline sessions list' \
  --output ./artifacts/systems/octet.json
```

For a long-lived process, provide an explicit command whose stdin remains open:

```console
python3 scripts/bench-systems.py \
  --idle-command idle='./target/release/octet --plain --model <local-model>' \
  --concurrency 1,2,4 \
  --repetitions 9 \
  --output ./artifacts/systems/octet-idle.json
```

The runner never invokes command strings through a shell. Use `env`, a wrapper
script, or an absolute executable in the argument vector when setup is needed.
JSON output retains every startup run, every per-run idle sample and peak, and
every per-run concurrency sample and peak. `--skip-startup`, `--skip-idle`, and
`--skip-concurrency` can split a long campaign into raw files without reducing
the repetition count for the retained cases. PSS is reported only where the
operating system exposes it; RSS is not a PSS substitute. Direct children are
measured, so an inference server must be reported separately and excluded from
the agent-overhead number.

The command adapter deliberately does not pretend to measure UI rendering,
provider-to-tool scheduling, or resume latency from a generic `--version` case.
Those cases require a harness-specific driver and should be supplied with
`--command` or an additional checked-in adapter. A comparison must use the
same task, endpoint, model weights, context limit, timeout, hardware, and
concurrency for every harness.

## Usability checks

Use these manual checks for installation, session resume, and cancellation:

1. Install the [version-pinned native release](../installation.md) into a fresh
   user directory, or build the checkout in isolation.
2. Configure either a local OpenAI-compatible endpoint or a cloud provider.
3. Run `octet --help`, start one session, and complete a small repository task.
4. Exit, resume with `octet --continue`, and complete a second task.
5. Cancel one intentionally long-running operation and regain the prompt.
6. Inspect `/status` (or the equivalent status command) and report the active
   model, endpoint class, and context information.

### Usability reports

Collect voluntary reports through an issue template, interview, or exported local
form. Record:

- OS, CPU/RAM/GPU, octet version, and install method;
- provider class (`local`, `remote`, or `subscription`), not credentials;
- installation and first-task completion: success/failure, minutes, and whether
  author assistance was needed;
- resume/cancel behavior: pass/fail; and
- crashes, hangs, provider configuration failures, and abandoned tasks.

Report the numerator, denominator, exclusions, and reason categories. Do not hide
an unresolved crash or data-loss issue in an average, or turn a small usability
sample into a superiority claim. Follow the [diagnostic sharing rules](#voluntary-diagnostics).

## Pi runtime fixture evidence

[`scripts/bench-pi-runtime.py`](../../scripts/bench-pi-runtime.py) is the
checked-in, stdlib-only driver for Pi aggregate lifecycle evidence. It runs no
network/provider/model request, inherits no credentials, uses a temporary home,
and writes bounded raw resource samples plus a checksum. It measures fixture
representations of no-extension, legacy-eager, lazy activation, shared-workspace,
and ordered-Pi-aggregate paths; it is intentionally hold-only until a real API
0.3 runtime-manager adapter is available. See [Pi runtime evidence
harness](pi-runtime-evidence.md) for invocation, exact candidate/fixture identity,
Linux/macOS limits, separate inference/GPU attribution, and publication rules.

## Publication boundary

Raw campaign homes and first-pass captures remain owner-only. Before committing
an evidence package, replace user-home, workspace, configuration-root, and
nonessential host-identity strings with stable public placeholders; remove
credentials, sessions, private evaluator material, and provider payloads rather
than redacting them in place. Keep methodology-relevant hardware, versions,
digests, argument flags, environment keys, and numeric samples. Record exactly
what was sanitized, state whether any measurements changed, and recompute public
artifact checksums after sanitation.

### Voluntary diagnostics

A voluntarily shared diagnostic bundle may contain version/build identity,
platform, provider kind, configuration keys with values removed, sanitized
startup diagnostics, and explicitly selected telemetry. Exclude credentials,
authorization headers, raw prompts, tool arguments/results, and workspace paths
where possible. Never request API keys, raw prompts, private repositories,
unredacted session files, or mandatory background telemetry.

Session content must be excluded unless the user deliberately redacts and
approves it for private support. That exception does not relax the public
evidence-package exclusions above. Diagnostic sharing and any export convenience
are optional, not prerequisites for using octet.

## Failure taxonomy

Record each non-success trial in one primary class and optional secondary
causes:

| Class | Evidence to retain |
| --- | --- |
| `benchmark_timeout` | process deadline, last telemetry record, active operation, and OS command state |
| `provider_failure` | sanitized provider phase/status/request ID, retry history, and whether any output was generated |
| `context_failure` | model limit, estimated/provider input, compaction attempts, and durable boundary |
| `tool_failure` | tool name, exit/timeout status, bounded result, and whether the workspace changed |
| `agent_failure` | terminal reason, session checkpoint, and last successful state transition |
| `verifier_negative` | verifier result only; never infer cause from a failed score |
| `integrity_exclusion` | exact reason, reviewer evidence, and original raw result |

Do not collapse verifier negatives into timeouts. Do not call a provider failure a
model failure without evidence. A trajectory audit is separate from official
benchmark adjudication.

## Canonical-run checklist

Before starting a full campaign, record:

1. octet version, commit, binary hash, compiler and OS image.
2. Harbor version/commit, exact dataset revision, task count, attempts, timeout,
   concurrency, and retry policy.
3. Model identifier and weight digest, provider/server version and endpoint
   settings, reasoning effort, context/output limits, and cache policy.
4. Exact system prompt/configuration, enabled tools, disabled extensions,
   workspace image, environment digest, and benchmark command.
5. Raw result files, trajectories, telemetry, stdout/stderr, and verifier output.
6. Reward-hacking audit rubric, reviewers, confirmed exclusions, ambiguous cases,
   and an official-vs-unofficial score distinction.

Run a small generic regression sample before changing any heuristic. A complete
campaign is not evidence that an implementation is good if the protocol or
inputs changed between control and candidate.

## Same-model harness shootout

Use one immutable endpoint and one task manifest. For each harness, collect:
accuracy, success/hour, wall time per success, model requests, octet/tool calls or
the closest equivalent, provider input/output/cache buckets, retries, timeouts,
agent RSS/PSS, and crashes. Publish raw per-trial records and a table that
separates runtime overhead, UI latency, agentic efficiency, and successful-task
throughput. If a harness cannot expose a metric, mark it unavailable rather
than estimating it from incompatible logs.
