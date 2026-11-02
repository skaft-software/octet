# Pi extensions eval

Measures whether octet runs real Pi extensions the way Pi does, in production shape:
the real `octet` binary with the shipped `extensions/octet-pi-compat` bundle in
auto-mirror mode, against real Pi **1.4.0** (`@earendil-works/pi-coding-agent@1.4.0`; 1.5.0 is
out of scope), both on a PTY, both with the same ~23 real npm Pi packages and the same mock
provider. Startup is the first metric; parity metrics ride along.

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
`--scratch DIR`, `--keep`, `--force-fixture`.

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
