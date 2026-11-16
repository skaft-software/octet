# Pi conformance harness

Runs native Pi and the actual octet CLI/adapter on PTYs against a loopback scripted
provider. No live providers, user credentials or ambient Pi configuration are used.
This extends [#720](https://github.com/skaft-software/octet/pull/720)'s existing
example driver, VT replay and migration-safety checks; its original commits and
attribution remain intact.

## Reproduce

Requires Unix, Python 3, Node 22.19+ and the Rust toolchain. Dependency installation
needs registry access once; execution is offline. From a clean source checkout:

```sh
npm ci --prefix scripts/pi-conformance --ignore-scripts --no-audit --no-fund
npm ci --prefix extensions/octet-pi-compat --ignore-scripts --no-audit --no-fund
cargo build -p octet-coding-agent --bin octet --profile ci-test --locked
target=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
HOME=$(mktemp -d) TMPDIR=$(mktemp -d) E2E_OCTET="$target/ci-test/octet" \
  python3 -B scripts/pi-conformance/conformance.py --representative --out /absolute/new/evidence
python3 -B scripts/pi-conformance/selftest.py
```

The 14 representative journeys have explicit success assertions, not just equality.
They cover nine unchanged Pi examples and five small callback fixtures: commands,
tools, dependency resolution, input transformation/handling, context, all three HTTP
provider hooks, dialogs, footer/status/widgets/overlay, a custom editor, session
append/name/cancellation/new/setup/switch-back, actual compaction with callbacks and
a later turn, and model switching with a later request. `journeys.json` maps each
journey to surfaces and **Pi source file:line** at the reviewed reference. Optional
`--pi-source /reviewed/source-snapshot` records its `REVISION` file without executing
source. The published npm Pi 1.5.0 CLI is the executable oracle.

For a narrow retry use `--only hook-pipeline,model-switch`. Use a fresh `--out` each
run. `--all-examples` retains #720's generic load/startup/tool-catalog corpus; it does
**not** turn each example into a fully exercised journey. Offline prerequisites in
`SKIP` are explicit skips. Errors in native Pi are failures, not automatic skips.
Historical `BLOCKED` notes are diagnostic context, never blanket waivers.
`--hosts pi` or `--hosts current` is diagnostic only; an unpaired run cannot pass.

To measure a reviewed local factory through the same generic journey:

```sh
HOME=$(mktemp -d) TMPDIR=$(mktemp -d) E2E_OCTET="$target/ci-test/octet" \
  python3 -B scripts/pi-conformance/conformance.py --skip-core \
  --extension /absolute/reviewed/factory.ts --out /absolute/new/local-evidence
```

This executes that factory and its imports with OS permissions, on both hosts.
Review it first: offline provider fixtures do not sandbox extension-owned network,
filesystem or process effects. Its dependencies must already resolve locally.
Generic results qualify loading and the offered tool catalog, not arbitrary commands
or the user's whole setup. Each leg gets its own HOME, TMPDIR, workspace and agent
directory. Existing Pi settings, auth and sessions are never imported.

Environment: `E2E_OCTET` (default `target/release/octet`), `E2E_NODE_BIN`,
`E2E_PI_ROOT` (directory containing Pi 1.5.0 and `@xterm/headless` in `node_modules`),
`E2E_TMP`, `E2E_WIRE=<prefix>` (fixture wire trace), `E2E_CPUS` (optional Linux CPU
set, applied to the correct executable on both hosts).

## Evidence format (schema 1)

- `run.json`: source HEAD/dirty flag, binary SHA-256, platform/runtime versions,
  Pi version/CLI digest, dependency lock and harness digests, exact factory set with
  digests, optional reviewed source revision and start time.
- `conformance.json`: per-journey status, per-observation values on both hosts,
  equality, failures, source citations, surfaces, final screen rows and safety.
- `conformance.md`: short human-readable comparison.
- `coverage.json`: counts and surface × journey status.
- `legs/<journey>/<host>/`: raw terminal bytes, provider requests, callback trace
  when present and leg result. These are synthetic fixture data, not user sessions.

A pass requires both hosts to execute, every boolean assertion to succeed, every
comparison to match and migration safety to hold. Equal false observations fail.
Exit status is nonzero for failed journeys. Skips never count as passes. Shared
underlying prompt/compaction text is compared after whitespace normalization;
canonical requests and raw terminal bytes remain available. Native chrome/layout
is not compared pixel-for-pixel. A transient resumed-session notification can be
erased by Pi's transcript hydration, so switch-back grades its callback receipt.

The [recorded run](results/representative.json) and
[coverage notes](results/README.md) state exact scope and supplemental regression
checks. This is representative evidence, not extension certification or closure of
all [compatibility deviations](../../docs/pi-compat-deviations.md). Independent
human reproduction of a third-party user's setup remains separate acceptance.

## Migration safety

Each isolated Pi agent directory is seeded with fake `auth.json` and a saved
session. Fingerprints (paths and hashes; symlinks by target) begin **before CLI
startup**, after reviewed configuration. Octet may touch only paths Pi touched in
the paired journey, and may never change the seeded auth/sessions. The adapter's
review/configure phase is not included in this startup fingerprint window.
