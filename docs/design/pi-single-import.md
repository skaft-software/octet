# Pi mirror startup: one import, and prepare off the warm critical path

Status: design plus a first slice (warm-path source walk dedupe). Parity reference:
Pi 1.4.0. Scope: perf handoff items #1 (double factory import) and #5 (warm path
runs prepare before the runner). Items #2-#4 belong to other work and are only
referenced. Line numbers are against `release/v0.10.0-candidate` at `7a917bd4`
plus this branch.

## 1. Current flow

```
host lifecycle  ->  pi_mirror::prepare_admitted  ->  node auto-configure.mjs
  -> configureMirror -> [cache hit: reuse | miss: captureCombined -> runner.mjs --inspect]
  -> configure() writes bridge.json + extension.toml  -> host parses manifest
  -> envelope ensure!  -> finish_preparation  -> tool-clash decisions -> catalog
  -> binding.activate -> spawn real runner.mjs -> initialize -> Runtime.load()
```

| Step | Where |
|---|---|
| Deferred decision: `defer_pi_mirror` when startup is after first frame | `crates/octet-coding-agent/src/extensions/lifecycle.rs:220-228` |
| Deferred path spawns prepare as a task; every startable extension waits on it | `lifecycle.rs:511-520`, `spawn_deferred_pi_preparation` `lifecycle.rs:1976-1990` (`extensions.pi_mirror.prepare.begin/end`) |
| Prepare completes: manifest swapped in, then tool clashes recomputed, catalog rebuilt, activations spawned | `finish_deferred_pi_preparation` `lifecycle.rs:687-760` |
| Host spawns `auto-configure.mjs <tmp>/octet-pi-compat <trusted>` with a sanitized env, 180 s timeout, 64 KiB output cap | `pi_mirror.rs:113-177` |
| Host reads generated `extension.toml` (64 KiB cap), checks name, then the capability envelope: `builtin_tool_overrides` must be a subset of the shipped manifest's and `provider_credentials` must be false | `pi_mirror.rs:179-201` |
| `finish_preparation` copies only entrypoint, contributes, capabilities onto the already-admitted descriptor; source, trust and activation identity are kept | `pi_mirror.rs:84-111` |
| `auto-configure.mjs` -> `configureMirror({reviewed, cache:true})` | `auto-configure.mjs`, `configure.mjs:305` |
| Pass 1 source walk (`mirrorPreparation` -> `reviewedSourceHashes`, ~682 files) and review fingerprint | `configure.mjs` `mirrorPreparation` (~`:150-175`) |
| Catalog cache read: key covers review, adapter identity, Node, Pi version, env, native tools | `mirrorCache` `configure.mjs:270-300` |
| Cache miss: `captureCombined` scans import gaps then spawns `runner.mjs --config <tmp> --inspect` | `configure.mjs:229-262`, `capture` `:45-56` |
| Capture child imports and runs every factory (`runtime.load()`), prints one metadata frame, exits | `runner.mjs:51-57`, `lib/runtime.mjs:330` |
| Pass 2 source walk inside `configure()`, then `bridge.json` + `extension.toml` written (static registrations, hooks, shortcuts, flags, envelope) | `configure.mjs:58-136` |
| Host spawns the real runner from the manifest args (`runner.mjs --config <tmp>/bridge.json`), sends `initialize` with `contributes` from the manifest, flag values, host state | `crates/octet-agent/src/extension_process/spawn.rs:630-735`, manager `extension_runtime/manager.rs:1387` |
| Runner `initialize` -> `load()` (pass 3 source walk compared with `bridge.json` `entrypoint_sources`, then factory imports) -> live metadata must equal the declared manifest and the static registrations or the runner refuses | `lib/runtime.mjs:330-415`, `:534-620` |
| Host negotiates the response against the manifest (response must not exceed it) | `spawn.rs:742-775` |

### Where the double import happens

On a catalog-cache miss the factories execute once in the capture child
(`runner.mjs --inspect`, `configure.mjs:45-56`) and once in the live runner
(`Runtime.load()`, `runtime.mjs:330`). The second import is warm for jiti's
transform cache, so the 5.5-7 s is evaluation of the module graph and each
factory body, not transpilation. Nothing in between persists a loaded module
graph; processes cannot hand one over.

Why the host cannot simply admit "from the live runner" today, found while
reading the flow:

1. **Contributions are consent metadata that precede the spawn.** The host takes
   `tools`, `commands`, `hooks`, `shortcuts`, `flags` from the manifest to run
   tool-clash resolution (`turn_off_tool_clashes`, `lifecycle.rs:1760`), build the
   runtime catalog and decide `withheld_tools` before it launches any process.
2. **Flags are an input to `initialize`.** `flag_values` are projected from the
   manifest's declared flags (`spawn.rs:640`, `:666`), and the CLI parses those
   flags before spawn. A runner that discovers its flags at load cannot receive
   their values in the same handshake.
3. **Capture runs without a host.** The capture child has no `initialize`, so no
   workspace, flag values or host state; the live load has them (`runtime.mjs`
   sets `initialHost`, `workspace`, `flagValues` before `load()`). Adopting the
   capture process as the live runner would hand factories a context they did
   not set up with. That would be a behavior change from Pi, not just a speedup.

So (a) "adopt the capture process" is rejected: it needs a process-handoff
primitive in the host and still has problem 3. Option (b) needs a protocol
addition (section 3).

## 2. #5: the warm path

Measured on an 8-factory fixture in an isolated HOME (`auto-configure.mjs`,
cache hit): cold 1.5 s, warm 0.27-0.32 s. Runner boot to `adapter.entry` is
0.27 s. The realistic 28-factory setup spends ~2.0 s in warm prepare, so on the
real setup the cost is almost entirely the repeated source-review walks, not
Node boot or module loading:

- Node boot + adapter module graph in prepare: ~0.3 s (shared with the runner,
  already served by the on-disk compile cache; lazy-loading the cold-only
  modules would save tens of ms, not worth the churn).
- Pass 1 walk (mirror preparation) and pass 2 walk (`configure()`), ~0.6 s each
  per the handoff.
- Pass 3 in the runner is the one that must stay (see section 4).

Overlapping the runner's boot with prepare therefore saves at most the ~0.3-0.5 s
runner boot, because the runner's `--config` argument, the manifest and the
clash/catalog decisions all depend on prepare's output. A real overlap needs a
host "pre-spawn, hold at initialize" primitive: launch the runner process
speculatively, withhold `initialize` (the only point at which any factory can
execute) until prepare's manifest is verified byte-identical to what the
speculative process was launched with, kill and respawn on mismatch. That is
sound (nothing executes before verification) but it requires the speculative
manifest and bridge directory to be stable across launches, which they are not
today (`tempfile` per launch), and a runtime-manager change to start a process
without a catalog entry. It is recorded as phase 2 below, not done here, because
its win is bounded by the boot cost measured above.

### 2.1 Implemented slice: one walk on a cache hit

`configureMirror` already skips a second `mirrorPreparation` on reuse
(`configure.mjs:355`). The second source walk inside `configure()` was still
executed. It guarded against edits between preparation and generation; on a
cache hit no factory runs in that interval (everything is synchronous reads in
one process), so it protected nothing the runner's own pre-import check does not.
Change: on a hit, `configure()` receives the digests from pass 1
(`carriedSourceHashes`, `configure.mjs:58`, `:73`, `:362`) instead of walking
again. On a miss (a capture executes factories in between) the second walk stays.
`sourceWalks` (exported counter) makes the property testable.

Saving: one ~0.6 s walk per warm launch on the reference setup (estimate from
the handoff, not re-measured on the 28-factory setup here).

Merge note: item #2's owner will rework the same walks (carry digests across
the runner too). This slice touches only `configure()`'s signature, the
`entrypoint_sources` expression and one call site, and renames two call sites to
the counting wrapper `hashSources`.

## 3. #1: proposed single-import flow (not implemented)

Generic host primitive: **contributions at handshake** for an extension whose
manifest opts in (e.g. `[entrypoint] contributes_at_handshake = true`, API
version gated by a negotiated feature). Rust stays Pi-agnostic; the Pi mirror
keeps its Pi-specific preparation only for resource discovery and trust inputs.

Flow on a catalog-cache miss:

1. Host admits the extension exactly as today: enablement, source trust,
   integrity, process policy, `start_decision == Allowed`. The shipped manifest
   states the maximum envelope (`builtin_tool_overrides` ceiling, no
   `provider_credentials`, process/network/filesystem declarations).
2. Host spawns the runner with a launch-time `contributes` of "none declared,
   to be supplied". `initialize` carries workspace, host state and *flag values
   as raw CLI input* (the runner resolves them against the flags it registers).
3. Runner imports factories once, in-process, with the real context. Its
   `initialize` response carries the complete contribution set (what capture
   returns today) plus the `entrypoint_sources` digests it verified.
4. Host validates the response before it activates anything (register tools,
   subscribe hooks, shortcuts, flags): the same `ensure!` as
   `pi_mirror.rs:186-200` (subset of the shipped override ceiling, no
   `provider_credentials`), identifier validation (`manifest.rs:382`), bounds
   (256 tools/commands, 64 flags/shortcuts as in `runtime.mjs:489`), and
   tool-clash resolution; a clashing tool becomes `withheld`, the existing
   mechanism that already drops contributions after initialize
   (`spawn.rs:~762`) instead of turning the whole extension off.
5. Host persists the accepted contribution set in the catalog cache, which
   today is `pi-mirror-<slot>.json`; later launches are the existing warm path.

Open points that make this a protocol change, not a patch: flags must be
declared before CLI parsing (so a first launch cannot accept an unknown
`--flag`; Pi 1.4.0 has the same ordering because flags are collected from
extensions before parsing), and shortcuts/hooks that the host admits
"during native startup" (`runtime.mjs:~611`) would arrive after first frame.
Staging: ship the primitive behind the manifest opt-in, keep the capture path
as fallback, flip the Pi mirror when the tests below pass. Interim lever that
needs no host change: raise capture parallelism / skip per-factory route
probing, which are cold-only costs.

## 4. Security argument

What is checked, when, by whom, before which side effect:

| Check | Owner | When | Side effect it gates |
|---|---|---|---|
| Enablement, source trust, integrity, process policy (`start_decision == Allowed`) | host | before prepare is spawned (`needs_preparation`, `pi_mirror.rs:68-82`) | any factory execution (capture or live) |
| Capability envelope (`builtin_tool_overrides` subset, no `provider_credentials`) | host | after prepare, before the manifest is adopted | catalog, tool registration, runner spawn |
| Review fingerprint of the Pi setup; cache key covers adapter identity, Node, Pi version, env | adapter | warm: before the cache is trusted; cold: before capture | factory execution in capture |
| Source digests (`entrypoint_sources`) | adapter runner | immediately before each import (`runtime.mjs:408`) | that factory's import |
| Static registrations equal live metadata; declared contributes equal live | runner and host negotiation | at `initialize` | exposing any contribution |
| Credentials: sanitized env for prepare, named selectors only | host | at spawn | secrets reaching factories |

The warm-path slice removes only a walk whose guarded window contains no
untrusted execution; the pre-import digest check in the runner (the one that
runs "immediately before import") is untouched, and a new test shows a source
edited after a cached prepare is refused before any factory runs. The
phase-2 proposals keep the same ordering: nothing executes before the
speculative launch is verified identical; in #1 the envelope and clash checks
run before any contribution is activated, though factory code has by then
executed, which is already true of the capture child today (capture executes
factories before the host sees the manifest).

Caches stay optimizations. A stale, oversized or unreadable catalog cache
returns `undefined` (`mirrorCache.read`) and the flow falls back to a fresh
capture; a failed prepare disables the bridge with a diagnostic
(`finish_preparation`), never an empty "working" mirror.

## 5. Failure and fallback

- Prepare timeout/failure: unchanged, 180 s, bridge disabled with an error
  diagnostic.
- Hit with a drifted source: the runner's pre-import check refuses and reports
  (`reviewed factory sources changed`); the next launch re-reviews because the
  cache key includes the review fingerprint.
- #1 handshake failure: fall back to the capture flow for that launch and
  record the failure so the opt-in is not retried every start.

## 6. Tests

Done (adapter, isolated HOME fixtures): `pi-mirror-cold-start.test.mjs`
(9/9), plus `pi-mirror-static`, `pi-mirror-dynamic-tools`, `source-review`,
`pi-setup` (41/41). New cases: one source walk per factory on a cache hit with
identical reviewed digests; a source edited after a cached prepare is refused
before any factory runs.

Wanted for phase 2: Rust test that a speculative runner never receives
`initialize` before manifest verification and is replaced on mismatch; Rust test
that a handshake contribution set exceeding the envelope is refused before any
tool is registered; adapter test that cold first launch imports each factory
exactly once (today `pi-mirror-cold-start` asserts the double import,
`['single','single']`).
