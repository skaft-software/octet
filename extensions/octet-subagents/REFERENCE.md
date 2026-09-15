# octet-subagents reference

**Distribution version: 0.7.6.** Catalog commands below require version-matched
published assets. Source checkouts and local archives require exactly octet 0.7.6.
See the [release record](../../docs/releases/v0.7.6.md) for publication and
installation evidence.

[Usage guide](README.md). This is the bundled API `0.2` runtime contract, not a
current extension-authoring example. Distribution `0.7.6` targets exactly octet
`0.7.6`; the API remains `0.2`.

The executable launches named, single-purpose child conversations through the
host-owned `agent_sessions` service. It is not an agent team, graph/recipe runtime,
swarm, hosted-agent scheduler, or second Agent loop.

## Safety model

V1 is deliberately bounded, with the parent's full standard tool scope as the default grant:

- at most **eight active children** and thirty-two retained workers per parent owner;
- depth one; a recursively admitted descendant is immediately interrupted when its host path/depth is observed;
- four predefined profiles (`explore`, `review`, `test-analysis`, `research`);
- per-worker `provider`/`model`/`reasoning` selection, defaulting to `inherit` (the API `0.2` service still carries no per-child model field, so a selection is accepted only when this session can confirm it and is otherwise refused with `unsupported_model`/`unsupported_reasoning`);
- requested tool scope is a non-empty duplicate-free subset of `read`, `search`, `edit`, `write`, and `bash`; the default grant is the full five-tool scope, and `tools: [read, search]` narrows a worker to hard read-only for pure investigations;
- wall-time, turn, and cost ceilings are optional per spawn: when omitted they inherit the parent session's ceilings (an unlimited parent remains unlimited); explicit values are bounded to 5 s–24 h, 1–256 turns, and 1–50,000,000 microdollars; returned output is 512–16,384 bytes;
- fresh child contexts inherit the parent's model, context/output limits, and optional session token ceiling exactly; an unlimited parent remains unlimited and the model-facing spawn schema has no separate token-budget field;
- strict owner derivation from `tool/call.context.resource_owner`; no tool schema accepts an owner;
- retry-safe spawn keys, bounded output/error retention, cooperative cancellation, explicit stop, continue (steer active / resume settled), an explicit parent wait/reattach surface, and session-scoped delegation (`orphaned` means *detached*, not dead);

`edit`, `write`, and `bash` are part of the default grant; the `tools`
argument narrows or restores any subset within the five-tool whitelist:
network, browser, computer control, mailbox/team
primitives, another agent primitive, and any other tool are rejected. The
canonical child policy keeps repository content and task text as data, not
policy, and never grants recursion or manager-generated commands.

API `0.2` creates the child with inherited model, cwd/workspace, environment,
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

The source bundle requires octet 0.7.6 and has one root directory named
`octet-subagents`. With [octet 0.7.6](../../docs/installation.md) and
version-matched published assets, use the catalog command:

```console
octet extension install octet-subagents
```

For source testing with a locally built octet 0.7.6 and reviewed local
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

`provider`, `model`, and `reasoning` select the child's orchestration policy per
spawn. All three default to `inherit`: the child copies the parent session's
already-normalized selection exactly, which is the recommended default. A
selection is validated fail-closed and never silently coerced:

- `provider` and `model` are accepted **only when this session can confirm them
  as configured**. The API `0.2` service exposes neither a provider catalog nor a
  per-child model field, and the host reports exactly one model to the extension
  (the parent session's), so that is the only per-worker selection the extension
  can verify. A `model` that is not the confirmed model, a malformed or
  metacharacter-carrying id, and a `provider` supplied without a matching `model`
  are rejected with the typed `unsupported_model` error. Confirmed requests are
  reported as applied; nothing is claimed applied that the host did not confirm.
- `reasoning` accepts `inherit` or `off|minimal|low|medium|high|xhigh|max|ultra`.
  An unknown level is rejected with the typed `unsupported_reasoning` error. A
  level above the target model's ceiling is clamped by the *mirrored* product
  policy (`reasoning.py` reproduces `crates/octet-coding-agent/src/app/mod.rs`
  `thinking_to_reasoning`/`supported_levels_with_subagents`, including the
  `clamps_effort_to_model_ceiling` ladder) with an explicit note; the extension
  never invents a second policy. The caller may declare the target model's
  `reasoning_capability` (`ceiling`/`floor`/`ultra`); omitted means the product's
  wire defaults (floor `minimal`, ceiling `high`).
- The requested and effective selections are both surfaced: the live panel row
  shows an explicit request as `model <provider>/<model>` / `reasoning <level>`
  and marks a request the host has not confirmed, and the inspector body carries
  an `Orchestration selection` line plus the clamp note. `inherit` renders as
  absence (the panel never prints placeholder text for an inherited value).

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

The escape hatch: reopen the **parent session and every running worker** as
separate interactive octet sessions, one pane/window per session, so a worker can
be orchestrated independently of the read-only parent-controlled panel. Every
property below is enforced in code and covered by
`extensions/octet-subagents/tests/test_launcher.py`.

- **Fail closed when the multiplexer is absent.** `tmux`/`herdr` are discovered
  with a read-only `PATH` lookup. If the multiplexer is missing — or the `octet`
  binary is missing — the command refuses with an actionable message naming it.
  octet never downloads or installs a multiplexer.
- **Running workers only.** `done`/`failed`/`limit_reached`/`stopped`/`timed_out`
  workers get no pane; the parent always gets one. A worker that is still owned
  by the session but not attached to any run, and a worker parked by the host at
  the approval boundary, get no pane either — and are returned in the result's
  `skipped` rows and named in the report with the reattach/approve step, so the
  row can never become a silent omission.
- **Bounded.** At most `MAX_OPEN_ALL_PANES` panes (the eight-worker fleet cap plus
  the parent). Above the cap the whole request is refused before anything is
  created.
- **Shell-safe.** Every session identifier, path, and flag is a separate argv
  element; the multiplexer is always invoked with an argv list and never
  `shell=True`. A session id must match a strict allowlist and a worker handle
  must be an opaque `agent-session:<sha256>` reference, so a metacharacter can
  never reach a command line. herdr has no argv-list pane executor, so its single
  command string is assembled only from tokens that already passed the same
  shell-safe allowlist.
- **No secret leakage.** Only the opaque, path-free session reference and the
  host-provided session id are ever placed on a command line. No credential,
  token, or transcript path is read, printed, or argv-passed, and none appears in
  the notice text.
- **Clean failure.** Panes are created one at a time; the first failure stops the
  run, reports exactly what exists, destroys nothing, leaves no silently orphaned
  panes, and is safe to re-run.
- **Ownership.** herdr's documented agent guardrail requires `HERDR_ENV=1` (this
  process must already be inside a herdr-managed pane), so open-all refuses to
  drive a herdr session it does not own.
- **What a pane can launch today.** The parent pane resumes the host session id
  directly. A worker's only host-published handle is the opaque
  `agent-session:<sha256>` reference (`crates/octet-agent/src/delegation.rs`
  `delegated_session_reference`), which is one-way and names a delegated child
  transcript under the owner-private `.delegation/team-*/` directory. The session
  store resolves an id only as `<session-dir>/<id>.jsonl`
  (`crates/octet-coding-agent/src/session_store.rs` `path_by_id`), so
  `octet --resume <reference>` cannot open it; the host's only resolver for that
  reference (`crates/octet-coding-agent/src/extensions/serve.rs`
  `driver_for_delegated_session`: `AuthorityProfile::ReadOnly`,
  `SessionLiveState::Locked`) returns a read-only locked inspection session
  reachable inside the owning process, not a launchable interactive one. The
  worker argv is still planned and validated, but no resume is fabricated for it:
  the pane is reported as **blocked**, naming the exact missing primitive — a
  launchable handle for a session-owned delegated child, which session-scoped
  reattachment supplies. The normal read-only parent-controlled mode is untouched
  by this command.

## Lifecycle and restart behavior

Worker states are authoritative projections of `agent/list`/`agent/wait`:

- `pending` → `queued`;
- `running` → `running` (temporarily `waiting` during a wait call);
- `completed` → `done`, with bounded exact host output in detail/results;
- `failed` → `failed`, with a bounded error;
- `interrupted` → `cancelled`, or `stopped`/`timed_out` when the extension issued that reason;
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
- **Open-all composes.** `/subagents open-all` resolves each running worker's
  session through the same contract (the host's opaque `agent-session:*`
  reference) so a pane always targets the live session of a running worker
  rather than a stale one.

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
the TUI, octet renders the complete latest owner-fenced worker roster as a
persistent transcript event immediately above the composer from native
`AgentEvent::DelegationUpdated` events; ordinary tool disclosure never truncates
it. It does not poll `/subagents status` for the composer block. Worker rows are
indented beneath **Subagents** and show `name  state · ↑input ↓output • $cost`.
Per-worker call counts remain retained telemetry, not transcript-row text. Input
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
