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
  conversation and retires its record when the owning run ends. See
  [session-scoped delegation](#session-scoped-delegation) for what that means
  when a worker outlives the turn that spawned it.

## Per-worker provider, model, and reasoning

`subagent_spawn` accepts `provider`, `model`, and `reasoning` per worker. All
three default to `inherit`, which copies the parent session's already-normalized
selection exactly and is the recommended default.

```json
{
  "name": "cheap-reader",
  "task": "List every caller of the auth helper.",
  "profile": "explore",
  "provider": "inherit",
  "model": "inherit",
  "reasoning": "inherit",
  "tools": ["read", "search"]
}
```

The selection is validated fail-closed, never silently coerced:

- a `model` this session **cannot confirm as configured** is refused with the
  typed `unsupported_model` error (API `0.2` exposes no provider catalog and the
  host reports exactly one model to the extension — the parent session's);
- a `provider` supplied without a matching `model`, or a malformed id, is refused
  with `unsupported_model`;
- an unknown `reasoning` level is refused with `unsupported_reasoning`;
- a level above the target model's ceiling is **clamped** by the same ladder the
  coding agent uses (`crates/octet-coding-agent/src/app/mod.rs`), with an explicit
  note. The extension mirrors that policy; it does not invent a second one.

The panel and inspector show both the requested and the effective selection, and
mark a request the host has not confirmed rather than implying it took effect.

## Drive the fleet

| Command | Use |
| --- | --- |
| `/subagents` | Host-owned worker list; Up/Down selects, Enter opens a read-only transcript. |
| `/subagents inspect <name-or-id>` | Cached detail for one worker. |
| `/subagents wait <name-or-id>` | Explicit parent wait; also the owner-bound reattach pass. |
| `/subagents reattach <name-or-id>` | Alias for the reattachment pass. |
| `/subagents stop <name-or-id\|all>` | Owner-bound interruption. |
| `/subagents open-all tmux\|herdr` | Reopen the parent and every running worker as interactive sessions, one pane each. |

The model-facing equivalents are `subagent_spawn`, `subagent_status`,
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
- **Running workers only.** `done`/`failed`/`limit_reached`/`stopped`/`timed_out`
  workers get no pane; the parent always gets one. A worker that is still owned
  by the session but not attached to any run — or parked at the host approval
  boundary — gets no pane either, and is **named in the report** with the
  reattach/approve step: a detached worker is alive and reattachable, so dropping
  its row silently would be wrong, and opening a stale pane for it would target
  the wrong session.
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

The parent pane resumes the host session id directly. A worker's only
host-published handle is the opaque, one-way `agent-session:<sha256>` reference,
which names a transcript inside the owner-private delegation directory. The
session store resolves an id only as `<session-dir>/<id>.jsonl`
(`crates/octet-coding-agent/src/session_store.rs` `path_by_id`), so
`octet --resume <reference>` cannot open it; and the resolver inside the owning
process (`crates/octet-coding-agent/src/extensions/serve.rs`
`driver_for_delegated_session`) hands back a **read-only, locked inspection**
session reachable inside the owning process, not a launchable interactive one.

The host half of that primitive has landed:
`octet_agent::resolve_launchable_child_session(session_directory, reference)`
resolves the opaque handle from the session-owned durable roster with no live
agent, `Agent::session_delegation()` does the same in-process with the
process-local liveness the roster cannot carry, and every `agent/list` row
carries the token plus `launchable` / `launch_blocked` (a live in-process
worker, a worker parked at the approval boundary, and a vanished transcript all
fail closed with a bounded reason). The remaining primitive is CLI-side wiring:
`path_by_id` must accept the reference and hand the resolved host-only child
path to the launcher. Until that lands the pane is reported **blocked**,
naming that exact missing wiring, rather than fabricating a resume. The normal
read-only parent-controlled mode is unaffected.

## Session-scoped delegation

A worker is not a detached OS process. Its record is owned by the **session**,
not by the run that spawned it: the end of the owning run (including an aborted
or dropped turn) records an explicit `run_detached` boundary and leaves the
worker discoverable, while only an explicit stop, owner teardown, or team
shutdown retires it. The durable roster (`fleet.json` in the delegation session
directory) carries each worker's id, name, task, child-session reference,
status, and consumed budget, so the owning session can reattach it on a later
turn and a restarted process can reconstruct it as `detached` instead of losing
it silently. Execution caps do not drift up across that boundary: reattachment
takes a slot per record and leaves the excess visibly detached.

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
- `open-all` composes with the same contract: only workers the host currently
  reports as running are planned, each through the host's opaque session
  reference, so a pane never targets a stale session. A detached or parked worker
  is excluded **and named** in the report with its reattach/approve step, and a
  worker reattached by `/subagents wait` is planned again on the next run.

## Where the rest lives

- [Extension README](../extensions/octet-subagents/README.md): install, enable,
  and the quick start.
- [Runtime reference](../extensions/octet-subagents/REFERENCE.md): exact tool
  contracts, safety model, states, presentation, and release smoke recipe.
- [Sessions](sessions.md) · [Models, providers and reasoning](providers.md) ·
  [tmux setup](tmux.md).
