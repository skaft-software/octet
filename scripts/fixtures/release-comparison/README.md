# Frozen-release comparison recorder contract

This is a **recorder and qualification gate**, not a new terminal implementation.
No competitor timings or extension equivalence are established by these fixtures.
`synthetic-driver.py` fabricates clocks to test the recorder; its results are never
comparative evidence. The template is intentionally unfrozen and refuses all
unqualified UI/extension/resource/paint cells. Only version-command execution is
implemented internally. The parent owns frozen RC preparation, approved real
cross-client drivers, native Tern instrumentation and public artifacts.

## Operator sequence (not authorization to run a campaign)

```sh
python3 -B -m unittest discover -s scripts/tests -p test_bench_release_comparison.py
python3 -B scripts/bench-release-comparison.py template --output /PRIVATE/new-manifest.json
# Fill and review the manifest after the FINAL source/build is frozen.
python3 -B scripts/bench-release-comparison.py validate --manifest /PRIVATE/frozen-manifest.json
# Only after separate correctness/native-Tern/network approvals:
python3 -B scripts/bench-release-comparison.py run --manifest /PRIVATE/frozen-manifest.json --output /PRIVATE/new-campaign
# Optional, separate structural export; inspect privately before publication:
python3 -B scripts/bench-release-comparison.py export --raw /PRIVATE/new-campaign --output /PRIVATE/new-public-export --replacements /PRIVATE/placeholders.json
```

Every output must be new. Run records, stdout/stderr captures and synthetic homes
are owner-only; they are retained after failed correctness, nonzero exits, missing
metrics and deadlines. Never run this against the currently mutating candidate.
No retries replace failed trials. One warmup and nine attempts are defaults; at
least nine completed independent observations, with all attempts correct and
complete, are required to qualify a distribution. Additional failures are not
hidden even when nine successes exist. Warmups remain raw and labelled; a warmup correctness/process failure also blocks
qualification. CLI exit 0 means all requested cells completed (noncomparative
cells are still labelled); exit 1 means recorded incomplete/refused qualification;
exit 2 means invalid configuration or an evidence-path collision.

Each trial gets a fresh home/workspace/session: only OS file caches can be warm.
Warm-session/cache campaigns need a separately reviewed driver and explicitly
matched cache policy, not a silent change to this isolation policy. Seeded random
order or rotating/alternating order is retained per iteration. Medians and p95 use
one per-process-trial value, never pooled frames. Nine-trial interpolated p95 is
only descriptive. Summaries do not assert superiority or throughput.

## Identity and matched conditions

Record actual hardware (CPU, RAM, GPU if relevant), OS/build, viewport dimensions,
Tern version/configuration/executable digest, toolchain and runtime configuration.
The recorder also records its Python version/hash and observed OS/architecture.
Use separate manifests for bare core and equivalent feature-enabled profiles.
Extension setup comparisons use Pi/Octet/PiG; UI performance uses Octet/Pi/Codex
**CLI TUI**, not Codex app-server versus Pi TUI. Unknown settings refuse launch;
non-equivalent frontend/provider/model/workload/theme/session/policy/extension
contracts refuse comparative launch. A refused or unavailable cell has no numeric
zero and cannot be called a speed win. Noncomparative cells must say so explicitly.
Split unavailable metrics into their own cells rather than letting one missing
allocation/PSS observation obscure a qualified RSS-only cell.

`clients.NAME.argv` is an absolute argument-vector prefix, e.g. `[node, cli.js]`.
A reviewed member's `subject_args` is appended verbatim. Drivers and measured
executables are never invoked with a shell command string. Avoid managed launchers
that select a runtime from the user's HOME: pin their digest/resolution, but use
the explicit runtime and bundle in the measurement vector. No `env` or `sudo`
driver wrappers. Child PATH is a fixed OS path, not the operator's PATH. Drivers
must use absolute paths for their own runtimes/child tools.

Pin **all actual runtime inputs** as `artifacts`: launcher, selected underlying
executable/bundle, runtime, loaded extension sources, dependency files/native
modules, package inventories, themes, workload fixture, session encoder, provider
replay implementation, terminal settings and generated configuration. A pinned
inventory file alone does not verify the dependency files it lists: add those
files to `artifacts`. Hashes are checked before/after each executed trial. A file
hash alone does not establish which source or compiler produced an executable.
`provenance.kind` distinguishes `source_build` from `installed_distribution`:

- Source builds need an exact revision, complete source snapshot/tree manifest,
  compiler/flags/profile/features, isolated target directory identity and retained
  build logs. Octet RC preparation is the parent's responsibility.
- Installed distributions need package/distribution metadata and explicit known
  limitations. Pi's actual package identity comes from installed `package.json`,
  not a guessed repository. PiG is `github.com/MichaelKinsy/PiG`, not an ambiguous
  tool named Pig; Go buildinfo without `vcs.revision` is not source revision proof.
  Codex distribution labels are not compiler/source proof either.

The `setup` fields must describe **observed effective state**, not CLI intent.
Provider/model objects should include versions/revision/digests, transport,
reasoning/context/output limits and cache policy. Workload/theme/session/policy
objects should include fixture versions/digests, motion mode, session ownership,
parent chain/history size, tool/permission/sandbox configuration. Only matched
semantic fields must be equal; actual extension IDs/runtimes may differ if their
`extension_contract` pins the SAME demonstrated behavior. Record every enabled
extension, total package dependency count, and expected resident child process
count. The driver's final observed setup must match those declarations. Retain
actual state snapshots and process/dependency inventories in private raw evidence;
simply echoing the request's setup is not qualification.

`home_files` entries are literal **synthetic** relative configuration files:
`{"path":".pi/agent/settings.json","content":"{}\n","sha256":"..."}`.
No source paths are copied. Never include live credentials, real sessions or
provider payloads. Initial/final home file hashes are retained. The environment is
allowlisted and strips keys, proxies, SSH sockets, Node options and inherited
configuration. HOME/XDG/CODEX_HOME/Pi/PiG roots and TMPDIR are private trial roots.
Environment isolation is NOT a network or OS credential-store sandbox. Operator
approval must cover the driver's credential-free offline/loopback policy. No live
inference, installs, updates, global config writes or terminal manipulation are
performed by the built-in recorder.

## External JSONL driver contract

A `mode: driver` member must provide absolute `argv`, all driver/runtime artifact
hashes, `approved: true`, and an explicit `network_policy`. The driver reads one
JSON request from stdin (schema `octet.release-comparison.driver.v1`), containing
subject argv, case/checks, desired setup, host/dimensions, isolated home/workspace
and deadline. The outer runner imposes its own monotonic deadline too. It spawns
a new POSIX session and signals only its OWN process group on cleanup. Drivers
must not detach children or use shared daemons; authoritative descendant cleanup
is a correctness requirement, not inferred from reaping the direct driver.

Stdout is UTF-8 JSONL (one record per line), ending in exactly one result. Stderr
is diagnostics. Each stream is bounded to 8 MiB; overflow fails and retains the
bounded prefix with a truncation flag. Every record has the driver schema:

```json
{"schema":"octet.release-comparison.driver.v1","type":"event","name":"subject_spawn","ns":1000000}
{"schema":"octet.release-comparison.driver.v1","type":"event","name":"complete_ready_frame_received","ns":3000000}
{"schema":"octet.release-comparison.driver.v1","type":"result","correctness":{"complete_ready_frame":true},"setup":{},"metrics":{"pty_ready_ms":{"events":[0,1],"observer":"pty"}}}
```

The example `setup` and checks are abbreviated, so it is NOT valid campaign
input. Event `ns` values must be integer monotonic timestamps from one common
clock or a documented calibrated domain; do not mix OS wall clocks, provider
clocks or uncalibrated terminal clocks. Timing metrics reference two indices in
the event-only sequence. Required event names and observer domains are defined
in `CLOCKS` in the recorder; the recorder derives milliseconds itself. Additional
raw frame/provider/process events remain in `stdout.bin` (JSONL) linked from each
run's `artifact_directory`. Never use a convenient startup log as readiness.

First-editable requires application-handled edit presentation (raw mode/no line
discipline echo). Submission readiness requires authoritative acceptance without
live inference; it is not a branded frame. PTY readiness must reach the complete
synchronized frame fence. Resume requires every historical marker once in order,
canonical copy/text correctness, not a recent tail. Picker clocks require actual
edit/filter interaction and selection identity. Cancellation admission and
complete authoritative descendant settlement are different clocks. Extension
ready, teardown and process-tree settlement are likewise separate.

`input_to_paint_ms` needs `observer: native_tern`, physical terminal observation
and separate `native_tern_approved: true`. PTY byte receipt/native replay is NEVER
physical screen paint. The parent must qualify the actual Tern driver on the
frozen RC. Existing `bench-startup-resume.py`, `test-pi-api-session.py` and the Pi
PTY frame observer are useful ingredients, NOT drop-in cross-client/paint drivers.

For any unsupported metric emit `{"value":null,"reason":"specific inability"}`,
never zero. Every named correctness check must be `true`; false/missing means a
failed trial, with raw observations retained but excluded from timing summaries.
The fabricated fixture marks `synthetic: true`, which permanently refuses a
comparative conclusion. Real synthetic WORKLOAD replay is not fabricated clocks.

RSS/PSS/CPU use `scope: agent_and_all_descendants`, explicit `method`,
`tree_complete: true`, independent per-run `samples` with monotonic `ns`, total
`value`, unique positive `pids`, matching `per_pid` numeric values and
`complete: true`. Totals must equal the per-PID sum. Peak is computed per run.
The reviewer must verify census coverage including workers/extension descendants;
a boolean alone does not prove it. Detached/unseen descendants, missing PID
samples, or direct-only `bench-systems.py` observations are not total overhead.
PSS remains unavailable on hosts that do not expose it. CPU method (interval vs
lifetime `ps pcpu`) must be matched. Allocation metrics require an explicit instrumentation `method` and use
`scope: allocator_traffic`, `samples` of monotonic interval increments, and units
`calls`/`bytes`; their sum is requested allocation traffic, NOT RSS or retained
memory. RSS/PSS/CPU, allocator traffic, UI clocks, provider throughput/cache and
verified agentic success are distinct. The latter two need separately approved
same-model campaigns and are not accepted metrics here.

## Publication

Raw is private and immutable. `export` creates a NEW directory containing only
manifest/structural run records/sanitization/checksums. Literal configuration
content, homes, captures, requests and per-run diagnostics/observed correctness
payloads are omitted; status counts and stable reason codes remain. A private
replacement map (e.g. real prefixes to `$HOME`, `$WORKSPACE`, `$CONFIG_ROOT`)
replaces strings in this separate copy. The export reports placeholders/counts
without exposing source prefixes. Numeric observations are unchanged. Manually
review ALL exported fields before sharing; export is not a secret detector and
the exported manifest is documentary, not runnable (config content omitted).
