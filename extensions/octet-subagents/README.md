# octet-subagents

**Source candidate 0.7.4 is UNPUBLISHED.** Public catalog commands below apply
to published octet 0.7.3 and its matching bundle; local candidate sources or
archives require a locally built octet 0.7.4. See
[the open release gates](../../docs/releases/v0.7.4.md).

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
  "model": "inherit",
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
| `subagent_continue` | Supply `target` and `message` to steer an active worker or resume a settled one with its conversation retained. Stopping and orphaned workers are rejected. |

<a id="enable-the-local-bundle"></a>

## Install and enable

With [octet 0.7.3 installed](../../docs/installation.md), once the matching signed
public bundle is published, install it, then explicitly enable it:

```console
octet extension install octet-subagents
octet --enable-extension octet-subagents
```

For candidate-only testing with a locally built octet 0.7.4 and reviewed local
archive, use `octet extension install --path ./octet-subagents-0.7.4.tar.gz`.
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
- Profiles are `explore`, `review`, `test-analysis`, and `research`. The model is
  inherited; there is no override or separate `max_tokens` argument.
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

## Reference

Bundle `0.7.4` requires exactly octet `0.7.4` and retains API `0.2`. The detailed
contract is a bundled-runtime reference, not a current extension SDK tutorial.

- <a id="safety-model"></a>[Safety model](REFERENCE.md#safety-model): exact grants, ceilings, ownership, and accounting.
- <a id="kernel-boundary"></a>[Kernel boundary](REFERENCE.md#kernel-boundary): host service ownership.
- <a id="install-enable-and-trust"></a>[Install, enable, and trust](REFERENCE.md#install-enable-and-trust): local rebuild and inactive skill discovery.
- <a id="tools"></a>[Tools](REFERENCE.md#tools): [spawn](REFERENCE.md#subagent_spawn), [status](REFERENCE.md#subagent_status), [wait](REFERENCE.md#subagent_wait), [stop](REFERENCE.md#subagent_stop), and [continue](REFERENCE.md#subagent_continue).
  <a id="subagent_spawn"></a><a id="subagent_status"></a><a id="subagent_wait"></a><a id="subagent_stop"></a><a id="subagent_continue"></a>
- <a id="lifecycle-and-restart-behavior"></a>[Lifecycle and restart behavior](REFERENCE.md#lifecycle-and-restart-behavior): authoritative states, retries, resync, and shutdown.
- <a id="tui-and-serve-presentation"></a>[TUI and Serve presentation](REFERENCE.md#tui-and-serve-presentation): privacy, usage, and owner-fenced inspection.
  - <a id="subagents-headlessnarrow-fallback"></a>[/subagents headless/narrow fallback](REFERENCE.md#subagents-headlessnarrow-fallback).
- <a id="release-smoke-recipe"></a>[Release smoke recipe](REFERENCE.md#release-smoke-recipe): measured inputs versus deterministic fixtures; no claimed live gain.
- <a id="tests"></a>[Tests](REFERENCE.md#tests).
