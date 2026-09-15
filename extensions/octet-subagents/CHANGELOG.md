# Changelog

## [Unreleased]

### Added

- `/subagents open-all tmux|herdr`: reopen the parent session and every **running**
  worker as separate interactive octet sessions, one pane/window per session
  (`octet_subagents/launcher.py`). Fails closed when the multiplexer or the `octet`
  binary is missing (never downloads or installs one), refuses above the
  eight-worker pane cap, passes every session id/path/flag as a separate `argv`
  element (never a shell, never string interpolation, metacharacter ids refused),
  never prints or argv-passes a credential or token, and reports exactly what
  existed when the multiplexer fails partway. Worker panes are planned and
  validated but reported **blocked** for the opaque `agent-session:*` handle that
  `octet --resume` cannot resolve today, naming the exact missing primitive — a
  *launchable* handle for a session-owned delegated child (the session store
  resolves only `<session-dir>/<id>.jsonl`, and the host's only resolver for the
  reference returns a read-only locked inspection session) — instead of
  fabricating a resume. Workers that are still owned but not attached to any run,
  or parked at the host approval boundary, are returned as `skipped` rows and
  named in the report with the reattach/approve step, so open-all never opens a
  stale pane and never hides a live worker. herdr support is verified against
  herdr's own
  documentation (pane split via `.result.pane.pane_id`, `pane run` command string,
  the `HERDR_ENV=1` ownership guardrail).
- Per-worker `provider`, `model`, and `reasoning` spawn inputs
  (`octet_subagents/reasoning.py`, `model.py`), defaulting to `inherit`. The
  selection is validated fail-closed with typed errors and is never silently
  coerced: a model this session cannot confirm as configured is refused with
  `unsupported_model`, an unknown reasoning level with `unsupported_reasoning`, a
  provider must accompany a matching model, and a level above the model's ceiling
  is clamped by a mirror of the coding agent's own policy
  (`clamps_effort_to_model_ceiling`/`supported_levels_gate_on_ceiling`). The panel
  and inspector surface the requested **and** effective selection.
- Session-scoped delegation (extension half): a worker whose host record the
  owning run retired is now explicitly **detached, not dead** — `detached`,
  `reattachable`, `detached_at_ms`, `reattach_count`, and `last_reattached_at_ms`
  are tracked, captured summaries/errors/usage and the complete sibling roster are
  retained, the owning session reattaches automatically when the host republishes
  the live record, `/subagents wait|reattach <name-or-id>` is an explicit
  owner-bound reattach surface, and a host-parked worker renders a bounded
  `awaiting_approval` state that `subagent_continue` refuses
  (`worker_awaiting_approval`) rather than mutating unattended.

- Live worker activity in the `/subagents` list: the host now exposes a
  bounded rolling `recent_tools` array (last six tool calls with flattened
  argument summaries, timing, and an error flag) on each `agent/list` record.
  Picker rows lead with the latest action — e.g. `read
  crates/octet-agent/src/delegation.rs` or `* search pattern=spawn_agent` for a
  call in flight, `!` prefixed after an error — so you can see what every
  worker is doing without opening its transcript. The inspect detail gains a
  "Recent tool activity" section and the headless narrow list shows the same
  action per row.

### Changed

- Workers now inherit the parent's full standard tool scope (`read`, `search`,
  `edit`, `write`, `bash`) by default; pass `tools: [read, search]` for a hard
  read-only guarantee. The child prompt states the granted scope instead of a
  blanket read-only ban, the `test-analysis` profile may run the checks it
  proposes, and spawn schema/skill/README guidance were updated.
- Terminal-gate rejections now carry cumulative session cost so per-worker cost
  tracks token usage between accepted turns.
- `orphaned` no longer reads as terminal. The label is now `detached` ("still
  owned by the session, currently detached from any run"), it maps to the generic
  `degraded` state instead of `unavailable`, detached workers are counted
  separately in the panel, and the `subagent_continue` rejection code is the
  stable `detached` rather than `orphaned`.
- Per-worker policy is no longer described as "inherited model only": the spawn
  schema, tool descriptions, README, and reference document the optional
  provider/model/reasoning selection and its fail-closed validation.

## [0.2.0]

### Added

- `subagent_continue`: steer an active worker through `agent/message` or
  resume a settled worker through `agent/follow_up`. A resumed worker keeps
  its durable conversation context; the host clears the stale completion
  timestamp and re-anchors an elapsed wall deadline so the new run owns a
  fresh budget.
- Per-spawn mutation grants: workers may be granted `edit`, `write`, and
  `bash` through the spawn `tools` list (the default remains the read-only
  `read, search` pair). The host's scoped tool snapshot is the enforcement
  boundary, not the worker's self-discipline.
- Per-spawn ceilings are now optional: omitted (or `null`) `timeout_seconds`,
  `max_turns`, and `max_cost_microdollars` inherit the parent session's
  ceilings, so an unlimited parent produces an unlimited child.
- Regression tests for granted mutation scope, the continue tool (steer,
  resume, stopping, and orphaned rejections), and protocol-level policy
  handling.

### Changed

- Raised limits from 2 active/16 retained to **8 active/32 retained**
  children per owner, with explicit ceilings raised to 256 turns, 50,000,000
  microdollars, and 24 hours of wall time (all overridable per spawn, all
  optional).
- The TUI composer-adjacent activity strip now appears only while workers
  are actively working, uses `•`/`└` glyphs with model-matched colours, and
  `Ctrl+O` expands it from the two to the five most recent workers (falling
  back to the verbose tool-output toggle only when no strip is visible).
- `agent/follow_up` on a settled child is a resume, not a rejection; the
  worker's persistent task and transcript survive between runs.

## [0.1.0]

### Added

- Add four bounded API `0.2` subagent tools over the host-owned `agent_sessions` service.
- Enforce two-child, depth-one, read/search-only orchestration policy with owner-derived scoping, idempotent spawn, budgets, timeout observation, cancellation, restart reconciliation, and shutdown settlement.
- Add generic semantic worker tree/activity/detail/actions and the cached `/subagents` narrow fallback.
- Ship the explicit activation skill, deterministic fake host fixtures, protocol/orchestration/presentation tests, synchronized Python SDK, and release smoke comparison.
