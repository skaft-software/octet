# Pi compatibility deviations

Octet is a Rust rewrite of Pi. Where Octet's own behavior differs from Pi in a
way a Pi extension can observe, **the Pi-compat path follows Pi**; Octet's native
behavior may differ for its own users. This register records every such
deviation, the decision, and its state, so a rewrite difference can never again
be mistaken for "compatible enough".

Target reference: Pi `1.5.0` (`~/github/earendil-works/pi`). The extension API
ledger ([pi-extension-api.md](pi-extension-api.md)) records member-level
coverage; this file records *behavioral divergence*.

States: **fixed** (compat path now matches Pi), **in flight** (assigned, not yet
verified), **deliberate** (Octet differs on purpose; documented and diagnosed),
**open** (known divergence, not yet addressed).

## Prompt → agent loop

| Deviation | Pi | Octet | State |
|---|---|---|---|
| `input` hooks in print/plain/RPC modes | Runs `_runInputHandlers` before skill/template expansion | Previously skipped in headless modes; transforms and `handled` ignored | **fixed** — `modes/{print,plain,rpc}.rs` run hooks with `source: "interactive"`/`"rpc"` before expansion; `handled` saves nothing and starts no request |
| `input` hooks on queued steer/follow-up | `_queueUserInput` → `_runInputHandlers` | Active-input path is synchronous and bypassed hooks | **in flight** |
| Prompt hook timeout | None | `PROMPT_RPC_DEADLINE` 5 s dropped the handler's transform/system replacement | **fixed** — deadline removed; transport no longer ends a prompt-phase wait |
| Adapter prompt budget | No per-handler prompt timeout | 2 s per handler / 4 s total; timed-out transform dropped with `ran:false` | **fixed** — `input`, `before_agent_start`, `session_start` unbudgeted; silent drop removed |
| `session_start` hook budget | None | 5 s cap skipped the extension (visible as "hook took over 5 s and was skipped") | **fixed** — unbudgeted |
| Tool-schema budget | None | Default 128 KiB refused 117-tool fleets before any provider I/O | **fixed** — default is `None`; full-request model context still enforced; explicit quota still available |
| OpenAI `strict` field | Omits default `false` | Sends explicit `false` | **deliberate** — semantically identical; comparison normalizes only this default |
| `before_agent_start` `images` | Passes attached prompt images | Always `undefined` | **open** |
| `before_agent_start` `messageType: "custom"` | Emitted for `sendMessage(..., {triggerTurn:true})` | Always `"user"` | **open** |
| Noninteractive message injection | Works in RPC/print | Incomplete | **open** |
| Compaction summary bound | No refusal | Refuses a handoff over 2 MiB instead of truncating | **open — to free** — refusal is an Octet budget Pi does not have |

## Extensions and host features

| Deviation | Pi | Octet | State |
|---|---|---|---|
| Child agent sessions | `createAgentSession` available to factories | Authority granted only to `octet-subagents` | **in flight** |
| `user_bash` results | Result transformation applied | Observed only; results refused | **in flight** |
| `ctx.abort` | Aborts the current run | No root-run abort contract | **in flight** |
| `provider_stream_event` | Delta observation/transformation | Unsupported | **in flight** |
| `@gotgenes/pi-permission-system` in `--print` | Tool guard runs; Pi has no per-hook timeout | Handler blocks ~30 s inside its own code (no host call in that window), so the host's 30 s request timeout ends the tool call with `extension request hook/run timed out` | **open — third-party headless path**; see [Known limitation: headless permission hook](#known-limitation-headless-permission-hook) |
| Virtual models | `registerVirtualModel`/`unregisterVirtualModel` | Unsupported | **in flight** |
| Provider request signing / OAuth | Supported surface | Refused where credential shapes cannot be represented | **open** |
| Builtin `grep`/`find`/`ls` factories | First-class tools | Wrappers refused; no native equivalents | **in flight** |
| Custom builtin `operations` (e.g. `ssh`) | Supported | Refused | **in flight** |
| Settings mirror | Full settings schema applies | Only theme, model, thinking projected | **in flight** |
| `navigateTree`, `ctx.shutdown` | Supported | Incomplete | **open** |
| `project_trust` events | Emitted | Unsupported | **open** |
| MCP HTTP, credentials, namespaces | Supported | stdio subset only | **open** |
| Windows `pi.exec` | Supported | Unix only | **open** |

## Security invariants (not compatibility refusals)

Policy: **Octet stays minimal.** Pi surface belongs in `octet-pi-compat` and
should be resolved from the user's own Pi installation, not reimplemented in
Rust. A budget, deadline, quota or list cap that Pi does not have is a defect in
the compat path and is removed, not tuned. Octet's extension API must not grow
Pi-shaped members: capability that only Pi exposes goes in the adapter.

The exception is a genuine safety invariant, which is not a Pi capability being
refused: workspace-trust admission, effect policy for external mutations,
no-follow path handling, bounded redaction of private remote data, persistence
before execution, and cancellation. Those stay, because weakening them would
change what Octet is allowed to do on the user's machine, not merely how
faithfully it imitates Pi. Where such an invariant is user-visible, it must be
diagnosed loudly rather than silently changing a Pi result.

| Area | Octet bound | Status |
|---|---|---|
| `isProjectTrusted` | Conservative; can tighten, cannot silently re-promote | Kept — never weakens native trust admission |
| Resource path admission | Requires explicit workspace trust and grants | Kept — native trust is authoritative |
| Unsafe host effects | Effect policy still applies to Pi tools | Kept — Octet owns host authority |
| `onTerminalInput` 1 s hang limit | Pi blocks typing indefinitely | Slow listeners are waited for in order; only a listener silent for 1 s is latched off (compatibility, not security) |
| Editor draft/history/paste/menu bounds | Bounded | **open — to free** where Pi is unbounded |

## Using this register

A row moves to **fixed** only when a real-host acceptance path (real `App`, real
adapter process, real tools/policy/persistence, scripted provider only) proves
the compat path matches Pi, and the ledger row cites it. Loading, registering, or
synthetic-host tests never move a row.

## Known limitation: headless permission hook

**Observed, reproducible.** With `@gotgenes/pi-permission-system` installed in the
mirrored Pi package set, a `--print` run that calls a tool fails that tool call:

```
extension request `hook/run` timed out
```

Measured on 2026-11-06 with octet 0.10.1 and the operator's Pi 1.5.0 package set:

| Stage | Duration |
|---|---|
| `before_tool_call` hook round trip | 29.1 s then 30.0 s on retry |
| `after_tool_call` | 0.05 s |
| `session_start` | 1.9–7.3 s |
| `before_prompt` | 0.45 s |

Isolation: the same run with `-ne` executes `bash` in 2.4 s, and the interactive
TUI executes the same tool in 2.8 s. In-process instrumentation attributes the
stall to that extension's `tool_call` handler and records **no host call** during
the stall, so the bridge is not waiting on a prompt or any other host round trip;
the handler is blocked in its own code. The host's
`DEFAULT_REQUEST_TIMEOUT` (30 s) is what ends the request.

Consequences a script must not ignore:

- The **run still exits 0**. The tool call is reported to the model as an error,
  the model answers without the tool result, and the process completes normally.
  Treating exit status as proof that tools ran is wrong in this configuration.
- Interactive use is unaffected, and the limitation is specific to a Pi package
  that blocks in a headless session; `octet` does not add a timeout Pi has.

Next step is to determine which Pi API that handler awaits in a headless session
before deciding whether the bridge or the extension owns the fix.
