# octet-subagents

**Distribution: 0.8.0.** This bundle requires exactly octet 0.8.0.
Use the [version-matched installation](../../docs/installation.md) and the
[0.8.0 release record](../../docs/releases/v0.8.0.md) for signed assets and
public-install evidence. Reviewed source checkouts and local archives remain
separate installation options.

Delegate a bounded task to a background worker while the parent continues other
work. octet owns the child conversations, permissions, persistence, limits, and
shutdown. This is not an agent team, swarm, or second model loop.

## Try a read-only investigation

After [installing and enabling the bundle](#install-and-enable), a `subagent_spawn`
call can narrow a worker to reading and search:

```json
{
  "name": "explore-auth",
  "task": "Trace authentication ownership and report relevant files and invariants.",
  "profile": "explore",
  "provider": "inherit",
  "model": "inherit",
  "reasoning": "inherit",
  "tools": ["read", "search"],
  "timeout_seconds": 300,
  "max_turns": 8,
  "max_output_bytes": 8192,
  "max_cost_microdollars": 200000,
  "background": true,
  "idempotency_key": "auth-audit-v1"
}
```

The acknowledgement is **not completion**. Keep doing independent work;
octet delivers the final output through its durable parent mailbox. Reusing an
identical spawn key is retry-safe; using it with different input fails.

| Tool | Use |
| --- | --- |
| `subagent_models` | Discover configured, credential-available worker models; optional query and limit (default 50, max 100). |
| `subagent_spawn` | Start a named worker; `background: false` requests a bounded foreground wait. |
| `subagent_status` | Resync one `target` or list all owned workers. |
| `subagent_wait` | Wait 1–60 seconds; cancelling or expiring the wait does not stop workers. |
| `subagent_stop` | Supply exactly one of `{"target":"explore-auth"}` or `{"all":true}`. Acknowledgement is not terminal completion. |
| `subagent_continue` | Supply `target` and `message` to steer an active worker or resume a settled one with its conversation retained. Stopping, `awaiting_approval`, and detached workers are rejected with stable errors. |

`provider`, `model`, and `reasoning` default to `inherit`. Use `subagent_models`
to discover exact configured routes and supported reasoning before selecting one.
Explicit routing requires negotiated `agent_model_selection_v1`; the host alone
validates and normalizes it, and unknown routes never fall back to the parent.
Requested and host-confirmed effective selections are visible in the inspector
and preserved across restoration/continuation. Credentials are never returned.

<a id="enable-the-local-bundle"></a>

## Install and enable

With [octet 0.8.0](../../docs/installation.md) and verified
matching published assets, install the bundle, then explicitly enable it:

```console
octet extension install octet-subagents
octet --enable-extension octet-subagents
```

For source testing with a locally built octet 0.8.0 and reviewed local
archive, use `octet extension install --path ./octet-subagents-0.8.0.tar.gz`.
Python 3.9+ is required. Installation has no hook or third-party dependency and
starts nothing; the bundle stays disabled until explicitly enabled. Default full
access (`unsafe_host`) implicitly trusts it without saving a grant. Optional
`--trust-extension` or source-bound `trusted_extensions` grants never enable it.
`--safe-mode` removes implicit trust and keeps executable processes stopped even
with explicit grants: startup still requires `unsafe_host`. Neither mode supplies
an OS sandbox. `/extensions status` shows the selected source, trust, API,
generation, and negotiated features. The packaged skill is separately opt-in with
`/skills load octet-subagents`.

## Bound the work

- At most **8 active children and 32 retained workers per parent owner**, depth one.
- Profiles are `explore`, `review`, `test-analysis`, and `research`. The
  provider/model/reasoning selection defaults to inherited; there is no separate
  `max_tokens` argument.
- The default tool grant is **read, search, edit, write, and bash**, not read-only.
  A requested list must be a non-empty, duplicate-free subset. No browser,
  network-specific, collaboration, or recursive agent tools are admitted.
- Workers inherit cwd, environment, sandbox, approval policy, and extension
  policy. A shared filesystem is **not isolation**. Scope mutations to owned
  paths; task prose cannot relax host policy.
- Omitted wall-time, turn, and cost ceilings inherit the parent's limits,
  including unlimited settings. Explicit limits and output bounds are in the
  [safety reference](REFERENCE.md#safety-model).

## Inspect the work

`/subagents` opens the host-owned worker list. Use Up/Down to select, Enter for a
scrollable read-only transcript, and Escape or Left to return. A bounded,
tool-like **Subagents** transcript block updates in place while workers are
active, including between root turns. Its heading counts worker states; up to
four active child lines show task and input/output tokens. Ctrl+O retains
disclosure; `/subagents` exposes all retained workers (up to 32), exact outcomes,
models, tool-call counts (not model turns), cost, and reasons after the block
settles. Prompts, tool arguments/results, and running model prose stay out of
the roster.

Worker state/reason transitions and raw first-party orchestration results
(including errors) append no automatic per-worker transcript or semantic-copy
notices, live or on replay. Actual failed/stopped or
approval-parked states, model-visible errors, durable results, and accounting are
unchanged; ordinary tool/run failures and approval prompts remain visible.
Serve inspection is also owner-bound and read-only; inspection cannot send a
prompt. `/subagents inspect <name-or-id>` provides cached detail and
`/extensions inspect agent-session:<digest>` is the explicit-reference fallback.

## Open the fleet in panes

**Partial — pane execution is blocked.** `/subagents open-all tmux` (or `herdr`)
refreshes owner-bound `agent/list` and reports bounded plans using opaque
`agent-session:<sha256>` handles. The host can resolve these handles, but even
`launchable: true` with `live_task: false` is only a snapshot, not exclusive
ownership. Every worker remains blocked with an explicit reason; otherwise-ready
workers report **atomic host writer claim/settlement unavailable**. The current
parent remains blocked because its calling process still owns it.

No pane is created or command submitted by product open-all, including repeated
calls. Execution cannot be enabled until the host supplies atomic writer
claim/settlement; operator coordination is not a substitute. The command never
installs a multiplexer and retains the eight-worker plan cap. Low-level tmux and
herdr adapters retain preflight, partial-failure reporting and direct stub tests;
those tests are not product ownership or live-handover qualification.
See [`/subagents open-all`](REFERENCE.md#subagents-open-all-tmuxherdr).

## Session-scoped delegation

A worker is owned by the parent session, not just the turn that spawned it.
Ending the parent turn does not stop its workers. After owner reconstruction,
the extension treats retained records as **detached, not dead**: summaries,
errors, usage, and the sibling roster remain recoverable. An owner-bound
reattach pass restores available workers as idle until an explicit follow-up;
it never invents a new task. A worker parked at the approval boundary is
rendered as `awaiting approval` and cannot be given unattended work. Host rebuilds
restore extension ownership from the persisted principal and resource-owner
fences. Each root session has a separate roster alongside its lease, even when
sessions share a delegation directory. Releasing an owner retains settled
output/error, terminal status, and completion time independently of attachment.

## Reference

This source bundle has distribution version `0.8.0` and uses API `0.4`, with an
exact runtime requirement of octet `0.8.0`. The detailed
contract is a bundled-runtime reference, not a general extension SDK tutorial.

- <a id="safety-model"></a>[Safety model](REFERENCE.md#safety-model): exact grants, ceilings, ownership, and accounting.
- <a id="kernel-boundary"></a>[Kernel boundary](REFERENCE.md#kernel-boundary): host service ownership.
- <a id="install-enable-and-trust"></a>[Install, enable, and trust](REFERENCE.md#install-enable-and-trust): local rebuild and inactive skill discovery.
- <a id="tools"></a>[Tools](REFERENCE.md#tools): [spawn](REFERENCE.md#subagent_spawn), [status](REFERENCE.md#subagent_status), [wait](REFERENCE.md#subagent_wait), [stop](REFERENCE.md#subagent_stop), and [continue](REFERENCE.md#subagent_continue).
  <a id="subagent_spawn"></a><a id="subagent_status"></a><a id="subagent_wait"></a><a id="subagent_stop"></a><a id="subagent_continue"></a>
- <a id="lifecycle-and-restart-behavior"></a>[Lifecycle and restart behavior](REFERENCE.md#lifecycle-and-restart-behavior): authoritative states, retries, resync, and shutdown.
  - <a id="session-scoped-delegation-reattachment"></a>[Session-scoped delegation (reattachment)](REFERENCE.md#session-scoped-delegation-reattachment): detached is recoverable, not terminal.
- <a id="subagents-open-all-tmuxherdr"></a>[/subagents open-all <tmux|herdr>](REFERENCE.md#subagents-open-all-tmuxherdr): the pane-per-worker escape hatch.
- <a id="per-worker-provider-model-and-reasoning"></a>[Per-worker provider, model, and reasoning](REFERENCE.md#per-worker-provider-model-and-reasoning): host-resolved selection and reasoning.
- <a id="tui-and-serve-presentation"></a>[TUI and Serve presentation](REFERENCE.md#tui-and-serve-presentation): privacy, usage, and owner-fenced inspection.
  - <a id="subagents-headlessnarrow-fallback"></a>[/subagents headless/narrow fallback](REFERENCE.md#subagents-headlessnarrow-fallback).
- <a id="release-smoke-recipe"></a>[Release smoke recipe](REFERENCE.md#release-smoke-recipe): measured inputs versus deterministic fixtures; no claimed live gain.
- <a id="tests"></a>[Tests](REFERENCE.md#tests).
