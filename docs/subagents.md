# Subagents

octet's `octet-subagents` extension delegates a bounded task to a named
background worker while the parent session keeps working. octet owns the child
conversations, permissions, persistence, limits, and shutdown; the extension owns
only decomposition and completion policy. It is not an agent team, swarm, graph
runtime, or a second model loop.

Bundle documentation: [extension README](../extensions/octet-subagents/README.md)
· [runtime reference](../extensions/octet-subagents/REFERENCE.md).

## What a worker is

- **Depth one.** A worker cannot spawn another worker; a descendant observed at
  depth two is interrupted immediately.
- **Bounded.** At most eight active children and thirty-two retained workers per
  parent owner. Wall-time, turn, cost, and output ceilings are optional per spawn
  and default to the parent session's settings (an unlimited parent stays
  unlimited).
- **Scoped.** The default tool grant is the parent's full standard scope
  (`read`, `search`, `edit`, `write`, `bash`); `tools: ["read", "search"]`
  narrows a worker to hard read-only. Workers inherit cwd, environment, sandbox,
  approval policy, and extension policy — a shared filesystem is **not**
  isolation.
- **Owned by the session, run by a host run.** The host creates the child
  conversation and detaches, rather than discards, its record when the owning
  run ends. See
  [session-scoped delegation](#session-scoped-delegation) for what that means
  when a worker outlives the turn that spawned it.

## Per-worker provider, model, and reasoning

`subagent_spawn` accepts optional `provider`, `model`, and `reasoning` identifiers.
Omitted values (or `inherit`) inherit the parent's selection. Explicit selections
require negotiated `agent_model_selection_v1`; older hosts fail closed before
creating a worker. The host resolves configured, credential-available routes and
never substitutes the parent model for an unknown or unavailable route.

Use `subagent_models` first to discover exact identifiers and supported reasoning:

```json
{"query": "haiku", "limit": 10}
```

`query` is optional plain text (at most 128 UTF-8 bytes); `limit` defaults to 50
and is bounded to 1–100. Results contain `models` and `truncated`; rows expose
provider/model identifiers, display name, reasoning levels, context window and
maximum output tokens, never credentials. Narrow the query when truncated.
Discovery is owner-bound and read-only; it does not authenticate or start workers.

Reasoning identifiers are `inherit`, `off`, `on`, `minimal`, `low`, `medium`,
`high`, `xhigh`, `max`, and `ultra`; use the choices returned for the target model.
`on` supports binary/always-on models; the host remains authoritative.

Supply an explicit model with an explicit provider. Unknown routes and unsupported
reasoning fail with `unsupported_model` / `unsupported_reasoning`. The host alone
normalizes reasoning against configured model metadata. The legacy
`reasoning_capability` input is only a compatibility hint and cannot affect
execution. Requested and host-confirmed effective selections stay separate in
the inspector, survive restoration, and are preserved by continuation. Host
`policy.resolved_model` carries effective provider/model and serialized
`ReasoningConfig`; the extension does not guess an effective route or clamp effort.

## Drive the fleet

| Command | Use |
| --- | --- |
| `/subagents` | Host-owned worker list; Up/Down selects, Enter opens a read-only transcript. |
| `/subagents inspect <name-or-id>` | Cached detail for one worker. |
| `/subagents wait <name-or-id>` | Explicit parent wait; also the owner-bound reattach pass. |
| `/subagents reattach <name-or-id>` | Alias for the reattachment pass. |
| `/subagents stop <name-or-id\|all>` | Owner-bound interruption. |
| `/subagents open-all tmux\|herdr` | Reopen the parent and every running worker as interactive sessions, one pane each. |

The model-facing equivalents are `subagent_models`, `subagent_spawn`, `subagent_status`,
`subagent_wait`, `subagent_stop`, and `subagent_continue`.

## Open the fleet in panes

`/subagents open-all tmux` (or `herdr`) is the escape hatch: it plucks the parent
session and every **running** worker out of the read-only parent-controlled panel
and reopens each as its own interactive octet session, one pane or window per
session. That is the doorway to orchestrating a worker — and a multimodel fleet —
independently of the parent.

- **Fail closed when the multiplexer is absent.** `tmux`/`herdr` (and the `octet`
  binary) are found with a read-only `PATH` lookup; if one is missing, the command
  refuses with an actionable message. octet never downloads or installs a
  multiplexer.
- **Host-launchable workers only.** A pane is planned for the parent plus each
  worker the host reports as launchable (`launchable` / `launch_blocked` on every
  `agent/list` row: settled, detached, or reattached work). A worker that is still
  live in the owning process, parked at the host approval boundary, or whose
  transcript is gone is not launchable — one session has one writer, and opening a
  parked worker would be unattended mutation — so it gets no pane, and is **named
  in the report** with the approve/reattach step: a detached worker is alive and
  reattachable, so dropping its row silently would be wrong, and opening a stale
  pane for it would target the wrong session.
- **Bounded.** At most nine panes (the eight-worker fleet cap plus the parent);
  above that the whole request is refused before anything is created.
- **Shell-safe.** Every session id, path, and flag is a separate `argv` element,
  the multiplexer is always invoked with an argv list and never through a shell,
  and a session id or worker handle carrying a shell metacharacter is rejected
  before a command line exists.
- **No secrets.** Only the opaque, path-free `agent-session:<sha256>` reference
  and the host session id ever reach a command line or a notice. Credentials,
  tokens, and transcript paths are never read, printed, or passed.
- **Clean failure.** Panes are created one at a time; the first failure stops the
  run, reports exactly what exists, destroys nothing, and is safe to re-run.
- **Ownership.** herdr's documented agent guardrail requires `HERDR_ENV=1`, so
  open-all refuses to drive a herdr session it does not own.

The parent pane resumes the host session id directly. A worker pane resumes the
opaque, path-free `agent-session:<sha256>` handle the host publishes on each
`agent/list` row (`octet_agent::delegated_session_reference`), and
`octet --resume agent-session:<sha256>` resolves it end to end:

- `SessionStore::path_by_id` recognizes the handle
  (`crates/octet-coding-agent/src/session_store.rs`) and resolves it through
  `octet_agent::delegation::resolve_launchable_child_session(session_directory,
  reference)` — no live agent, no credential, no network. The resolved host-only
  transcript path is handed to the launcher, so the pane opens **that child's own
  session**: its own history is replayed and appended, never the parent's and never
  a stale transcript.
- The `agent-session:` prefix is reserved in the `--resume` namespace. A handle is
  either a strict `agent-session:` + 64 lowercase hex token that resolves to a
  launchable child, or a refusal; it is never reinterpreted as an ordinary session
  id, and an unlaunchable handle never falls back to another session.
- The token is validated **before any filesystem work**, so a shell
  metacharacter, a control byte, or a path component (`..`, `/`) can never reach a
  path join. The resolved path is then confined to the session store's private
  `.delegation/team-*/` directory, so a forged or copied roster entry that hashes
  to the same handle cannot escape it.
- Every refusal is typed (`DelegatedHandleRefusal`), bounded, actionable, and
  free of credentials, paths, and session secrets:
  `malformed_worker_handle`, `delegation_roster_unavailable`, `unknown_worker_handle`,
  `worker_awaiting_approval`, `worker_live_in_owning_process`,
  `worker_transcript_missing`, `worker_handle_outside_delegation`. Nothing panics,
  and nothing silently opens a different session. A worker parked at the approval
  boundary is specifically not openable for unattended mutation.
- Ordinary `--resume <session-id>` and the session picker are unchanged: an
  ordinary id keeps exactly its previous `<session-dir>/<id>.jsonl` resolution, and
  the picker remains a flat, non-recursive view that never offers a delegated
  child.

The extension half of the pane plan still needs its one-line policy flip, and it
lives in the `octet-subagents` bundle (recorded here because that bundle is
versioned separately): `launcher.py::resolve_worker_pane` currently hardcodes
`resolvable=False` with `WORKER_PANE_BLOCKED_REASON`, and `plan_open_all` plans
only `active` workers. With the handle resolved, it should instead read the host's
`launchable` / `launch_blocked` from the `agent/list` row (today the extension's
`Worker` model drops both), set `resolvable=True` for a launchable worker, carry
`launch_blocked` as the bounded blocked reason otherwise, and plan a pane for every
host-launchable worker rather than only for a running one. Until that flip lands,
the worker pane is reported **blocked**, naming that exact reason, rather than
fabricating a resume. The normal read-only parent-controlled mode is unaffected.

## Session-scoped delegation

A worker is not a detached OS process. Its record is owned by the **session**,
not by the run that spawned it: the end of the owning run (including an aborted
or dropped turn) records an explicit `run_detached` boundary and leaves the
worker discoverable. Owner teardown parks the worker for reattachment while
retaining any settled status, output/error, and completion time; explicit stop
or team shutdown still stops execution. Each root has a separate durable roster
(`fleet-<owner hash>.json`) beside its root-scoped lease in the delegation
session directory. It carries each worker's id, name, task, child-session
reference, status, and consumed budget, so the owning session can reattach it on
a later turn and a restarted process can reconstruct it without losing it
silently. Matching legacy `fleet.json` snapshots are read-only migration sources;
new scoped snapshots take precedence. An observer denied the fleet lease can
inspect but cannot admit mutations or start a worker. On later takeover it
reloads the current authoritative roster under its new claim before any
admission or roster write; an absent or stale claim refuses new work rather
than acknowledging a worker without a durable record. Execution caps do not
drift up across that boundary: reattachment takes a slot per runnable record
and leaves the excess visibly detached. Records with no undelivered task settle
without reserving execution slots, so they cannot starve runnable siblings.
A follow-up to a recovered, settled worker needs a free slot before acceptance;
if capacity is full it is rejected without queuing or extending its deadline.

The restart roster remains bounded to 256 KiB. When completed/limit-reached
output would exceed that budget, the roster retains explicitly marked output
prefixes; complete committed responses remain in the referenced child sessions.
Approval reasons and failure diagnostics are not shortened by this output budget,
and oversized metadata still fails closed.

Initial tasks and accepted follow-ups persist their payload, random delivery
identity, and failed-delivery count before acknowledgement. Accepted direct
steering also keeps its payload and delivery identity in the durable FIFO until
the child session confirms delivery, including while a channel notification or
an uncommitted prompt holds a process-local attempt. Restart reconciliation
uses delivery identities on the child session's active ancestry, not matching
text: identical requests remain distinct work. Explicit resume drains older
accepted work before the new follow-up; process-local commands only wake that
durable queue. Undelivered startup or prompt failures are retained for explicit
retry and dead-lettered after three failed attempts, with durable diagnostics.
Unreadable child-session authority retains accepted payloads and retry counts
without executing them. After repair, both automatic reattachment and explicit
resume reconcile delivery identities before replaying only undelivered inputs.
If reattachment finds no undelivered task, the worker settles as `interrupted`
with an explicit continuation hint rather than waiting forever as `pending`.
Its session, usage, and buffered messages remain available to `subagent_continue`.
A delivered prompt or checkpoint alone is not proof of successful completion;
reattachment neither invents success nor automatically replays delivered work.
Worker panics are supervised and wake parent waiters; neither an old supervisor
nor an old worker's cleanup can settle a newer worker incarnation. The standard
release build retains panic unwinding so this isolation also works in installed
binaries; an embedder choosing `panic = "abort"` instead terminates the process.

The extension models the gap as **detached, not dead**:

- A worker whose host record is no longer reported becomes `detached` (the wire
  state stays `orphaned`) instead of a terminal row: `detached` is true,
  `reattachable` reports whether its durable session reference was already
  observed, `detached_at_ms` records when detachment was first seen, and its
  summaries, errors, usage, and the complete sibling roster are retained.
- When the owning session republishes the live record, the extension **reattaches**
  automatically: `detached` clears, `reattach_count` increments,
  `last_reattached_at_ms` is recorded, and the bounded detachment note is cleared
  (a real host error is preserved). `/subagents wait <name-or-id>` forces that
  pass from the command surface.
- A targeted wait result carries
  `reattachment: {state: "detached"|"reattached"}` so the caller always knows
  which one happened. The cached/narrow fallback states plainly that it performed
  no wait — never a silent stall and never a fake success.
- If the host parks a detached worker at the approval boundary, the extension
  renders the bounded `awaiting approval` state. `subagent_continue` refuses it
  with `worker_awaiting_approval`, because queueing work into a parked worker
  would be unattended mutation; `subagent_stop` still stops it explicitly.
- `open-all` composes with the same contract: only workers whose host row is
  `launchable` are planned, each through the host's opaque `agent-session:<sha256>`
  handle, so a pane never targets a stale session and `octet --resume` refuses the
  same rows the host does. A detached or parked worker is excluded **and named** in
  the report with its reattach/approve step, and a worker reattached by
  `/subagents wait` is planned again on the next run.

## Where the rest lives

- [Extension README](../extensions/octet-subagents/README.md): install, enable,
  and the quick start.
- [Runtime reference](../extensions/octet-subagents/REFERENCE.md): exact tool
  contracts, safety model, states, presentation, and release smoke recipe.
- [Sessions](sessions.md) · [Models, providers and reasoning](providers.md) ·
  [tmux setup](tmux.md).
