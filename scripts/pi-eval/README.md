# Pi extensions eval

The original package/PTY harness measures whether octet runs real Pi extensions
the way Pi does, in production shape: the real `octet` binary with the shipped
`extensions/octet-pi-compat` bundle in auto-mirror mode, against real Pi **1.4.0**
(`@earendil-works/pi-coding-agent@1.4.0`), both on a PTY with the same 23 pinned npm
Pi packages and mock provider. That historical reference pin remains unchanged;
it is not the adapter's current support ceiling. Startup is the first metric;
parity metrics ride along. The separate prompt/tool loop below compares Pi
1.0.2 and **1.5.0** by default.

## Prompt/tool comparison loop (no installation)

`prompts.py` runs the actual CLIs with synthetic factories in fresh canonical
homes/workspaces and a local scripted provider. It never loads the user's
factories, installs packages, uses ambient credentials or calls a paid provider.
It tests startup ordering, chained system prompts, input transformation/chaining,
handled-input suppression (including Octet persistence), a real tool roundtrip,
and transport of all 117 large tool definitions.

```sh
python3 -B scripts/pi-eval/prompts_selftest.py
python3 -B scripts/pi-eval/prompts.py \
  --octet /absolute/path/to/built/octet \
  --pi-install /absolute/path/to/existing/.pi/agent/install \
  --pi-versions 1.0.2,1.5.0 --reps 2 \
  --out /private/tmp/octet-prompt-comparison
```

Use `--cases input-transform,input-chain,input-handled` for a narrow retry,
`--octet-mode plain` to exercise the plain frontend, and a fresh `--out` each run.
Both reference versions must already exist in the managed install. Octet supports
Pi **1.5.0** through its reviewed capability manifest and version-specific helper
routes; see [compatibility and limits](../../docs/pi-compatibility.md). This loop
checks only its listed synthetic prompt/tool cases, not complete runtime, helper
or original-package parity. The older 23-package/PTY evaluation below remains
pinned to Pi **1.4.0**.

Every leg retains raw provider requests, semantic observations and stdout/stderr;
`results.jsonl` records failed legs and `comparisons.jsonl` records equality.
Repetitions alternate execution order. The command fails on either an unmet
case assertion or a cross-CLI mismatch, not on answer wording. Comparisons retain
assistant tool names/arguments and full fixture tool schemas, normalizing only
generated IDs, content framing and omitted OpenAI `strict: false`. A common
extension-owned base removes intentional native default-prompt differences;
original wire requests remain available. These synthetic prompt/tool receipts
do not qualify the user's full setup or UI, settings, helper and lifecycle APIs.

## Run

```sh
# needs: node >= 22.19, npm, python3, network on the first run (fixture build), a built octet
python3 scripts/pi-eval/selftest.py                      # parsing/scoring logic, no processes
python3 scripts/pi-eval/fixture.py --probe               # optional: build + probe the fixture now

python3 scripts/pi-eval/run.py --reps 5 --variant baseline            # cold+warm, pi+octet
python3 scripts/pi-eval/run.py --reps 5 --variant v1 --baseline .claude/hillclimb/pi-extensions/baseline
python3 scripts/pi-eval/run.py --legs warm --targets octet --reps 3 --variant v1   # one leg while hillclimbing
python3 scripts/pi-eval/run.py --variant v1 --report-only                          # reprint the report
```

Options: `--octet BIN` (default `target/release/octet`), `--legs cold,warm`, `--reps N`,
`--targets pi,octet`, `--variant baseline|vN`, `--out DIR` (default
`.claude/hillclimb/pi-extensions/<variant>/`), `--baseline DIR`, `--ceiling S` (hard per-leg
limit, default 180), `--cache DIR` (fixture cache; `$PI_EVAL_CACHE` or `~/.cache/octet-pi-eval`),
`--scratch DIR`, `--keep`, `--force-fixture`, `--extensions-dir DIR` (extensions root holding
`octet-pi-compat/`, default this checkout's `extensions/`; point it at a `git worktree` of an older commit
to measure another adapter revision with the same octet binary, the adapter is Node and read live).

This executes downloaded third-party code (Pi 1.4.0 and the locked npm packages run, unsandboxed,
under a throwaway HOME). The fixture build itself uses `npm install --ignore-scripts`, so no
package lifecycle script runs; the legs and the probe do execute the packages' extension code.

## Fixture (`fixture.py`, `packages.lock.json`, `sentinel/`)

`packages.lock.json` pins Pi 1.4.0 and 23 packages, each the newest version whose pi peer ranges
admit Pi 1.4.0 (`pi-background-tasks` has none, so it stays at its newest release as the
"peer range excludes Pi 1.4.0" case). Three further candidates (`pi-pretty`, `pi-memory`,
`pi-web-search`) are listed under `dropped_for_pi_conflicts`: Pi 1.4.0 refuses to start when two
extensions register the same tool name, so a fixture containing them cannot be started by Pi at all.
(octet starts anyway; that leniency is a behavioural difference worth knowing about, not a metric.)

The built home is cached at `<cache>/<hash>/` (hash over lock, sentinel, probe, layout version):

```
.pi/agent/install/current-version                 1.4.0
.pi/agent/install/releases/1.4.0/node_modules/@earendil-works/pi-coding-agent
.pi/agent/npm/{package.json,.gitignore,node_modules/...}   Pi's own package-manager layout
.pi/agent/pi-eval/pi-eval-sentinel.ts             readiness sentinel
.pi/agent/settings.json                           packages: ["npm:<name>@<ver>", ...],
                                                  extensions: ["pi-eval/pi-eval-sentinel.ts"], default model
```

Packages are installed exactly as Pi's `DefaultPackageManager.installNpmBatch` does:
`npm install <specs...> --prefix <agentDir>/npm --legacy-peer-deps` (a combined install without
`--legacy-peer-deps` fails on peer conflicts), plus `--ignore-scripts --no-audit --no-fund`.
`fixture.json` records installed versions and the peer-exclusion list; `pi-probe.json` is the
Pi-side load/command report (below). Each leg copies the cached home to a fresh HOME
(`cp -a --reflink=auto`); each leg also writes the mock provider's port into
`~/.pi/agent/models.json` (Pi) and `~/.octet/credentials/custom.json` (octet).

## Legs

For each rep and leg the targets alternate back to back (order flips every rep):

- **cold**: a fresh copy of the fixture home, no caches of either target.
- **warm**: the same home immediately after that target's cold launch (second launch; octet's
  mirror/catalog/transform caches and Pi's jiti cache are populated). If the cold leg was
  skipped by resume, an unmeasured priming launch runs first.

PTY 120x40, `TERM=xterm-256color`. octet runs with `OCTET_STARTUP_TRACE=1`; stderr goes to
`work/<leg>/stderr.log` (not the PTY) so trace lines cannot corrupt the screen, and the
generated mirror `bridge.json` is copied out of `$TMPDIR/octet-pi-mirror-*` while octet runs.

## Metrics and readiness definitions

| id | meaning (better) |
|---|---|
| `typeable_s` | seconds from spawn to the model label (`mock-1`) on screen: octet's status line / Pi's footer (lower) |
| `full_ui_s` | seconds from spawn to `PI-EVAL-READY` on screen. The sentinel extension sets it on `session_start` through `ctx.ui.setStatus` and `ctx.ui.setWidget`, so it means every configured extension is loaded and `session_start` was delivered. **Headline: octet/Pi `full_ui_s` ratio of medians per leg, below 1.0 means octet is faster** (lower) |
| `packages_parity` | fraction of locked packages whose loaded/not-loaded state matches Pi (higher) |
| `command_parity` | Jaccard of extension slash-command names, octet vs Pi (higher) |
| `clean_exit` | 1 if Ctrl+D exits 0 within 15 s and every terminal mode octet/Pi turned on is turned off again (alt screen, mouse, bracketed paste, focus, kitty keyboard, cursor visibility) |

Pi-side truth (untimed, once per fixture): `pi_probe.mjs` loads the home through Pi 1.4.0's own
`DefaultResourceLoader` and reports each extension's path, commands and any load errors. A package
loads in Pi if one of its extensions is listed (a tool-name conflict recorded as an error still
counts as loaded, with the detail kept). octet-side: the generated `bridge.json` entries minus
`mirror_excluded`, minus any `[pi-compat] skipped <entry>` line seen on screen or stderr, and (when
the trace has them) each factory must have an `adapter.factory.registered.N` line; commands come
from `bridge.json` `registrations.commands`. Pi rows report parity 1.0 and are flagged
`meta.reference`. Informational: `meta.peer_excluded_pi14` lists packages whose peer ranges exclude
Pi 1.4.0. Per-package pi/octet status, command differences, startup error/issue lines
(`[Extension issues]`, `[octet-pi-compat Warning]`, `[pi-compat] skipped ...`) and the parsed trace
(host phases such as `extensions.pi_mirror.prepare.*`, `extensions.attached`, `frame.ready`; adapter
phases `adapter.load.ready` and `adapter.factory.import.N`) are in each row's `meta`.

## Output (claude-api build-eval layout)

`<out>/results.jsonl` one row per (case, rep), case = `<leg>.<target>` (`prompt_id`), `tags` =
`[leg, target]`, `status: "ok"`, `grade` = the five metrics, `meta`. A leg that reaches no full UI
within the ceiling, exits early, or hits a harness bug goes to `errors.jsonl` (class `ceiling`,
`no_full_ui`, `no_bridge`, `probe`, `harness`) and is never a result row, so it is retried on the
next run and counted beside the report. `traces/<case>_rep<k>.json` hold screen, scrollback, trace
lines and the package table as turns. `_state.json` declares the metrics (headline `full_ui_s`,
lower is better) and `perf_fields`. Rows are appended and fsynced as legs finish; rerunning skips
finished (case, rep) pairs. `work/` holds raw transcripts and stderr per leg.

## Fairness (fixes applied to the harness)

1. **Warm legs share the cold leg's TMPDIR.** Pi 1.4.0 keeps its jiti transform cache in
   `$TMPDIR/jiti` (jiti's `fsCache`: nearest `<dir>/node_modules/.cache/jiti`, else `os.tmpdir()/jiti`;
   `moduleCache` is off) and Node's compile cache under `$TMPDIR/node-compile-cache`. The harness used
   a per-leg `work/<leg>/tmp`, so Pi's warm leg started with an empty jiti cache and measured a second
   cold start (pilot: Pi warm 25.2 s vs cold 26.6 s; fixed: cold 24.2 s, warm 2.2 s). TMPDIR is now
   `<scratch>/<target>_rep<k>/tmp`, created beside the home, reused by the warm launch and wiped with the
   home on the next cold leg. Stale `octet-pi-mirror-*` dirs in it are removed before every launch so a
   warm leg cannot read the cold leg's `bridge.json`.
2. **octet's adapter caches are reset on cold legs.** The host strips the adapter's environment
   (`OCTET_PI_COMPAT_CACHE` never reaches it), so `startup-cache.mjs` puts jiti, the compile cache and the
   persisted mirror cache (`pi-mirror-<slot>.json`, keyed by agent dir, which is identical in every fresh
   HOME) in `<OCTET_EXTENSION_DIR>/.cache`, inside the checkout and outside the leg HOME. A cold leg (and the
   unmeasured priming launch of a warm-only run) deletes `<extensions-dir>/octet-pi-compat/.cache`, and
   the run deletes it again at exit, so nothing leaks between legs, runs, or between a baseline worktree
   and the current tree (each has its own `.cache`).
3. **Network posture matches.** octet legs pass `--offline`; Pi legs set `PI_OFFLINE=1`,
   `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`. A `strace -f -e trace=connect` of Pi's cold and warm start
   shows no socket connect other than AF_UNIX, so Pi's startup does no network I/O here.
4. **Terminal queries do not stall Pi.** Pi sends DA1 (`CSI c`), kitty `CSI ? u` and OSC 10/11 queries; the
   VT answers only DSR. Pi proceeds without answers: its warm start reaches the full UI in about 2 s, so any
   query timeout is below that. octet sends none of them.
5. Both targets get the same environment otherwise (fresh HOME copy, TERM/COLORTERM/LANG, the same mock
   provider, PTY 120x40) and `OCTET_*`/`PI_*` variables from the caller are stripped.
6. A variant run without Pi (`--targets octet`) with `--baseline` computes octet/Pi ratios against the
   baseline's Pi medians, labelled as such in the report.

## Noise guidance

Startup on a shared or loaded machine varies by several seconds. Alternate legs (the harness does),
run at least 5 reps and compare medians (the report prints median±IQR), do not run other builds or
benchmarks at the same time, and treat IQR wider than the delta as no change. Cold octet legs are
dominated by one-time mirror preparation (the `auto-configure.mjs` capture) and Pi's by module
transforms, so cold needs the most reps.

## Known limits

Pi's `typeable_s` is the footer model label, which Pi draws after its own extension load, so Pi
typeable is close to its full UI; octet draws first. The sentinel's position in the extension order
decides how many other `session_start` handlers have finished when it fires. Pi's built-in
extensions (`mcp`, codemode, ...) are not part of the command comparison. Only the interactive TUI
path is covered; RPC/print modes are not.
