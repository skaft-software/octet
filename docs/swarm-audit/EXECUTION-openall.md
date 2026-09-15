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
START 2026-09-15T17:13:23Z openall4 alive

## 17:13Z — openall4: adopted openall3's committed work (no restart) and hardened it

State found at start: `python3 -m pytest extensions/octet-subagents/tests -q` ->
**76 passed, 61 subtests passed** at HEAD `9c43111d`. openall2/openall3's work is
committed there, so TASK 2 was ALREADY FIXED before this worker started; nothing
was reverted. TASK 2's verifier case does not reproduce:

```
$ python3 -m pytest extensions/octet-subagents/tests/test_orchestrator.py -q -k policy
6 passed, 20 deselected, 26 subtests passed in 0.02s
$ python3 - <<'PY'   # independent probe, not the test
from octet_subagents.model import SpawnRequest, SubagentError
SpawnRequest.parse({"name":"worker","task":"x","model":"other"})
PY
refused {'name': 'worker', 'task': 'x', 'model': 'other'} ==
  code=unsupported_model ... "not a model this session can confirm as configured"
refused {'provider': 'anthropic'}          -> code=unsupported_model
refused {'reasoning': 'turbo'}             -> code=unsupported_reasoning
accepted {}                                -> provider/model/reasoning all `inherit`
accepted {provider+model+reasoning: max}   -> effective=high
  note="requested max clamped to high by the model ceiling high"
```

Panel surfacing observed (`reasoning_capability.ceiling=medium`, request `max`):

```
row:    model_policy="claude-sonnet-test" model_policy_applied=true
        reasoning_policy="max" reasoning="inherited"
        reasoning_note="requested max clamped to medium by the model ceiling medium"
detail: "Orchestration selection: provider/model inherit/claude-sonnet-test,
         reasoning max (applied by the host; effective claude-sonnet-test /
         reasoning inherited). requested max clamped to medium by the model ceiling medium"
```

The `reasoning.py` mirror was re-checked against the Rust source it claims to
mirror: `app/mod.rs:350 supported_levels_for_model` (Ultra filtered unless
`model_supports_ultra`), `:369 supported_levels_with_subagents` (Ultra filtered
unless `subagents_available`), `:60 thinking_to_reasoning` (clamp down to the
highest supported effort <= request, else lowest non-Off), `:820/:841` the two
named tests. No second policy was invented.

### TASK 1 — what a worker pane can actually launch (verdict: still blocked, now proven)

The predecessor's "no resolver exists" claim was WRONG in one detail, so the
operator-facing refusal was rewritten with the real evidence:

- `crates/octet-coding-agent/src/session_store.rs:2255 path_by_id` resolves an id
  only as `<session-dir>/<id>.jsonl` (`session_file_exists` -> `session_id_is_valid`
  at `:767`), and a delegated child transcript lives under the owner-private
  `.delegation/team-*/` directory -> `octet --resume agent-session:<sha256>`
  cannot open it.
- A resolver for the opaque reference DOES exist
  (`crates/octet-agent/src/delegation.rs:555 open_session_reference`, wired at
  `agent.rs:5513 Agent::open_delegated_session_reference` and `agent.rs:782
  Run::open_delegated_session_reference`), and the serve layer accepts
  `agent-session:` ids (`extensions/serve.rs:3025 open_session`,
  `:1611 driver_for_delegated_session`) — but it returns a **read-only, externally
  locked inspection** session (`AuthorityProfile::ReadOnly`,
  `SessionLiveState::Locked`, `ActorOwnerState::ExternallyLocked`), reachable
  inside the owning process, not a launchable interactive session.
- So the missing primitive is a **launchable handle** for a session-owned
  delegated child, not a resolver. `WORKER_PANE_BLOCKED_REASON` now says exactly
  that, naming the two crates/functions above, and
  `docs/subagents.md`, `REFERENCE.md`, `README.md`, `CHANGELOG.md` were corrected
  to match. No crates/** file was edited.

### TASK 1/4 — open-all now composes with reattachment (new behaviour + tests)

Observed gap before the change: with two session-owned detached workers, the
command-surface `/subagents open-all tmux` rendered

```
open-all tmux: 1 pane(s) created, 0 blocked, clean
- opened parent pane (parent)
```

— the detached workers VANISHED from the report (`skipped` key did not exist).
That is the silent omission TASK 4 forbids. Now `Orchestrator.open_all` returns
`skipped` rows and names each one, and never opens a stale pane for it:

```
open-all tmux: 1 pane(s) created, 0 blocked, 2 not opened, clean
- opened parent pane (parent)
- not opened detached worker (live-worker): still owned by this session but detached
  from any host run, so its live session is not addressable and no stale pane is
  opened for it. Reattach it with /subagents wait or subagent_status, then re-run
  open-all; a reattached worker is planned again.
- not opened detached worker (gone-worker): ...
skipped = [("live-worker","orphaned",true),("gone-worker","orphaned",true)]
notifications = ["Subagent open-all left session-owned workers unopened"]
executed calls = [["new-session","-d","-s","octet-fleet-parent-session","-n","parent",
                   "-c","/workspace","--","<octet>","--resume","parent-session"]]
```
After the host republished both records, the same command planned
`["parent","live-worker","gone-worker"]` with `skipped == []` — reattachment
restores the pane plan, so open-all never depends on a stale handle. A worker
parked at the approval boundary is likewise not opened and says
"unattended mutation".

### TASK 1 — exact argv (observed, stub PATH; unchanged from openall3 except herdr direction)

```
parent (not inside tmux):
["tmux","new-session","-d","-s","octet-fleet-parent-session","-n","parent",
 "-c","/workspace","--","/…/octet","--resume","parent-session"]
worker (planned + validated, not launched):
["tmux","new-window","-d","-t","octet-fleet-parent-session","-n","live",
 "-c","/workspace","--","/…/octet","--resume","agent-session:<sha256>"]
herdr split: ["herdr","pane","split","--current","--direction","right","--no-focus"]
herdr run:   ["herdr","pane","run","%7","<octet> --resume <handle>"]
```
The herdr split now uses `--direction right`, the only direction value present in
the herdr evidence fetched last wave (an unobserved enum value is never passed);
the previous `down` was undocumented, so code and its own docstring disagreed.

Shell-safety probe (all refusals observed, nothing executed):

```
REFUSED parent_session_id="parent;rm -rf /"  -> code=unlaunchable_session
REFUSED parent_session_id="parent$(id)"      -> code=unlaunchable_session
REFUSED worker name "evil;rm -rf /"          -> code=invalid_label
REFUSED worker handle "agent-session:x;id"   -> code=unlaunchable_session
REFUSED relative workspace                   -> code=invalid_workspace
REFUSED 9 running workers + parent           -> code=pane_cap (cap 9, nothing opened)
calls = []
```
Command-surface refusal (no multiplexer on PATH -> `multiplexer_missing`, "octet
never downloads or installs a multiplexer"; `screen` -> `unsupported_multiplexer`)
creates nothing: `calls == []`.

### Tests and verification (observed)

```
$ python3 -m pytest extensions/octet-subagents/tests -q
80 passed, 61 subtests passed in 1.36s        # was 76; +4 new tests
$ python3 -m unittest discover -s extensions/octet-subagents/tests -t extensions/octet-subagents/tests
Ran 80 tests ... OK
```
New/updated tests: `test_session_owned_workers_that_are_not_opened_are_named_with_a_reason`
(+ accurate blocked-reason assertions), `test_open_all_command_opens_the_parent_and_names_detached_workers`
(exact argv, exactly one real call, `skipped` rows, reattach-restores-pane-plan,
no secret in the report), `test_open_all_command_never_opens_a_worker_parked_at_the_approval_boundary`,
`test_open_all_command_refuses_a_missing_multiplexer_and_creates_nothing`, and a
positive default-inherit assertion next to the `{"model": "other"}` rejection.

Honest gaps: still no `herdr` binary on this host, so no live herdr pane was
created; whether `pane split` returns JSON without an explicit `--json` flag is
read from herdr's own skill text, not observed; and no worker pane was ever
launched live — `octet --resume agent-session:<sha256>` is impossible until the
host publishes a launchable child handle.
