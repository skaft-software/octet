# v0.10 Pi candidate: isolated Mac qualification

This is a **historical source-candidate test recipe**, not current release status.
Its pre-launch availability and authorization notes below describe that earlier
checkpoint. See the [v0.10.0 release notes](../releases/v0.10.0.md) for launch scope.
The v0.9.6 release remains independent. See the [compatibility contract](../pi-compatibility.md),
[API ledger](../pi-extension-api.md), [release gates](../pi-compat-release-status.md),
and [measurement contract](../design/performance.md). Passing synthetic suites
is necessary but does not qualify an existing Pi setup.

## Obtain and pin the candidate

The local candidate now identifies as 0.10.0 and its single release catalog
includes Pi. Public 0.10.0 artifacts do not exist and are not qualified. The
current dirty overlay supports diagnostic source archives only: preserve
commit/tree plus overlay diff and complete source hashes, and do not call those
bytes exact HEAD. Do not create a Git repository/commit in staging or bypass
clean-tree packaging gates. Coherent release Git actions and any remote
publication need separate authorization. Rebuild and replay original setups
against settled source; historical guest/native results do not qualify it.

For an exact-commit qualification checkout, the maintainer must first supply a **published branch and full commit SHA**.
Do not substitute a moving branch, PR's older binary, or the installed release.
For the performance integration branch (once published):

```sh
git clone https://github.com/skaft-software/octet.git octet-v010-test
cd octet-v010-test
git fetch origin octet/perf-on-pi-v0.10
git checkout --detach <maintainer-supplied-full-SHA>
test "$(git rev-parse HEAD)" = '<maintainer-supplied-full-SHA>'
git status --porcelain
```

Run this inside each disposable **Cua Space macOS VM**, not in the operator's
real HOME. Keep one VM snapshot per setup and preserve the exact same snapshot
for Pi and Octet. A Python venv alone does not isolate Node extensions, HOME,
OS processes, credentials or executable effects. Use the Space's OS/VM isolation;
venvs may isolate test-driver Python dependencies within it.

Install Rust 1.88+, Node 22.19+, and Python 3.11+ with `venv` and `ensurepip`
in the VM. Verify Python before running executable-extension tests; a bare
interpreter without `ensurepip` is insufficient. Build without replacing an
installed Octet binary:

```sh
unset OCTET_STARTUP_TRACE
python3 -I -B -c 'import sys, venv, ensurepip; assert sys.version_info >= (3, 11)'
export CARGO_TARGET_DIR="$PWD/target"
cargo build --locked --release -p octet-coding-agent --bins
cargo build --locked -p octet-coding-agent --example native-editor-test-host
(cd extensions/octet-pi-compat && npm ci --ignore-scripts --no-audit --no-fund)
export OCTET_NATIVE_EDITOR_TEST_HOST="$PWD/target/debug/examples/native-editor-test-host"
(cd extensions/octet-pi-compat && npm test && npm run test:editor-native)
cargo test --locked -p octet-agent --test delegation
cargo test --locked -p octet-coding-agent --no-fail-fast
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
shasum -a 256 target/release/octet target/release/octet-host
```

Retain commit, clean-worktree status, OS/architecture, Rust/Node/npm/Pi versions,
binary hashes, build logs, test counts/skips and failures. Use distinct target
directories for before/after source snapshots. Do not copy a compiled binary
between architectures or reuse artifacts to infer source identity.

## Setup matrix and executable review

Start with separate VM snapshots for:

1. Empty Pi setup; resources only (skills, prompts, theme, keybindings).
2. A normal installed setup with dependency-bearing factories and commands.
3. Busy footer/widgets, custom editor, and their combination.
4. Builtin tool overrides, guarded tools, lifecycle hooks, and subagents.
5. Missing dependencies, malformed resources, changed/removed factories, slow
   setup/handlers, and failed provider discovery.

Pin each installed Pi/package version and preserve sources and resource filters.
Test supported Pi versions separately; a version refusal is a failed compatibility
case, not a passing empty setup. Review executable factories/imports before
activation, including installed-Pi fallback code. Do not copy `auth.json`, real
sessions, credential stores or trust grants from the operator's machine. Start
credential-free/offline; paid/live provider journeys need separate explicit
approval. `--offline` disables discovery, **not inference**: do not submit a
prompt unless the endpoint and authorization are controlled.

Use a scratch workspace outside the checkout and this checkout's binary. For
local-source zero-step mirror activation, explicitly grant this source directory:

```sh
OCTET_STARTUP_TRACE=1 /absolute/octet-v010-test/target/release/octet \
  --offline --extension-dir /absolute/octet-v010-test/extensions \
  --enable-extension octet-pi-compat 2> /absolute/private-evidence/octet-trace.txt
```

Do not add provider-credential grants, disable owner fencing, broaden frame bounds,
or suppress review to get a factory to load. Project `.pi` resource cases must
exercise both untrusted and explicitly trusted workspaces through the host's
normal trust flow. Verify that disabling the bridge withdraws its contributions
without modifying either application's saved setup.

## Behavioral and timing journeys

For every setup, exercise startup, the first picker open, `/model`, repeated
Ctrl+P cycling, thinking changes, prompts/tools/guards, editor draft and slash
completion, resize, cancellation, `/new`, resume, `/reload`, and shutdown. Check
actual requests, durable history, ordered events, process lifetime, editable
text, final frames and terminal restoration. A factory loading is not parity.
Use [the existing real-package and PTY probes](../pi-compatibility.md#qualification-and-reproduction)
with explicitly reviewed entrypoints; no optional skip counts as qualification.

Capture `OCTET_STARTUP_TRACE=1` stderr before/after **each performance change**.
Keep host phases, `extensions.process.*` step durations, and adapter
`tracePhase` marks distinct: their `elapsed` origins differ across processes.
Subtract only boundaries on the same clock. Attribute factory import/setup,
`session_start`, catalog discovery, and busy UI/event cost independently. Record
cold/warm cache state, trial order, raw trials and correctness failures. Alternate
matched Pi/Octet trials, retaining the same enabled setup, model, reasoning,
permissions, geometry and transport. Measure input-to-PTY/frame fences externally;
startup trace timestamps are not emulator paint or first-editable-frame timings.

A credential-free real-adapter idle-switch diagnostic is available separately:

```sh
OCTET_STARTUP_TRACE=1 cargo test --locked -p octet-coding-agent --lib \
  profile_idle_model_switch -- --ignored --nocapture
```

It reports ten in-process switch samples and factory load count for a synthetic
factory. The [local before/after record](../benchmarks/idle-model-switch-2026-10-22/README.md)
retains its raw samples and limitations. It does **not** measure a populated
mirrored setup or prove Octet faster than Pi. Do not enable launch-time fleet
prefetch by default without maintainer
approval: it contacts configured providers that the user did not select.

Keep raw captures private to the Space. Publish only reviewed, sanitized evidence
without credentials, private paths, prompts, sessions or tool payloads. Report
passes, Octet defects, missing prerequisites, unsupported contracts and optional
skips separately, with reproducible steps and the exact pinned candidate. A red
case blocks that setup's parity claim even when other setups pass.
