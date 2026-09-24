# octet-subagents reference

**Distribution version: 0.7.6.** Catalog commands below require version-matched
published assets. This source checkout targets local octet 0.8.0 via its exact runtime pin;
published 0.7.6 assets retain their historical compatibility.
See the [release record](../../docs/releases/v0.7.6.md) for publication and
installation evidence.

[Usage guide](README.md). This is the bundled API `0.2` runtime contract, not a
current extension-authoring example. This local checkout retains distribution `0.7.6` and API `0.2` but targets
exactly octet `0.8.0`; it does not announce a new public bundle release.

The executable launches named, single-purpose child conversations through the
host-owned `agent_sessions` service. It is not an agent team, graph/recipe runtime,
swarm, hosted-agent scheduler, or second Agent loop.

## Safety model

V1 is deliberately bounded, with the parent's full standard tool scope as the default grant:

- at most **eight active children** and thirty-two retained workers per parent owner;
- depth one; a recursively admitted descendant is immediately interrupted when its host path/depth is observed;
- four predefined profiles (`explore`, `review`, `test-analysis`, `research`);
- per-worker `provider`/`model`/`reasoning` selection, defaulting to `inherit`, with host-resolved configured routes and reasoning;
- requested tool scope is a non-empty duplicate-free subset of `read`, `search`, `edit`, `write`, and `bash`; the default grant is the full five-tool scope, and `tools: [read, search]` narrows a worker to hard read-only for pure investigations;
- wall-time, turn, and cost ceilings are optional per spawn: when omitted they inherit the parent session's ceilings (an unlimited parent remains unlimited); explicit values are bounded to 5 s–24 h, 1–256 turns, and 1–50,000,000 microdollars; returned output is 512–16,384 bytes;
- fresh child contexts use the selected model, bound inherited context/output limits by its capabilities, and inherit the optional session token ceiling exactly; an unlimited parent remains unlimited and the model-facing spawn schema has no separate token-budget field;
- strict owner derivation from `tool/call.context.resource_owner`; no tool schema accepts an owner;
- retry-safe spawn keys, bounded output/error retention, cooperative cancellation, explicit stop, continue (steer active / resume settled), an explicit parent wait/reattach surface, and session-scoped delegation (`orphaned` means *detached*, not dead);

`edit`, `write`, and `bash` are part of the default grant; the `tools`
argument narrows or restores any subset within the five-tool whitelist:
network, browser, computer control, mailbox/team
primitives, another agent primitive, and any other tool are rejected. The
canonical child policy keeps repository content and task text as data, not
policy, and never grants recursion or manager-generated commands.

The service creates the child with the selected model and inherited cwd/workspace, environment,
sandbox, approval policy, and extension policy, but `agent/spawn.policy` is the
hard per-child boundary: octet installs a detached tool snapshot containing only
the granted tools (never collaboration or agent primitives), applies the
requested per-child turn/cost ceilings or inherits the parent's ceilings when
they are omitted, inherits the parent's context/output and optional
session-token settings without inventing a child ceiling, accounts cumulative
tokens/cost, caps UTF-8 summary bytes, and owns the absolute wall deadline.
Each child starts a fresh context; its usage is mirrored into the root ledger
for accounting only, never inserted into the parent's model context, and never
charged to the parent's own-context token ceiling. The
eight-active/depth-one/thirty-two-retained limits are also checked by the
real host service. Extension restart or absence of polling cannot relax those
limits. A shared cwd/filesystem is **not isolation**.

The host returns an opaque `agent-session:*` reference rather than the private
delegation JSONL path. Serve resolves that reference only by inventorying its
owner-private delegation directories, opens the transcript through a
no-follow descriptor, and exposes a locked read-only session projection. The
reference carries no filesystem path and cannot be used to submit another
prompt or bypass the worker policy.

There is no dedicated writer profile in V1: mutation capability is granted per
spawn through the requested tool list and is enforced by the host's scoped
tool snapshot, not by cooperative prompts alone. A worker granted `edit`,
`write`, or `bash` operates inside the same shared filesystem the parent sees,
so grant mutation only for tightly scoped, verifiable work.

## Kernel boundary

The package contains decomposition/completion policy, tool and command definitions, semantic projection, fixtures, and tests. It does **not** contain a model loop or session store.

octet owns:

- the child model conversations and durable session files;
- ancestry, concurrency/depth/team limits, inherited permissions, and cost limits;
- owner/principal checks for every `agent/*` request;
- persistence, cancellation, restart service continuity, and descendant shutdown;
- completion mailbox claim/ack and delivery as a legal new parent event/turn.

The extension calls only these SDK helpers, which map directly to API `0.2`:

- `list_agent_models` → `agent/models` (requires `agent_model_selection_v1`);
- `spawn_agent` → `agent/spawn`;
- `list_agents` → `agent/list`;
- `wait_agents` → `agent/wait`;
- `interrupt_agent` → `agent/interrupt`;
- `send_agent_message` → `agent/message`;
- `follow_up_agent` → `agent/follow_up`.

`agent/message` steers an active worker and `agent/follow_up` resumes a
settled one; both are exposed only through `subagent_continue`. It does not
use the graph/recipe spike, built-in team mailboxes, or another scheduler.

## Install, enable, and trust

The source bundle requires local octet 0.8.0 and has one root directory named
`octet-subagents`. With [octet 0.7.6](../../docs/installation.md) and
version-matched published assets, use the catalog command:

```console
octet extension install octet-subagents
```

For source testing with a locally built octet 0.8.0 and reviewed local
archive, use `octet extension install --path ./octet-subagents-0.7.6.tar.gz`.

Installation/discovery is inert: it never enables, persists a trust grant, or
starts the process. The bundle is disabled by default. Default full access
(`unsafe_host`) implicitly trusts the selected extension without saving a grant;
executable activation still requires explicit enablement and the process gate.
Prefer separate OS isolation.
The current workspace bundle can be rebuilt and installed deterministically with:

```console
./scripts/reinstall-octet-subagents.sh
```

This updates `~/.octet/extensions/octet-subagents`; rebuilding `octet` with
`cargo run` alone does not replace an already installed extension bundle.
Explicitly enable it; no extra trust flag is needed in full access:

```console
octet --enable-extension octet-subagents
```

`--trust-extension` and source-bound `trusted_extensions` grants are optional
explicit trust decisions, not enablement. `--safe-mode` removes implicit trust
and never starts executable extensions even with explicit grants: startup still
requires `unsafe_host`, and trust is not an OS sandbox. Use `/extensions` to enable
or disable installed bundles, and `/extensions status` to inspect source, trust,
API, generation, and negotiated features. The tools return an explicit unavailable
result when the extension is not running or the host has not offered its
owner-bound `agent_sessions` service.

The bundle is self-contained and has no install hook or third-party dependency.
`vendor/octet_extension/` is a synchronized copy of octet's dependency-free Python
SDK. Python 3.9+ is required at runtime. The optional packaged skill at
`skills/octet-subagents/SKILL.md` is discovered after installation but remains
inactive until explicitly loaded with `/skills load octet-subagents`.

## Tools

### `subagent_spawn`

Launch a worker in the background by default:

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

There is intentionally no `max_tokens` argument. The child gets a fresh model
context with the parent's model context/output limits and inherits the parent's
optional cumulative session-token ceiling exactly (`null` remains unlimited).

### Per-worker provider, model, and reasoning

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

If no key is supplied, the extension derives one from the complete canonical request. Keys are scoped by octet to the extension principal and durable session owner. Identical retries return the same host-present child. If a new owning run retires that live host record, the worker becomes **detached**: still owned by this parent session, no longer attached to a run. The extension retains the last bounded summary/error, usage, and the complete sibling roster as recoverable evidence, reports the worker as `detached` with a reattach affordance, and reattaches it automatically when the owning session republishes the live record (see *Session-scoped delegation*). An explicit identical retry may also replace that detached cache entry and ask the host to create a new authoritative worker. Reuse with different input fails. The orchestration fingerprint is also placed in the canonical child message so a restart cannot accidentally make host-visible input equality narrower than extension input equality.

The immediate result is an acknowledgement, not completion. Continue independent parent work and let octet deliver the worker's concise final output through its durable parent mailbox. Set `background: false` only when a bounded foreground wait is actually useful.

### `subagent_status`

Refresh the authoritative host-present tree plus any bounded evidence retained after owning-run cleanup. `target` may be a displayed name, stable agent ID, or host path. Without a target it returns a compact list. A missing active record becomes explicitly `orphaned` — *detached*, meaning still owned by this session but currently not attached to any run — and carries `detached`/`reattachable`, its durable session reference, and a reattach instruction; captured summaries/errors, usage, and sibling rows never disappear. It never accepts a caller-supplied owner and never infers state from output prose.

### `subagent_wait`

Wait 1–60 seconds for one target or all owned workers. The host reverse request is cancellable and sliced to keep cancellation responsive. Expiring or cancelling the wait leaves workers in the background; reaching a worker's wall deadline requests host interruption and produces the distinct `timed_out` state.

The wait is also the explicit parent reattachment surface: its authoritative
`agent/list`/`agent/wait` reconcile is what lets the owning session pick a
detached worker back up, so a targeted wait returns
`reattachment: {state: "detached"|"reattached", ...}` and, for a worker the host
parked at the approval boundary, an explicit `approval` block. `/subagents wait
[name-or-id]` performs the same owner-bound wait from the command surface.


### `subagent_stop`

Provide exactly one of:

```json
{"target": "agent-1"}
```

or:

```json
{"all": true}
```

The host validates the target against the extension principal and current resource owner and interrupts the selected descendant tree. An accepted request remains `stopping` until a subsequent authoritative `agent/list` or `agent/wait` record reports the terminal interruption; acknowledgement alone is never presented as completion. Repeated stop on a terminal worker is a bounded no-op.

### `subagent_continue`

Provide a `target` (displayed name, stable agent ID, or host path) and a `message`:

```json
{"target": "explore-auth", "message": "Also check the revocation path."}
```

An active worker receives the message through `agent/message` as a queued
turn on its running session; a settled worker (`done`, `failed`,
`cancelled`, `stopped`, or `timed_out`) is resumed through `agent/follow_up`
as a new run of the worker's durable session, so the earlier conversation
context is retained. Workers still draining a stop (`stopping`) are rejected
with the stable `worker_stopping` error rather than raced. A detached worker
(`orphaned`) is rejected with the stable `detached` error until the owning
session republishes its live record — never resumed from a stale handle — and
a worker parked at the host approval boundary is rejected with the stable
`worker_awaiting_approval` error, because queueing work into a parked worker
would be unattended mutation. The host clears a settled record's completion
timestamp on resume, so elapsed time always measures the current run.

## `/subagents open-all <tmux|herdr>`

**Partial — all product pane execution is blocked pending atomic host writer
claim/settlement.** An owner-bound command refreshes `agent/list`, retains
`launchable`, `launch_blocked` and `live_task`, then reports blocked plans using
the host's opaque `agent-session:<sha256>` handle. The session store can resolve
that handle for `octet --resume`; handle support does not confer ownership.

Even settled/detached workers with `launchable: true`, `live_task: false` and no
host refusal remain blocked: **atomic host writer claim/settlement unavailable**.
Live or approval-parked workers retain their more specific refusal. Missing
records lose launchability; cached command fallback cannot authorize a launch.
The **parent pane stays blocked** because the calling process still owns it.

The existing `Session::persist` stale-length check is per-write, not a lifetime
writer lock. A launchability observation cannot prevent a concurrent host resume
or repeated launch. Consequently product open-all never splits panes or submits
commands; execution must not be enabled on snapshots or operator coordination.
A real host-owned exclusive claim/settlement primitive is required first. There
are no fabricated leases or new ownership APIs in this extension.

- Missing `tmux`/`herdr` or `octet` refuses without downloading/installing.
- At most eight candidate workers plus the blocked parent are planned. Detached,
  parked and other unlaunchable retained workers are named in skipped rows.
- Session IDs/references remain strict allowlisted separate argv elements. No
  transcript path, credential or token is used. Duplicate worker handles in one
  plan are refused even though all rows are blocked.
- herdr still requires `HERDR_ENV=1`; no outside-session control is attempted.
- The low-level adapters remain directly unit-tested: tmux builds session/window
  argv; herdr validates command tokens and all local bounds before splitting.
  Adapter failure stops further operations without destroying existing panes.
  Known pane IDs survive submission failure; malformed replies, nonzero
  acknowledgements and timeouts preserve uncertainty rather than claim no effect.
  These direct tests do not grant product execution authority.

The read-only parent-controlled panel and owner-bound continue/stop tools are
unchanged. Stub adapter tests do not qualify a real herdr installation or atomic
cross-process ownership transfer.

## Lifecycle and restart behavior

Worker states are authoritative projections of `agent/list`/`agent/wait`.
Host list/wait observations and their reconciliation are serialized per parent
owner; a delayed older response cannot overwrite a newer terminal snapshot.
Other owners can still refresh independently:

- `pending` → `queued`;
- `running` → `running` (temporarily `waiting` during a wait call);
- `completed` → `done`, with bounded exact host output in detail/results;
- `failed` → `failed`, with a bounded error;
- `interrupted` → `cancelled`, or `stopped`/`timed_out` when the extension issued that reason;
  reattachment with no undelivered task also settles as `interrupted`, with a host
  diagnostic directing the caller to `subagent_continue`. The retained session
  is not replayed automatically or presented as successfully completed;
- `shutdown`/missing active record → `orphaned` (**detached**: still owned by this session, currently not attached to any run, reattachable);
- an `awaiting_approval` park reported by the host → `awaiting_approval` (rendered explicitly, never resumed unattended).

When an owning run removes records before another status/wait observation, the
extension keeps its last bounded summaries, errors, usage, and complete sibling
roster instead of deleting the local tree. Previously active missing records
become explicit `orphaned` rows that mean **detached, not dead**: `detached` is
true, `reattachable` reflects whether the durable session reference was already
observed, `detached_at_ms` records when the detachment was first seen, and the
detail body states the reattach path. An identical explicit spawn retry may
replace its matching detached cache entry; the host remains authoritative for
new execution.

### Session-scoped delegation (reattachment)

A delegated child is not a detached OS process: the owning **run** owns the
record and the host retires it when the run ends. The extension therefore treats
a retired record as detachment, not death:

- **Reattachment.** The next authoritative `agent/list`/`agent/wait` observation
  is the reattach surface. When the owning session republishes a live record for
  a detached worker, the extension clears `detached_at_ms`, increments
  `reattach_count`, records `last_reattached_at_ms`, sets the phase to
  `reattached to the owning session`, and clears exactly the bounded detachment
  diagnostic it wrote earlier (a real host error is preserved). Captured
  summaries, errors, usage, and the complete sibling roster survive the whole
  cycle — that guarantee is deliberate and is covered by tests.
- **Explicit parent wait.** `/subagents wait [name-or-id]` (and
  `/subagents reattach [name-or-id]`) performs an owner-bound `agent/wait`, so
  the operator can force a reattach pass from the command surface. The cached
  fallback holds no live service client, so it reports the detached set and
  states that no wait was performed — never a silent stall and never a fake
  success. The tool result carries `reattachment: {state: detached|reattached}`
  and `approval` when the host parked the worker.
- **Unattended mutation.** If the host parks a detached worker at the approval
  boundary, the extension renders the bounded `awaiting_approval` state (panel
  row, detail body, wait result). `subagent_continue` refuses it with
  `worker_awaiting_approval`; `subagent_stop` still stops it explicitly.
- **Open-all remains Partial.** `/subagents open-all` retains the host's opaque
  `agent-session:*` reference in blocked plans, but never opens a worker pane
  without atomic host writer claim/settlement. Neither the handle nor a fresh
  launchability snapshot grants ownership.

A supervised extension restart receives a new process generation but the host service retains trees by stable extension principal plus durable session owner. The next owner-scoped call resyncs with `agent/list`, marks recovered records as restarted, and restores the public task name, profile, idempotency fingerprint, host-created/started/completed/deadline timestamps, policy, usage, and stable session reference. Retrying the same spawn key returns the same child without creating another session. A complete process-host rebuild creates a new service boundary for mutation; retained transcript inspection remains separately read-only and provenance-authorized.

Outstanding API requests are cooperatively cancelled. Cancelling a wait does not stop the worker. If spawn cancellation races a durable host create, the required idempotency key makes the next identical call safe; unsafe ambiguous work is not replayed with new input.

On extension shutdown the local projection settles, while octet's API `0.2` process shutdown stops every child tree owned by the extension service. The shutdown callback never reuses a stale parent request ID.

## TUI and Serve presentation

The bundled manifest declares:

```toml
[contributes]
presentation = true
```

The extension emits complete monotonic `presentation/update` snapshots using the generic host contract.
The process revision is assigned while capturing state under the orchestrator lock,
not when a callback happens to arrive. Publication is serialized outside that lock;
a delayed older capture cannot overwrite a newer state, selected detail, or owner.
A genuinely resumed run receives a newer capture and remains visible—terminal states
are not latched.

Snapshots contain:

- compact status counts;
- content-free activity rows;
- stable list/tree nodes and parentage;
- queued/running/waiting/done/failed/stopped/cancelled/timed-out/detached(`orphaned`)/awaiting-approval/restarted distinctions (a detached worker maps to the generic `degraded` state, not `unavailable`);
- elapsed time, inherited model/profile, turns, token/cost budgets, session/artifact references;
- current structured phase/tool and bounded recent tool arguments in explicit inspector detail, not compact rows;
- selected detail with `parent > worker` breadcrumb, policy provenance, inherited cwd/sandbox/approval/environment facts, the host-observed terminal summary (unsafe controls visibly escaped), artifacts, bounded error, and restart state;
- declared inspect, stop, and stop-all actions routed only to the manifest command.

Prompts, tool arguments/results, and running model prose never appear in the
worker list or composer-adjacent activity block. Transient tool identities and
phases also stay out of compact summaries so the lifecycle/usage columns do not
shift on each child tool start or finish. The host returns per-worker
structured phase/current tool, host-observed tool calls, disjoint provider token
buckets, turn count, and priced cost. The extension places those values in
generic activity `metrics`; it never supplies terminal rows or footer text. In
the TUI, octet renders a bounded owner-fenced **Subagents** strip pinned above
the composer while retained workers are active. It is chrome, not a persistent
transcript event, and never enters semantic copy. Native
`AgentEvent::DelegationUpdated` supplies the snapshot; the strip does not poll
`/subagents status`. Rows show lifecycle, model, metrics, and `tools` (tool-call
count, not model turns). Terminal groups collapse to counted summaries; Ctrl+O
expands within the height cap. `/subagents` retains the complete roster and
failure details after the strip hides. First-party orchestration calls/results
and worker state/reason transitions do not append transcript notices; ordinary
tool/run errors and approval prompts remain visible. Input
includes the three disjoint uncached/cache-read/cache-write buckets, while
reasoning remains a subset of output.

Before the root run settles, octet stops and briefly joins its children, sums each
child session's durable usage/cost records including picodollar remainders, and
writes one `delegated_agent` usage record per worker into the root session. The
live child total is included in the footer only until that durable handoff, so
delegated spend contributes exactly once to cumulative session cost and later
cost-limit checks.

The opaque worker resource reference is stable and owner-scoped.
Serve opens it only after host-written provenance binds the exact parent session,
path-free extension principal, and resource owner; the web view is locked and
read-only. The TUI's live block is host-rendered from semantic activity metrics;
no extension status or footer contribution is rendered. The no-argument
`/subagents` command opens a host-owned list: Up/Down moves between workers,
Enter opens the selected scrollable read-only transcript, and Escape or Left
returns to the list. The same owner-bound status command used by the live tick
and open panel reconciles authoritative `agent_sessions` state and publishes the
next complete presentation revision; the frontend keeps focus by stable node ID
and revalidates the latest owner-scoped reference before opening it.
`/extensions inspect agent-session:<digest>` remains the explicit reference
fallback. Both paths open only a child in the current parent's delegation team.
Neither frontend can submit prompts or mutate a worker; all mutation remains on
owner-bound `agent_sessions`. The package supplies no Rust TUI plugin, web code,
or frontend scheduler. Generic rendering, selection/navigation, reconnect and
instance/generation fencing, authenticated action routing, and Serve transport
are host-owned.

### `/subagents` headless/narrow fallback

```text
Subagents · 1 running · 1 done
├─ explore-auth         running    00:42  agent-1
└─ inspect-tests        done       01:08  agent-2
```

Use `/subagents inspect <name-or-id>` for cached read-only detail. The octet coding
host binds API `0.2` command requests to their host-derived owner, so an explicit
`/subagents stop ...` and the generic TUI/Serve stop action use the same
owner-checked `agent_sessions` path. A host or headless integration that omits
`context.resource_owner` fails closed without issuing a stop. The extension
never smuggles a stale request ID into a command. A cached list may lag; run
`subagent_status` from an active model turn to resync.

## Release smoke recipe

Compare measurements from the same task run once directly and once with up to two read-only workers:

```console
./release-smoke.py \
  --direct /tmp/direct.json \
  --subagents /tmp/subagents.json \
  --require-gain
```

Each input records accepted finding IDs, input/output tokens, wall time, CPU time, peak RSS, duplicate findings, and failure classes. The script reports quality gain and resource deltas. It consumes caller-captured measurements and never starts a provider or makes a network call during packaging.

A deterministic fixture smoke is:

```console
./release-smoke.py \
  --direct fixtures/smoke/direct.json \
  --subagents fixtures/smoke/subagents.json \
  --require-gain
```

For a real evaluation, keep the prompt, model, reasoning, workspace revision, and acceptance rubric fixed. Count only reviewed/accepted unique findings; record timeouts, provider failures, cancellation, duplicate findings, and policy violations rather than discarding failed trials. A fixture result is not a measured live gain.

## Tests

From the package root:

```console
python3 -m unittest discover -s tests -v
```

The package-owned fake host service covers owner/principal isolation, concurrency, duplicate keys, cancellation races, timeout interruption, supervised restart/resync, completion claim/ack and legal parent-turn delivery, session/export inspection, and descendant shutdown. Protocol tests run the vendored SDK over JSON-RPC streams and verify negotiation, owner correlation, presentation updates, `/subagents`, cancellation, and graceful shutdown. Release tests verify manifest/archive bounds, SDK synchronization, fixtures, executable bits, and the smoke report. Fixture coverage does not qualify live-provider behavior or measured gains.
