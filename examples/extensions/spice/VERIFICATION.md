# SPICE source verification

## Parent follow-up

The parent independently reran all 14 Python source/utility tests successfully;
F01/F02 still explicitly exit 2 as BLOCKED. Offline Cargo metadata resolution for
`aarch64-apple-darwin` succeeded without downloads and generated the standalone
lockfile. The runner now uses `--offline --locked` and includes that lockfile in
source evidence. Metadata resolution is **not** a Rust typecheck, build, simulator
execution or F pass. Parent artifacts are under the run's `evidence/` directory:
`spice-parent-source.*`, `spice-parent-prerequisite.*`, `spice-parent-metadata.*`.

The first real Rust compile found a private `model_tool_definitions` call
(`spice-parent-compile-r1.*`, exit 101). The harness now tests initial lazy
visibility through an actual Agent provider request instead of accessing that
private helper or substituting the unfiltered registered catalog. The second
compile passed with unchanged before/after source identities
(`spice-parent-compile-r2.*`, exit 0):

```sh
CARGO_TARGET_DIR=/Users/achumukundan/octet-rc/execute-20261003-180557/target-core \
CARGO_BUILD_JOBS=1 cargo test --offline --locked --jobs 1 --profile ci-test \
  --manifest-path examples/extensions/spice/host-smoke/Cargo.toml --no-run
```

This built the two-test harness; it did not execute either test, ngspice, or a
provider request. All runtime F01/F02 oracles remain unverified and blocked.

## Initial worker source verification — 2026-10-04 06:18Z

**F01 spice_acceptance: BLOCKED. F02 spice_interrupt: BLOCKED.** Real ngspice is
absent. Simulator executions: **0**. Production-host F executions: **0**. Rust
harness builds/type-checks: **0**; the editor owns the heavy-build slot. No
installation/download, Cargo invocation, provider call, commit or global change
was performed. This continuation changed only `examples/extensions/spice/`.

## Changes since the first handoff

- Uses the finalized real SDK `Resource[T]`, `BlobRef`, `publish_bytes` and bounded
  `read` helpers, including supported 64-character digest schema bounds and
  128-character IDs. No RPC/schema compatibility adapter.
- Fixed explicit model projections: ResourceRef/BlobRef descriptors now appear
  in bounded summary text as well as typed structured output. Structured details
  alone do not supply a model-visible handle. Numerical sample bytes stay local.
- Added production `ExtensionProcess` harness source with stable F01/F02 names,
  host discovery, actual Agent request capture through a loopback scripted endpoint,
  scalar physics oracle, progress, host-owned release and zero-dispatch reuse.
- Added opt-in real-child SIGSTOP/event/marker cancellation barrier. F02 asserts
  pending native execution remains pinned after caller cancellation, then resumes,
  terminates/waits, checks PID absence and absence of waveform publication. Early
  real solver completion before the barrier is BLOCKED, never a successful test.
  No claim of interrupting a particular numerical integration step is made.
- Added real utility-process tests. These ordinary Python children are explicitly
  not solver substitutes, never run through the ngspice backend and emit no waveform.

## Observed commands/results

Cwd: `/Users/achumukundan/octet-rc/extensions-finish-20261004.jhktsi/source`.
Interpreter: **Python 3.14.0**. Baseline remains the shared candidate checkout.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s examples/extensions/spice -p 'test_*.py' -v
```

**Exit 0: 14 tests in 1.249 s; 0 failures/errors/skips.** Five parser/domain tests,
five generated-contract/model-summary checks, four deterministic process-utility
and missing-prerequisite checks. The latter verifies missing ngspice refuses
`--run-host` before Cargo, target or evidence creation, terminates/reaps a real
child, escalates for a SIGTERM-ignoring child, and holds a real stopped child until
an explicit cancellation/allow-stop rendezvous. None is an F conformance pass.

```sh
rustfmt --edition 2021 --check examples/extensions/spice/host-smoke/tests/spice.rs
PYTHONDONTWRITEBYTECODE=1 python3 examples/extensions/spice/run_conformance.py
```

Rust formatting: **exit 0** (syntax/format only, not a Rust type-check/build).
Conformance launcher: **exit 2**, exact output:

```text
F01 spice_acceptance: BLOCKED: real ngspice executable is missing; no fallback.
F02 spice_interrupt: BLOCKED: real ngspice executable is missing; no fallback.
```

All new Python files passed `compile(source, path, 'exec')`; both TOML files parsed
with `tomllib`; explicit new-file whitespace and scoped `git diff --check` passed.
No `host-smoke/Cargo.lock` or local `target` directory was created.

## Source identities at that check (SHA-256)

| Source | Hash |
|---|---|
| `extension.py` | `2dd4389360e6f7595c8f306c67853bcdafc0e6e4283d80db17a64ca8c5751a2c` |
| `solver.py` | `a6a1461b55f263d89890ac72944a76976db94c7c24dde0ab9917bd477e2c80eb` |
| `interrupt_probe.py` | `161c10b7906cfa17130946cbac5246350e0f9a2d27e9fb09726cb534e83165a4` |
| `host-smoke/tests/spice.rs` | `7833f1dcea9a0daa50045a82a0b6e5cf42c4e12352b6b083fa42eb6ffca3051e` |
| `sdk/python/octet_extension/extension.py` (checkout-relative) | `73eac5061dc13a00f4cf3c48bf951e0733cade4435c36fccc580080fbd9865bd` |
| `sdk/python/octet_extension/bulk.py` (checkout-relative) | `c20c9ec800ce0d3359866a87c0797994ead3cc3df535fa40f332a5dcb0f3648e` |

## Remaining execution

The [README's exact next command](README.md#exact-next-conformance-command) is
prerequisite-fenced and reuses the parent's shared target. It requires an allocated
build slot and an already installed real ngspice; installing it remains unauthorized.
The Rust harness now compiles, but actual ngspice output compatibility/physics,
model capture and F02 native interruption remain unverified. No successful F row
is inferred from the separate SDK host suites. F03/Pi is outside this example's
test scope.

This file supersedes the initial nine-test source handoff's verification state.
Worker: `spice-example`; follow-up delegation: `243df44596d0e8ed5f6015e983083def`.
