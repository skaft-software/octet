# Common brief: octet v0.10.0 Pi compatibility, usability and performance

Read this whole file before starting.

## Goal

Make octet's Pi compatibility usable for any Pi user, with no regressions and no slop.
Measure against Pi 1.4.0. Prove every claim with a reproduction on the real fixture, not
just a unit test.

## Repo and branches

- Repo: `/home/user/octet`. The release branch is `release/v0.10.0-candidate` at `852ca760`.
  The coordinator's branch `claude/admiring-feynman-2sqpfb` is identical to it.
- You are in your own git worktree. First run
  `git checkout -B <your-branch> 852ca760` (your branch name is in your task).
- Commit locally, early and often: after every passing step. End each message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Do NOT push. Uncommitted work
  is lost if you are stopped by a usage limit.
- Read first: `docs/maintainers/pi-extensions-eval-handoff.md`,
  `docs/maintainers/pi-v0.10-perf-handoff.md`, `docs/design/pi-native-esm.md`,
  `docs/design/pi-single-import.md` and `extensions/octet-pi-compat/README.md`.

## The maintainer's bug reports (manual qualification on macOS, their real Pi 1.4.x setup)

Screenshots: `/tmp/claude-0/-home-user-octet/cd824a8e-e848-5e07-a6dc-ce8284284071/images/1.webp`,
`2.webp` and `3.png`. Open them with the Read tool.

1. **Footer swap jank.** octet first draws its own native footer, then the Pi footer
   replaces it seconds later, once the extensions load. With Pi compat enabled, the Pi
   footer must appear immediately, with no visible swap.
2. **Footer inconsistent across sessions.** In one session (cwd `~/github/skaft-software/octet`)
   the Pi footer appeared after a long delay. In another (cwd `~`, the home directory) it
   never appeared, and the native footer `GPT-6.1-Sol · max · 5%/272K   ~` stayed.
   Screenshot 2 shows real Pi on the left (footer
   `~   GPT-5.6 Luna 0/272K (0%) | $0.000 (sub) | 1m 22...`) and octet on the right.
3. **Footer fidelity is poor.** When the Pi footer does appear in octet (screenshot 1) it
   looks wrong: a separate `pi-lens` status line, the path on its own line, and
   `GPT-5.6-Luna | 1m 22...` on the right. The model ID is shown instead of the display
   name, and the context-usage (`0/272K (0%)`) and cost (`$0.000 (sub)`) segments are
   missing. It must look exactly like Pi.
4. **Prompt composition and `before_agent_start` failing.** Extension prompt composition
   fails and the before-prompt hook fails. The exact error text is unknown, so reproduce it.
   Relevant code: `extensions/octet-pi-compat/lib/before-agent-start.mjs` and
   `lib/handler-budget.mjs`, which set per-handler 2 s and total 4 s budgets, plus the
   host's 5 s before_prompt deadline. Check what Pi 1.4.0 actually does.

Screenshot 3 also shows `extension action queued for the next idle boundary; approval and
eligibility are checked there` and a `Controls · queued / command · idle` block after a
`/reload` issued while a turn was running.

## Real fixture (use it; don't mock what you can run)

- The cached fixture for Pi 1.4.0 plus 23 real npm packages, including
  pi-powerline-footer, pi-lens and pi-hermes-memory, is at
  `/tmp/claude-0/-home-user-octet/cd824a8e-e848-5e07-a6dc-ce8284284071/scratchpad/pi-eval-cache/`.
  Build or locate it with `scripts/pi-eval/fixture.py` (`fx.build_fixture(cache)` returns
  `.home`, `.pi_cli`, etc.).
- The eval harness is `scripts/pi-eval/run.py`, with helpers `launch`, `make_env`,
  `prepare_home`, `EvalSession` (a PTY session with stderr written to a file) and `leg_tmp`.
  It uses a mock OpenAI-compatible provider from `scripts/e2e/octet_e2e/provider.py`.
- Repro starting point:
  `/tmp/claude-0/-home-user-octet/cd824a8e-e848-5e07-a6dc-ce8284284071/scratchpad/repro/repro.py`.
  It is untested. Usage: `python3 repro.py <octet|pi> <workspace|home> <outdir>`. It
  samples footer states over time, sends one prompt, and saves the screens. Copy it into
  your own outdir and adapt it.
- The octet release binary is `/home/user/octet/target/release/octet`. If you change Rust,
  build your own (`cargo build --release -p octet-coding-agent`, about 15 min on 4 cores)
  and use `--octet <your binary>` and `--extensions-dir <worktree>/extensions`.
- Running the Pi CLI and the fixture packages is allowed here: this is a disposable sandbox.

## Machine discipline (4 cores, shared with other workers)

- Every command that launches octet or Pi on the fixture, runs a cargo build or test, or
  runs the full adapter suite must be wrapped in
  `flock -s /tmp/claude-0/-home-user-octet/cd824a8e-e848-5e07-a6dc-ce8284284071/scratchpad/measure.lock <cmd>`.
- Only timing measurements use the exclusive form, `flock -x`. That keeps other workers'
  runs from distorting timings.
- Use isolated HOME and cache directories for adapter tests:
  `HOME=$(mktemp -d) OCTET_PI_COMPAT_CACHE=$(mktemp -d)`.

## Rules (non-negotiable)

- **Lean core.** Rust stays Pi-agnostic. Pi-specific behaviour lives in
  `extensions/octet-pi-compat`. Touch the host only for a generic primitive, and name it as such.
- **No weakening.** Do not weaken trust, review, source integrity (including the per-import
  recheck from `852ca760`), owner or generation fences, credential boundaries, process
  cleanup, or test deadlines. Never skip hooks to look faster.
- **Pi is the reference.** When behaviour is in doubt, read Pi 1.4.0's installed source
  (read-only) under the fixture's `.pi/agent/install/releases/1.4.0/node_modules/@earendil-works/`
  and match it.
- **No slop.**
  - Smallest correct diff.
  - No dead code, speculative abstractions, debug leftovers or commented-out code.
  - Comments only where they explain *why*.
  - Match the surrounding style.
  - Every behaviour change gets a regression test that fails before the change and passes after.
- **Adapter tests.** Run the focused tests you touch, then the full adapter suite. The known
  pre-existing failures differ by environment (60 to 118); compare failing-name lists before
  and after your change, and they must be identical. A baseline list is at
  `/tmp/claude-0/-home-user-octet/cd824a8e-e848-5e07-a6dc-ce8284284071/scratchpad/after.sorted`,
  but regenerate it on `852ca760` in your own environment.
- If the sandbox denies an action, stop that line of work and report it. Do not work around it.

## Report back

- Branch, commits, and root causes with file:line references.
- The fix and why the guarantees still hold.
- Before/after evidence from the real fixture: screens, timings, and the failing-name comparison.
- Remaining risks, and anything you could not verify.

## PRIMARY fixture: the maintainer's real Pi setup (added 2026-11-03)

- **Location:** `/tmp/claude-0/-home-user-octet/cd824a8e-e848-5e07-a6dc-ce8284284071/scratchpad/pi-eval-cache/03dc02b6ce9f3a89`.
  Its home is in `home/`. It is built by `scratchpad/maint/build_maintainer_fixture.py`; load it with
  `fx.Fixture(root=Path(that), lock=json.load(...))`, or point `HOME` at a copy of `home/`. Always use a
  copy (`fx.copy_tree`); never mutate the cached template.
- **Packages:** the same 29 packages and versions as the maintainer, with Pi 1.4.0.
  - 13 autoload: pi-agent-extensions, pi-mcp-adapter, pi-web-access, pi-subagents,
    pi-background-tasks, pi-lens, pi-permission-system, pi-hermes-memory,
    rpiv-ask-user-question, pi-hashline-edit-pro, bigpowers, awesome-pi-themes, and
    @termdraw/pi.
  - 16 installed with `autoload: false`. octet must NOT load these, exactly as Pi does not;
    verify it.
- **Settings:** the same Pi settings as the maintainer:
  - `theme: ghostty-dark` (from awesome-pi-themes);
  - `tuiMode: fullscreen`;
  - `defaultTools: ["+codemode"]` with `codemode.mode: on`;
  - `steeringMode: all`;
  - `defaultThinkingLevel: max`;
  - `terminal.showTerminalProgress`;
  - `markdown.mermaid: streaming`;
  - `compaction.enabled`.

  octet must honour every one the way Pi does. Report any it ignores.
- **Footer:** there is NO pi-powerline-footer here. The maintainer's footer comes from one
  of these packages or from Pi's built-in footer. Find out which.
- **Provider:** the maintainer uses `openai-codex`; the fixture uses the mock provider. To
  catch the model-ID vs display-name bug, configure the mock model with id `gpt-5.6-luna`
  and name `GPT-5.6 Luna`, a context window of 272000 and a subscription-style zero cost,
  in BOTH Pi (`models.json`) and octet (`custom.json`).
- **Usage:** this fixture is primary for every acceptance check. The 23-package fixture is
  secondary, for breadth.

## Diagnostics presentation (maintainer requirement, added 2026-11-03)

Extension warnings, errors and notices must be polished, concise and grouped/deduplicated, and
must never dump logs, stack traces, absolute paths or extension console output into the chat.
Worker `fix/pi-diagnostics-ux` owns the presentation layer: lib/issues.mjs, the notification and
diagnostic paths, and the host rendering of extension diagnostics. Other workers must not add
new user-facing messages ad hoc. Route them through the existing issue/diagnostic paths with a
short label and a one-line action, and put detail in the payload.

## DISK DISCIPLINE (mandatory; the disk filled up once already)

The session has a fixed disk allowance (about 7 GB free after cleanup) shared by seven workers.

1. **Before every fixture copy or cargo build, check free space** (`df -h /`). If less than
   3 GB is free, clean up your own leftovers first; if that isn't enough, wait (re-check
   every few minutes) rather than fail.
2. **Fixture homes.** Never full-copy more than one at a time. Copy the read-only trees as
   hardlinks: `cp -al <fixture>/home/.pi/agent/install` and `.../npm`. Nothing writes into
   them at runtime; verify with a checksum before and after your first run. Full-copy only
   the small rest of the home (settings, sessions, caches).
3. **Delete each leg's home and outputs as soon as you have extracted what you need.** Keep
   screens, logs and traces (small) and delete the copied homes.
4. **Cargo.** Build only when your change touches Rust. Set
   `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` for test builds.
   Delete your worktree's `target/` when you no longer need it (keep only a release binary
   you still use, copied out to `scratchpad/bin-<branch>/octet`).
5. **Never delete another worker's directories.** Ask the coordinator instead.
