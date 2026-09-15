# EXECUTION — openall (extensions/octet-subagents, /subagents open-all) — progress log

Exclusive paths: `extensions/octet-subagents/**`, `docs/subagents.md`, `docs/swarm-audit/EXECUTION-openall.md`.
Base: df5a7e80. HEAD when this worker started: 28c09976 ("vibe: wave 6 checkpoint ... subagents panel").

START 2026-09-15T11:53:48-0400 openall alive
START 2026-09-15T16:28:32Z openall2 alive
START 2026-09-15T16:50:18Z openall3 alive

## 16:50Z — openall3 clean-start survey (IMPORTANT correction to the hand-off)

The hand-off said "predecessor died twice, no partial work to adopt". That is **wrong
about the committed tree**: openall2's work was swept into the parent's checkpoint
commit `28c09976` (authored 12:49 EDT, 20 min after openall2 started at 12:28 EDT).
Committed already, and NOT mine to revert:

- `octet_subagents/launcher.py` — 609 lines: tmux + herdr, fail-closed on a missing
  multiplexer, pane cap, argv-list execution, opaque-ref validation, clean partial failure.
- `octet_subagents/reasoning.py` — 201 lines: mirror of the coding-agent effort policy
  (`clamps_effort_to_model_ceiling` / `supported_levels_gate_on_ceiling`).
- `model.py` — `SpawnRequest.provider/model/reasoning/reasoning_capability`, `Worker`
  detached/reattach fields, `DETACHED_STATE`/`DETACHED_LABEL`.
- `orchestrator.py` — `open_all`, `command` verb routing, detached state constants.
- `presentation.py` — Part 4 landed (already verified: 54 tests OK).

Observed at start: `python3 -m pytest extensions/octet-subagents/tests -q` →
`1 failed, 54 passed` — `test_spawn_schema_policy_allows_whitelisted_mutation_and_rejects_outliers`
asserts `{"model": "other"}` raises `unsupported_model`; under the new policy a
structurally-valid model id is accepted and flagged unapplied instead.

openall3 continues from this committed state (never reverting it) and completes:
TASK 1 tests + wiring, TASK 2 schema/validation surface, TASK 3 reattachment/wording.

## What a running worker's pane can actually launch (evidence, not guess)

`crates/octet-agent/src/delegation.rs:139` `delegated_session_reference()` derives the
only handle the host publishes for a delegated child session: `agent-session:<sha256>` of
`team-<random>/<child>.jsonl`. It is one-way (hash), and `REFERENCE.md` records that the
child JSONL lives under the private `<session-dir>/.delegation/team-*/` directory and is
"not addressable by the session store". So `octet --resume <handle>` cannot open a child
session today; the launcher plans+validates the worker argv and refuses to fabricate a
resume for it (`WORKER_PANE_BLOCKED_REASON`). A host-side resolver that accepts an opaque
`agent-session:` reference is the exact missing primitive, and TASK 3's host half
(agent7: session-owned durable child records + reattachment) is what supplies it.

`herdr`: see the herdr section appended below with the web-search result.

## 17:05Z — verifier finding (root message): permissive model validator — FIXED

Reproduced the exact verifier case before fixing:

```
$ python3 -m pytest extensions/octet-subagents/tests -q | tail -3
SUBFAILED(arguments={'name': 'worker', 'task': 'x', 'model': 'other'}) ...AssertionError: SubagentError not raised
1 failed, 54 passed, 30 subtests passed
```

Root cause: openall2 replaced the base contract (`model != "inherit"` ->
`unsupported_model`) with a purely structural regex, so any well-formed but
unverifiable model id was accepted. That is the fail-closed bug.

Fix (model.py `SpawnRequest.parse(..., known_models=...)`):
- `provider`/`model` are accepted only when the calling session can CONFIRM them.
  The only confirmable model this process can observe is the parent session's own
  model (`owner.inherited_model`); `Orchestrator.spawn` passes it via
  `known_models_for(owner)`. Anything else raises `SubagentError(code="unsupported_model")`
  naming the missing primitive (API 0.2 exposes no provider catalog and carries no
  per-child model field). Nothing is coerced to `inherit`.
- `provider` must be present with `model`, and must match the confirmed model's
  provider segment, else `unsupported_model`.
- `reasoning` unknown level -> `SubagentError(code="unsupported_reasoning")`;
  a known level outside the advertised ceiling is clamped by the mirrored policy
  (`reasoning.py`) with an explicit note, never silently dropped.
- Default path unchanged: no provider/model/reasoning supplied -> all `inherit`,
  child copies the parent session exactly.

Observed after the fix:

```
$ python3 -m pytest extensions/octet-subagents/tests -q
57 passed, 47 subtests passed in 0.47s
```

## 17:20Z — open-all tests (TASK 1) + herdr verification

New `extensions/octet-subagents/tests/test_launcher.py` (14 tests, 14 subtests). The
launcher resolves the multiplexer/octet binary with `shutil.which` on the *process*
PATH, so the tests patch `os.environ` and install stub `tmux`/`herdr`/`octet`
executables in a temp dir (plus a real-tmux test guarded on availability).

Exact argv constructed for `open-all tmux` with `parent_session_id=parent-session`,
`workspace=/workspace`, not already inside tmux, `octet` on PATH, one running worker:

```
parent: ['tmux','new-session','-d','-s','octet-fleet-parent-session','-n','parent',
         '-c','/workspace','--','<octet>','--resume','parent-session']
worker: ['tmux','new-window','-d','-t','octet-fleet-parent-session','-n','explore-auth',
         '-c','/workspace','--','<octet>','--resume','agent-session:<sha256>']
```
Inside an existing tmux (`$TMUX` set) both panes use `['tmux','new-window','-d',...]`
and no session is created. herdr:
```
['herdr','pane','split','--current','--direction','down','--no-focus']  # -> .result.pane.pane_id
['herdr','pane','run','%7','<octet> --resume <handle>']                 # single command string
```
Note the fix from `--direction down` to `--current --direction down`: herdr's own skill
file says an omitted target "may use the UI-focused pane, which can belong to the user
or another client", so open-all now targets the caller's pane explicitly.

### herdr — real? verified (no guessing)

- GitHub API `search/repositories?q=herdr` -> `herdrdev/herdr`, 38692 stars,
  "the runtime your coding agents live on"; siblings `herdr-reviewr`, `herdrm`,
  `herdr-remote`.
- crates.io: crate `herdr` "terminal workspace manager for AI coding agents",
  homepage https://herdr.dev, repo https://github.com/ogulcancelik/herdr.
- npm: reserved package `herdr` (AGPL-3.0), "terminal workspace manager for AI coding agents".
- https://herdr.dev/docs (Starlight) documents a CLI + local socket API.
- https://raw.githubusercontent.com/herdrdev/herdr/master/skills/herdr/SKILL.md:
  `herdr pane split --current --direction right --cwd "$PWD" --no-focus` then
  `.result.pane.pane_id`; `herdr pane run <pane-id> "<command>"` "atomically sends
  command text and Enter"; `herdr pane wait-output`, `herdr pane read`.
- https://herdr.dev/agent-guide.md: `HERDR_ENV=1` marks a Herdr-managed pane and
  "Herdr blocks nested launches by design" — the ownership guardrail the launcher enforces.
Still UNVERIFIED (honest gaps): no `herdr` binary on this host, so no live herdr pane
was ever created; whether `pane split` emits JSON without an explicit `--json` flag is
read from the skill's `.result.pane.pane_id` examples, not observed; and there is no
per-worker model/provider field anywhere in herdr's documented pane API.
