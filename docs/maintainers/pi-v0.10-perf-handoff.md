# v0.10 Pi setup startup: performance handoff

Status at commit `ba2f0539` on `release/v0.10.0-candidate` (2026-11-02). Read this
before any startup or Pi-compat performance work.

## Where qualification stands

- The local qualification ledger has 54 PASS and 1 FAIL out of 55 rows. The one FAIL
  is setup row (a): the user's real Pi 1.0.2 setup, with 28 factories from 29 original
  packages, replayed side by side with Pi.
- Row (a) is at functional parity on all six recorded legs: cold, warm and
  providerless, all with exit 0 and an exact terminal restore. It fails **only on
  startup time**.
- "Full UI" means the powerline footer is visible, i.e. the extensions are live:

| full UI | Pi 1.0.2 | Octet now | gap |
|---|---|---|---|
| cold (fresh HOME) | 22.4 s | 34.2 s | +11.7 s |
| warm (second launch) | 5.1 s | 9.4 s | +4.3 s |

Octet's editor is already typeable at 0.4 s warm (3.1 s cold), compared with 5.1 s and
22.4 s for Pi. The gap is entirely in getting the Pi extensions live.

## Next step: do it once and never again

The direction from the maintainer is to bring back octet's **do it once and never
again** performance philosophy, and to make octet *faster than Pi*: negative overhead,
even with the process wire between the Rust host and the compat runner. Pi pays
its whole extension-loading cost on every launch. Octet should pay each unit of
work once, persist the result, and never repeat it while its inputs are unchanged.

The current startup breaks that rule in several measured places:

1. **Factories are imported twice on first launch.** `auto-configure.mjs` captures
   registrations by running `runner.mjs --inspect`, which imports every factory and
   exits. The host then spawns the real runner, which imports everything again
   (about 5.5–7 s from a warm transform cache). Fix: one import. Either the
   capture process is adopted as the live runner, or the runner captures in-process
   and the host admits from the handshake before any grant or tool decision. Admission,
   trust and grant fences must not weaken. This touches the host's admission flow;
   keep it a generic host primitive (see the lean-core rule below).
2. **Source review is hashed repeatedly.** After this commit, `reviewedSourceHashes`
   runs once in prepare, once in `configure()`, and once more in the runner, which
   re-checks before import. Each pass walks about 682 files (about 0.6 s). Hash
   once per launch, carry the digest set forward, and keep exactly one check
   immediately before import.
3. **The jiti module resolution is re-derived every launch.** About 3.4–3.7 s per
   launch goes to `jitiResolve` (stat storms, thrown `NodeError`s). Pi pays this
   too, so caching it is where octet can **beat** Pi. Persist a resolution map
   (parent and specifier mapped to the resolved file), validated cheaply by
   mtime/size or the reviewed digests, and reuse it across launches.
4. **Transforms are content-cached but serial on a cold start.** This commit adds
   `lib/transform-warmup.mjs`, which transforms the reviewed TypeScript closure in
   parallel worker threads into jiti's own content-validated cache. It runs only
   transforms, never factory code. Cold prepare dropped from 28.7 s to 25.0 s. The
   cold first frame moved from 0.4 s to 3.1 s, likely CPU contention from the
   workers; bound the workers or lower their priority so the first frame stays
   instant.
5. **The warm path still runs prepare before the runner.** On a catalog-cache hit,
   prepare is deterministic (2.0 s). Overlap it with runner startup, or let the
   runner start from the cached catalog while prepare verifies, refusing on any
   mismatch before a factory executes.

Target: cold and warm full UI below Pi's on the same machine, recorded side by side.

## Rules that still apply

- **Lean core.** The Rust core stays Pi-agnostic. Pi-compat fixes go in
  `extensions/octet-pi-compat` first. Touch the core only for a generic host
  primitive, such as "an extension may supply its contributions at handshake".
- **No weakening.** Do not weaken trust, review, source-integrity, owner/generation
  fences, credential boundaries or test deadlines to gain speed. Caches are
  optimizations; a stale or unreadable cache must fall back, never fail open.
- **Measure, don't claim.** Use alternating Pi/octet legs on fresh HOMEs, with full
  UI as the readiness signal. `OCTET_STARTUP_TRACE=1` prints the host phases
  (`extensions.pi_mirror.prepare.*`, `extensions.attached`) and per-factory
  `adapter.factory.import.N` timings. Expect noise on a loaded machine; compare
  runs alternated Pi/octet back to back.

## Notes from the last measurement pass

- Pi's `[Extension issues]` block on the reference legs (built-in `mcp` versus
  `pi-mcp-adapter`, and typebox/pi-tui declared as dependencies) is Pi's own
  diagnostic about the user's packages. It is reference behaviour, not an octet
  failure.
- The slowest warm factory imports were `pi-background-tasks` and `pi-hermes-memory`
  (about 1.2 s each), then `pi-subagents`, `pi-mcp-adapter` and `pi-lens`. One
  `session_start` handler took about 1.1 s after load.
- Focused adapter tests for the startup files pass in an isolated HOME. Rerun the
  full adapter suite and the Rust suites on this commit before any release claim.
