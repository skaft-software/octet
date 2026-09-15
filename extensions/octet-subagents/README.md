# octet-subagents

**Distribution version: 0.7.6.** Catalog commands below require version-matched
published assets. Source checkouts and local archives require exactly octet 0.7.6.
See the [release record](../../docs/releases/v0.7.6.md) for publication and
installation evidence.

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
| `subagent_spawn` | Start a named worker; `background: false` requests a bounded foreground wait. |
| `subagent_status` | Resync one `target` or list all owned workers. |
| `subagent_wait` | Wait 1–60 seconds; cancelling or expiring the wait does not stop workers. |
| `subagent_stop` | Supply exactly one of `{"target":"explore-auth"}` or `{"all":true}`. Acknowledgement is not terminal completion. |
| `subagent_continue` | Supply `target` and `message` to steer an active worker or resume a settled one with its conversation retained. Stopping, `awaiting_approval`, and detached workers are rejected with stable errors. |

`provider`, `model`, and `reasoning` select the worker's orchestration policy per
spawn and default to `inherit` (copy the parent session exactly). A selection is
validated fail-closed: a `model` this session cannot confirm as configured is
refused with `unsupported_model`, an unknown reasoning level with
`unsupported_reasoning`, and a level above the model's ceiling is clamped by the
coding agent's own policy — never silently coerced. Requested and effective
selections are both visible in the panel and inspector.

<a id="enable-the-local-bundle"></a>

## Install and enable

With [octet 0.7.6 installed](../../docs/installation.md), install the matching
signed public bundle, then explicitly enable it:

```console
octet extension install octet-subagents
octet --enable-extension octet-subagents
```

For source testing with a locally built octet 0.7.6 and reviewed local
archive, use `octet extension install --path ./octet-subagents-0.7.6.tar.gz`.
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
scrollable read-only transcript, and Escape or Left to return. The transcript's
complete worker roster is indented beneath **Subagents** and shows state, token
usage, and cost without per-worker call counts. Tool-call telemetry remains in
the inspector; prompts, tool arguments/results, and running model prose stay
out of the roster.
Serve inspection is also owner-bound and read-only; inspection cannot send a
prompt. `/subagents inspect <name-or-id>` provides cached detail and
`/extensions inspect agent-session:<digest>` is the explicit-reference fallback.

## Open the fleet in panes

`/subagents open-all tmux` (or `herdr`) reopens the **parent session and every
running worker** as separate interactive octet sessions, one pane/window per
session — the escape hatch for orchestrating a worker independently of the
read-only parent-controlled panel.

It fails closed when the multiplexer is missing (octet never installs or
downloads one), refuses above the eight-worker pane cap, passes every session id
and path as a separate argv element, and never prints or argv-passes a credential
or token. Panes are created one at a time; the first failure reports exactly what
exists and destroys nothing. A worker's only host-published handle is the opaque
`agent-session:<sha256>` reference, which `octet --resume` cannot resolve today,
so the worker argv is planned and validated but the pane is reported **blocked**
with the exact missing host primitive instead of a fabricated resume; the parent
pane opens normally. See
[`/subagents open-all`](REFERENCE.md#subagents-open-all-tmuxherdr) and
[`docs/subagents.md`](../../docs/subagents.md).

## Session-scoped delegation

A worker is not a detached OS process: the owning run retires its record when the
run ends. The extension treats that as **detached, not dead** — the worker is
still owned by the parent session, keeps its summaries, errors, usage, and the
complete sibling roster, and is reattached automatically when the session
republishes the live record. `/subagents wait [name-or-id]` forces an owner-bound
reattach pass, and a worker the host parks at the approval boundary is rendered
as `awaiting approval` and cannot be given unattended work.

## Reference

Bundle `0.7.6` requires exactly octet `0.7.6` and retains API `0.2`. The detailed
contract is a bundled-runtime reference, not a current extension SDK tutorial.

- <a id="safety-model"></a>[Safety model](REFERENCE.md#safety-model): exact grants, ceilings, ownership, and accounting.
- <a id="kernel-boundary"></a>[Kernel boundary](REFERENCE.md#kernel-boundary): host service ownership.
- <a id="install-enable-and-trust"></a>[Install, enable, and trust](REFERENCE.md#install-enable-and-trust): local rebuild and inactive skill discovery.
- <a id="tools"></a>[Tools](REFERENCE.md#tools): [spawn](REFERENCE.md#subagent_spawn), [status](REFERENCE.md#subagent_status), [wait](REFERENCE.md#subagent_wait), [stop](REFERENCE.md#subagent_stop), and [continue](REFERENCE.md#subagent_continue).
  <a id="subagent_spawn"></a><a id="subagent_status"></a><a id="subagent_wait"></a><a id="subagent_stop"></a><a id="subagent_continue"></a>
- <a id="lifecycle-and-restart-behavior"></a>[Lifecycle and restart behavior](REFERENCE.md#lifecycle-and-restart-behavior): authoritative states, retries, resync, and shutdown.
  - <a id="session-scoped-delegation-reattachment"></a>[Session-scoped delegation (reattachment)](REFERENCE.md#session-scoped-delegation-reattachment): detached is recoverable, not terminal.
- <a id="subagents-open-all-tmuxherdr"></a>[/subagents open-all <tmux|herdr>](REFERENCE.md#subagents-open-all-tmuxherdr): the pane-per-worker escape hatch.
- <a id="per-worker-provider-model-and-reasoning"></a>[Per-worker provider, model, and reasoning](REFERENCE.md#per-worker-provider-model-and-reasoning): fail-closed selection and mirrored clamping.
- <a id="tui-and-serve-presentation"></a>[TUI and Serve presentation](REFERENCE.md#tui-and-serve-presentation): privacy, usage, and owner-fenced inspection.
  - <a id="subagents-headlessnarrow-fallback"></a>[/subagents headless/narrow fallback](REFERENCE.md#subagents-headlessnarrow-fallback).
- <a id="release-smoke-recipe"></a>[Release smoke recipe](REFERENCE.md#release-smoke-recipe): measured inputs versus deterministic fixtures; no claimed live gain.
- <a id="tests"></a>[Tests](REFERENCE.md#tests).
