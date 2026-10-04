# SPICE extension source demonstration (F)

**F01 and F02 are BLOCKED, not passed.** Real `ngspice` is absent in the
implementation environment; installing/downloading it is not authorized. There
is no fallback solver or generated waveform. Parser tests are not simulator or
host acceptance. The example uses the actual working-tree Python SDK resource,
BlobRef and bounded bulk helpers; no second RPC implementation or compatibility
adapter is embedded.

This is an unpackaged macOS/Linux source example for the working-tree API 0.4
Python SDK, not a published bundle or an installed-host qualification. See the
[approved design](../../../docs/design/extension-values-v1.md),
[conformance matrix](../../../docs/design/extension-values-v1-conformance.md), and
[SDK guide](../../../sdk/python/README.md).

## Typed SDK workflow

`extension.py` loads the adjacent source SDK without installing anything. Its
four `typed_tool` declarations generate schemas and descriptors from dataclasses
and `Resource[T]` / `BlobRef` annotations. Native nominal types are declared once;
`receiver` only marks presentation. All calls use explicit ordinary arguments.

| Tool | Input | Output |
|---|---|---|
| `spice_open` | `{"netlist":"rc.cir"}` (default) | `{circuit: Resource[Circuit]}` |
| `spice_instantiate` | `{circuit: ...}` | `{session: Resource[SimulationSession]}` |
| `spice_transient` | `{session: ...}` | `WaveformRef` descriptor |
| `spice_measure` | The same `WaveformRef` descriptor | Finite scalar `Measurement` |

`WaveformRef` contains only the SDK BlobRef, sample count, encoding, signal and
units. `ext.bulk.publish_bytes` hashes/commits the small bounded owned buffer;
`ext.bulk.read(..., max_bytes=...)` authenticates, verifies, closes and releases
its read lease. No transfer path is returned to these handlers. Diagnostics
use the SDK's existing `TypedResult` / `Diagnostic` profile. Explicit bounded text
summaries include the SDK-generated ResourceRef/BlobRef descriptor so the model
can use it; structured details alone are not model-visible text. Neither summary
nor structured result contains waveform sample bytes.

`extension.toml.example` is a local template, not an auto-enabled bundle. To use
it later, copy it to `extension.toml` and replace its absolute script path; the
child cwd is the host workspace, not this directory. Keep the source checkout
layout. The host must offer `request_progress`, `resource_refs_v1`,
`operation_descriptors_v1`, and `bulk_objects_v1` with a bound `local-file.v1`
store. Missing resource/bulk support fails negotiation; missing progress support
refuses transient before starting the solver. Trust/enablement is explicit and
capabilities are consent metadata, not an OS sandbox.

## Backend and bounds

- `rc.cir` is the only input: a 1 V source charging 1 uF through 1 kohm, initial
  capacitor voltage zero, 10 us requested step, 5 ms stop. No arbitrary paths,
  netlists, `.control`, `.include`, shell expressions, or extra solver arguments
  are accepted from tool input.
- Native `Circuit` and `SimulationSession` stay local. A session copies only the
  circuit's immutable source text; it does not retain a mutable Circuit alias.
- `solver.py` invokes an argv list `ngspice -n -b rc.cir`. Startup scripts are
  disabled, numeric locale is C, stdin is closed, and combined stdout/stderr is
  captured in a bounded pipe. `.print tran v(out)` creates no waveform/log file.
  The call owns a private temporary directory and its <=4 KiB input file until
  execution has settled, then removes it.
- Output is bounded to 1 MiB and 10,000 samples / 160,000 binary bytes. Solver
  work has a 30-second deadline and a 50 ms cancellation polling wait. Cleanup is
  a separate execution-settlement fence. Every subprocess exit
  path terminates a still-running child, waits one second, kills if necessary,
  and waits for settlement **before** returning/raising. The pipe closes before
  temporary files are reclaimed. Extension crash cleanup remains host-supervised.
- The local parser supports ngspice's ASCII paginated `.print` table, rejects
  invalid indices/nonfinite values/nonmonotone time, and checks RC plausibility.
  The binary domain format is little-endian interleaved float64 `(time_s, v_out)`.
  Final time must be within 1 ns of 5 ms; final voltage within 2 mV of
  `1-exp(-5)` (about 0.993262 V). It is deliberately not a general SPICE parser.
- Solver stdout/stderr, numerical sample bytes and scratch paths never become
  result text or diagnostics. Domain failures have bounded stable codes/messages.
  The adapter supplies ordinary SDK cancellation and ephemeral status progress;
  neither carries sample data. No compression/numpy/external Python dependency.

These application limits and capability declarations are not an OS sandbox or
OS disk/CPU quota. The reviewed bundled source and installed executable are trusted.

## Checks without installation

From the checkout:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 examples/extensions/spice/check_prerequisites.py
# Missing ngspice: prints BLOCKED, exits 2. Finding it does not pass F01/F02.

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s examples/extensions/spice -p 'test_*.py' -v
```

The **14 tests** comprise five parser/domain checks, five static SDK/schema/text
projection checks, and four process-utility/prerequisite checks. The handwritten
three-row parser fixture is not captured ngspice output and never reaches the
backend. Process-utility tests launch explicitly labeled ordinary Python children
to test terminate/wait/kill and barrier mechanics; they never impersonate ngspice
or generate waveform data. They are not F acceptance. No skips turn missing
prerequisites green. Extension startup also exits 2 with BLOCKED if ngspice is absent.

## Production-host harness (compiled; execution blocked)

`host-smoke/tests/spice.rs` supplies stable `f01_spice_acceptance` and
`f02_spice_interrupt` tests against the production `ExtensionProcess` and this
actual source SDK process. There is no synthetic RPC host. Before discovery,
F01 captures an actual Agent provider request and checks that `spice_open` is
visible while resource-consuming operations are hidden. F01 then drives:

1. Explicitly select/call `spice_open`: `rc.cir` -> `Resource[Circuit]`.
2. Host lookup selects instantiate with `/circuit`; explicit ordinary arguments
   instantiate `Resource[SimulationSession]`.
3. Host lookup selects transient with `/session`; ordinary `tool/call` emits
   ephemeral progress, runs the actual solver, commits a BlobRef, and returns only
   a domain `WaveformRef` descriptor. Native objects and transfer locators stay local.
4. Measurement reads through an authorized SDK lease, closes it, and returns finite
   scalar values. Capture the actual model request to prove no sample bytes or
   locators entered it.
5. The **host-owned release action** releases the session and records cleanup,
   then reuse must fail before target invocation. Do not implement an ordinary
   pinned `release(session)` tool: that would correctly receive `resource_busy`.

For harness evidence, launch the extension with `--events /private/new-events.jsonl`.
It exclusively creates a mode-0600 append-only log, bounded to 64 KiB, containing
only extension PID, event names and owned solver PID. It logs tool entry, native
cleanup, solver start and **post-wait** settlement; paths and payloads are absent.
The harness owns that log, private HOME/workspace/TMPDIR, and cleanup. F01 uses a
loopback-only scripted model endpoint to capture the real Agent requests; it
performs no external inference. It asserts exact selected schema projection,
ordinary progress, descriptor-only model text, scalar physics and host release.

F02 adds `--interrupt-barrier /private/directory` **only at harness startup**.
`interrupt_probe.py` SIGSTOPs the real post-exec ngspice child and confirms its
stopped state with `waitpid`; it manufactures no output. The harness cancels the
ordinary call, waits for `interrupt_cancelled`, proves release is still
`resource_busy`, and writes `allow_stop`. Only then does the extension resume,
terminate and wait the child, before its terminal reply. The test checks PID
absence, no waveform commit, unchanged generation, disposal and rejected reuse.
These are explicit event/marker barriers, not delay-based race guesses. If the
fast solver exits before the stopped-state barrier, F02 reports BLOCKED/nonzero,
never a pass. This tests cancellation of a real pending simulator execution, not
interruption at a specified numerical integration/convergence step.

## Exact next conformance command

From the checkout, **only after the parent allocates the heavy-build slot**:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 examples/extensions/spice/run_conformance.py \
  --run-host \
  --target-dir /Users/achumukundan/octet-rc/execute-20261003-180557/target-core \
  --evidence examples/extensions/spice/evidence-next
```

The prerequisite guard currently exits **2**, explicitly listing both F01/F02 as
BLOCKED **before running Cargo or creating evidence/target directories**. Nothing
installs or downloads ngspice. With prerequisites available and the build slot
approved, it runs the two Rust tests offline, one Cargo job/test thread, `ci-test`
profile, reusing the chosen target. Its checked-in standalone lockfile was
resolved offline from cached dependencies; the runner uses `--offline --locked`.
Metadata resolution is not compilation or simulator acceptance. A missing
cache/build failure is not a pass. Choose a fresh evidence directory for each run.
The runner retains source hashes and the harness writes bounded PID-bearing logs,
model requests and measurement evidence. Calling the Rust tests directly also
fails on missing ngspice; neither test is ignored or returns early as success.

**Current evidence:** 14 Python source/utility checks and an offline, locked Rust
harness build (`cargo test --no-run`) passed. No ngspice execution, host F test,
or local/external provider call was run. Compilation does not qualify the
runtime assertions. F01/F02 remain BLOCKED; F03/Pi and prior A–E gates belong to
their separate suites.
See [VERIFICATION.md](VERIFICATION.md) for the observed source-check commands.
