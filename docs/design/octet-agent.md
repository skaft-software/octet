# octet agent design

## Responsibilities

`octet-agent` owns one mutable session, reconstructs canonical provider context, opens and consumes provider streams, executes registered tools, persists complete semantic records, and emits frontend-neutral events. Provider wire formats stay in `octet-ai`; terminal policy stays in `octet-coding-agent`.

## Commit and cancellation invariants

1. Streaming deltas are provisional and never enter the session. Opt-in provider lifecycle feedback is likewise forwarded only as transient `AgentEvent` telemetry; it does not mutate context, assembled assistant content, durable telemetry, or session records.
2. A complete assistant message is persisted before any emitted tool is executed.
3. Each tool result is persisted immediately after its execution outcome is committed.
4. A completed call marked schema-invalid by `octet-ai`'s request snapshot receives a static bounded paired error; it never reaches effect classification, hooks, or the tool implementation.
5. Crash replay requires both `ReplaySafety::Safe` and an exact host classification of `Pure` or `WorkspaceRead`. Every other unresolved call becomes an indeterminate error and is not executed.
6. One level-triggered abort signal is selected against provider open/body consumption, retries, tools, and autonomous compaction. Cancellation wins same-poll races. A cancelled compaction persists neither usage nor summary.
7. Every driven run emits exactly one `RunFinished` and one durable checkpoint.
8. Optional telemetry is an observer outside the session ledger. When installed,
   it receives an opaque run-start hook and coarse request/tool/compaction
   boundaries; it records hashes and bounded measurements, never raw prompts,
   arguments, results, or provider payloads.
9. `Agent::prompt_without_tools` starts with a sticky tool-free policy.
   `RunControl::finish_now` persists its input at the next safe turn boundary and
   makes that policy sticky for the remainder of the run. Subsequent provider
   requests contain no tool schemas and set `ToolChoice::None`; calls emitted by
   an already-open provider request are paired with synthetic errors rather than
   executed. Effects already admitted at the time of the control settle under
   the ordinary cancellation and commit rules.

Streamed tool-call start/delta/end events describe provisional generation; they
never execute a tool. Automatic speculative Bash execution is removed: shell
command spelling, broker admission, and later argument equality cannot establish
read-only effects, independence from earlier mutations, or durable ordering.
Cancellation cannot undo an early observation or effect. Mixed/dependent tool
batches retain emitted execution order; eligible independent observations may
still overlap through the effect-checked **post-persistence** parallel path.
This deliberately gives up unsafe overlap rather than promising equivalent
latency. See the [performance contract](performance.md).

## In-process provider recovery

Recovery surrounds one unfinished provider request, never `Agent::prompt` or
previously committed tool effects. The initial qualified contract is an
`OpenAiResponses` model with the **host-selected**
`ResponsesRuntimeProfile::Codex`. Selecting that profile now also asserts that
this route performs generation with locally dispatched function tools; an
embedding host must not assign it to an arbitrary compatible endpoint merely
for wire formatting. Model names, provider labels, Responses Lite, event counts,
and invisible output cannot grant recovery authority.

Canonical local function tools qualify, including mutations that remain
provisional. Opaque input is restricted to message, reasoning, function-call,
function-result, and compaction items. Unknown/provider-hosted item types,
server-side continuation IDs, explicit storage, and arbitrary context management
fail closed. The codec/transport owns wire facts and socket retirement; the
agent alone owns autonomous replacement policy.

Qualified unknown `response.failed` and `response.incomplete` reasons (before
assistant persistence), transient provider `server_error`/internal/unavailable
errors,
body interruptions, codec-annotated provider-frame JSON failures, HTTP 408, and
terminal EOF errors permit at most **eleven stream replacements** per logical
turn. Qualified Connect/ResponseHeaders transport failures (including timeouts)
and all HTTP 5xx with permanent-code vetoes share a separate
**29-replacement opening/admission** allowance. Both share a **35-replacement cumulative cap**; neither transport
fallback nor pre-send network waiting resets these counters. Thus stream-only
failures permit twelve attempts, HTTP-admission-only failures thirty, and mixed
failures at most thirty-six. This covers the pinned local Codex `3d3df0a0`
default count envelope: five WS stream retries followed by HTTP fallback, with
five physical HTTP sends (`request_max_retries=4`, inclusive loop) for each of six
outer HTTP attempts. Octet's flattened host-owned policy is not a reproduction
of Codex's nested scheduling or a promise of identical outcomes for every error
sequence. No hidden HTTP replay is added to `octet-ai`.

Admission retries do not consume the independent stream allowance: twenty HTTP
503 failures followed by an EOF can still recover. The emitted cumulative
`max_attempts` ceiling reflects the remaining allowance for the current failure
class and never exceeds 35; it can change when failure classes change. Arbitrary
local-request JSON, UTF-8, schema validation, resource, pricing, auth, and permanent
request failures do not qualify. Only codec-owned `StreamFailure` wrapping
`Decode::Json` or `Decode::InvalidUtf8` grants provider-stream parse recovery;
raw decode errors do not. Typed `ResponsesFailed` provenance authorizes unknown
qualified terminal errors, not arbitrary `Provider` errors. Explicit permanent
provider codes outrank retry- or context-sounding messages, including policy,
auth, quota, `server_is_overloaded`, and `slow_down` denials. The counter survives re-preparation and WebSocket-to-HTTP fallback;
it resets only on a completed assistant response. The failed stream is dropped
before hooks, compaction, waiting, or opening another attempt. Remote cancellation
and exactly-once provider charges are not promised.

A separately classified, definitely **pre-send** `AiError::NetworkUnavailable`
or credential `AuthError::Unavailable` on the qualified route permits sustained
cancellable waiting, without consuming the finite inference budget. Generic
Connect errors (including unknown DNS/TLS/certificate failures) do not authorize
unbounded waiting. Backoff starts around five seconds, doubles with run-specific
jitter, and is capped at sixty seconds. Valid provider `Retry-After` is never
shortened. `set_max_network_wait(Some(duration))` bounds elapsed outage recovery
for embedding hosts; it does not extend external job/child deadlines. Ordinary
cancellation remains level-triggered and wins same-poll races.

`ProviderRetry` invalidates all provisional text, reasoning, media and tool
presentation owned by the failed attempt. `ProviderWaitingForNetwork` represents
unbounded-count pre-send waiting without a fictional retry denominator. Both
leave the run, session, accepted controls, and children alive. Controls are
received during main-request backoff; replacement re-enters the existing safe
preparation boundary so steering delivery, sticky FinishNow and schema/tool
implementation snapshots remain coherent. Follow-ups retain their ordinary
queue semantics. Retry hooks cannot expand eligibility or budgets;
`ProviderRetryContext.max_attempts` is `None` for pre-send waiting, serialized as
JSON null, and its kind is `waiting_for_network`; qualified inference replacement
uses `interrupted_inference`. `ProviderRetryContext.operation` is the typed
auxiliary operation, or `None` (JSON null) for a main assistant request. Hooks
receive the owning run/resource identity and cannot shorten host/provider delay.
Post-generation `rate_limit_exceeded` is retryable on the qualified route, with
Codex-style "try again in" seconds/milliseconds hints honored; permanent quota,
usage-not-included and request codes remain excluded.

Autonomous local compaction, native compaction, and terminal-gate calls share the
same classification/budget guardrails through an auxiliary helper. Their immutable
session snapshot remains exclusively borrowed until settlement. Recovery emits
`ProviderOperationRetry` with its actual typed operation and route diagnostic,
never a main-answer rollback event. Only successful completed auxiliary calls
reach existing usage/checkpoint commits. Auxiliary drives drain accepted controls
while the immutable request is pending, then deliver at the safe settlement
boundary. A successful terminal gate cannot return ahead of queued input;
FinishNow remains sticky. A serialized final admission/drain boundary includes
controls submitted in the gate's final poll or while `TurnFinished` is yielded:
successful submissions continue the run; submissions after closure return
`RunEnded`. Natural completion uses the same atomic drain/close admission
boundary without holding its mutex across a yielded event. Cancellation has
priority over control/event traffic.
Manual native-compaction calls also use bounded recovery and retry hooks.

Local summary and gate calls expose opening separately from body collection:
outage deadlines cover opening/backoff, never a healthy reconnected response
body. Outage deadlines also preempt pending retry hooks, even when shorter than
the hook's advisory timeout. Backoff longer than the remaining allowance waits
until the actual outage deadline rather than expiring early or shortening
Retry-After. Native compaction uses the AI client's explicit
HTTP-header opening boundary for both manual and autonomous calls: credential
and header opening remain outage-bounded, while a healthy reconnected body uses
its provider body deadlines.
If the outage deadline interrupts opening after possible dispatch, main and
auxiliary paths emit and persist usage uncertainty before returning
`NetworkWaitLimit { usage_unknown: true, .. }`. Credential-only waiting and
pre-send backoff do not invent provider usage. Dispatch is conservatively
tracked at the AI boundary, including opaque host transports; missing headers
never establish nonacceptance. Resumed hard ceilings respect the durable marker.

Failed-attempt usage is **unknown**, not zero. Every observed accepted failure
is durably recorded before replacement, independently of successful usage and
session branches. HTTP 5xx and 408 responses also carry unknown usage: a gateway
failure can hide accepted upstream work. Replay authority is not zero-billing
evidence. Under a hard ceiling these stop after the first response; without a
ceiling their uncertainty remains durable after recovery. Hard cumulative token/cost ceilings fail closed on outstanding
uncertainty, including after resume or later ceiling activation. Child uncertainty
is mirrored before its known usage subtotal. `ProviderUsageUncertain` is emitted
on the first observed uncertainty and at run start for an already-uncertain
session; successful turns do not clear it. Subsequent cost/token numbers are known
subtotals, not complete totals. Context-size estimates still use successful
provider usage, never invented failed-attempt tokens.

Accepted OAuth rotation with indeterminate completion is not autonomously
replayed merely to refresh credentials. Generic Connect/DNS/TLS failures have
only the finite opening allowance, never indefinite outage waiting. Pinned Codex
`codex-api/src/sse/responses.rs:613` skips malformed frame deserialization and
`:489` maps malformed completed-response parsing to a retryable stream error.
Octet replaces the unfinished qualified request within its finite stream budget,
recording unknown usage, rather than continuing an incompletely decoded stream.
Post-parsing field/resource/state-machine validation remains fail-closed.

Deterministic tests include interrupted reasoning/text/provisional tools, durable
mutation ordering, cancellation, exhaustion, hard budgets, permanent/unqualified
routes, steering/FinishNow and operation-scoped terminal-gate recovery. A virtual
clock exercises over fourteen days of pre-send waiting with a constant session
ledger and no finite inference-budget consumption. A loopback WebSocket-to-HTTP
fixture combines an initial interruption, 20,200 credential-unavailable waits,
and recovery on the twelfth accepted request or finite exhaustion. Auxiliary
callsite tests hold more than eight controls, test sticky FinishNow, verify
healthy bodies beyond outage deadlines, and exercise hook Stop/bounded delay.
Manual and autonomous native fixtures separately hold credential opening, HTTP
headers, and healthy response bodies. HTTP 520 sequences/exhaustion and unknown
failed/incomplete terminal partial generations retain the same finite envelopes.
Four terminal EOFs followed by success also retain durable unknown usage. This is not a live-provider
interruption, real-terminal qualification, weeks-long wall-clock soak, or a claim
of complete unattended-runtime parity.

## Effect admission boundary

Every registered `Tool` classifies the exact parsed call through host-owned code. The model cannot provide or lower this classification, and the trait default is `Unknown`. Unknown effects fail closed under every policy, including `UnsafeHost`. Before any hook or tool implementation receives a call, the agent constructs a bounded canonical `EffectIntent` over the principal, run, tool-catalog generation, provider call ID, tool name, effect, arguments, and policy version.

The default `Controlled` policy admits `Pure` and `WorkspaceRead`, requests interactive confirmation for workspace mutation and non-whitelisted `HostProcess` calls, auto-approves a conservative set of known-safe read-only `bash` commands, and otherwise denies host reads/mutations, native processes, network, delegation, executable extensions, and unknown effects. `UnsafeHost` admits classified effects but is not containment. The coding product
selects it by default; `--safe-mode` selects `ControlledBashApproval`. Product code
must additionally prevent executable-extension process startup under controlled
policies because a broker check at later tool invocation cannot contain an
already-running executable.

Workspace-mutation approval creates a random, short-lived capability bound to the canonical intent digest. Tokens are atomically single-use, stored by one-way verifier, redacted in debug output, and never supplied to tools. Dispatch reserves admission before `before_tool_call`, then commits and consumes the exact grant only after all hooks pass and immediately before calling `Tool::execute`. Hook denial or cancellation drops and revokes an uncommitted reservation; cancellation after commit cannot restore it. `after_tool_call` runs only for a committed effect.

Sequential, parallel, and crash-recovery dispatch all use this boundary. Static `ToolConcurrency::Parallel` and `ReplaySafety::Safe` declarations are intersected with the exact host classification: only `Pure` and `WorkspaceRead` calls may run in a parallel batch or be replayed after a crash. A broker or argument denial is returned to the provider as a paired tool error before hooks or executable code; a trusted hook may veto an otherwise admitted call before dispatch.

The broker is a deterministic admission reference monitor, not an OS sandbox. Controlled intentionally denies effect classes that still lack isolation or dedicated brokers, while allowing safe read-only `bash` commands through the `Controlled` process channel; `ControlledBashApproval` (selected by `--safe-mode`) confirms every `bash` call. The default `UnsafeHost` policy lets classified command and process effects use ambient host authority.

`ToolPolicyDecision` is secret-safe host evidence emitted after `ToolStarted` and
before its matching `ToolFinished`. An allowed decision is finalized only after
trusted hooks and reservation commit succeed, not when a reservation is first
created. Stable denials distinguish `secondary_hook_denied`,
`effect_reservation_commit_denied`, and `invalid_tool_arguments`; their
model-visible messages never include hook, parser, broker, argument, or approval
details.

Each decision carries an `EffectiveToolPolicy` snapshot. `effect_policy`,
`workspace_confinement`, `allow_edit`, `allow_write`, `allow_process`,
`allow_shell`, `shell_path`, `bash_timeout_ms`, `max_output_bytes`, and
`allow_remote_read` are `{ value, source }` values. `shell_path.value.selection`
is only one of `configured`, `system_bash`, `path_bash`, `sh_fallback`, or
`unavailable`, derived by the same resolver used for Bash execution; it contains
neither a path nor a digest.

## Sessions

Sessions are append-only JSONL records containing entries, head updates, provider usage, and checkpoints. Entries form a parent-linked tree and the latest durable head selects the active branch. Compaction adds a Pi-structured summary, `first_kept` boundary, active-skill snapshot, and cumulative `readFiles`/`modifiedFiles` details without deleting ancestry. Both product-triggered and autonomous compaction use the same serialized handoff contract.

Before every provider turn, the agent estimates the complete request and retains a fixed 16K output reserve (or a larger explicit reasoning floor). The provider-advertised maximum completion size remains the model ceiling; the individual request is clamped only to the context space remaining after input. The default compaction threshold is the full context window, so the fixed reserve is not combined with an additional percentage buffer. If a provider nevertheless ends at the output limit while emitting tools, the assistant envelope is persisted, every call is paired with a synthetic error without execution, and a corrective continuation asks the model to reissue complete arguments.

Writes use an advisory exclusive lock, compare the observed file length under that lock, append complete record buffers, and call `sync_data` before updating in-memory state. Read-only inspection uses a shared lock and never repairs or truncates. Writable open performs explicit torn-tail recovery while exclusively locked. Files are `0600` on Unix and parsing is bounded by bytes and record count.

## V2 task delegation

For explicit orchestration boundaries (hosted delegation vs local delegated children, sandbox/approval/env/cwd inheritance, extension trust propagation, and explicit non-goals), see [`docs/design/extension-capability-and-orchestration-boundaries.md`](extension-capability-and-orchestration-boundaries.md).

`octet-agent` owns host execution for `AgentDelegation::V2`; the model capability in
`octet-ai` is metadata only. The generic `Agent::enable_v2_delegation` API can
install the native collaboration surface for embedders that explicitly choose
it. The coding product instead enables the manager in extension-only mode: only
the trusted, enabled `octet-subagents` extension receives the owner-bound
`agent_sessions` service, and the root model never receives the parallel native
`spawn_agent`, `followup_task`, `send_message`, `wait_agent`, `list_agents`, or
`interrupt_agent` tools. Available/proactive mode guidance applies only to the
generic API; product orchestration and observation remain extension-owned.

Each child has a stable ID and ancestry path, an isolated append-only `Session`,
and its own agent loop. It inherits the effective root system prompt at spawn
time, approved extension host/tool set, sandbox, model, reasoning and cache
settings, compaction model/policy, completion policy, output modalities, resolved
context/output limits, retry policy, turn limit, optional session token ceiling,
session cost ceiling, and the root's cloned effect broker. A missing root session
token ceiling remains missing in the child; the host does not invent one. Each
child starts a fresh independent context. Its settled usage is mirrored into the
root ledger for accounting and cost-limit checks, never inserted into the
parent's prompt context or charged to the parent's own-context token ceiling. The
broker clone preserves a shared policy/grant store; it is not yet child-specific
authority attenuation. Controlled therefore denies the `Delegation` effect
entirely, while UnsafeHost delegation must be treated as ambient-authority
compatibility mode. Children
can message peers, steer active work, queue messages for an idle worker, receive
follow-up runs, wait without lost notifications, and spawn within the remaining
depth and concurrency bounds.

Each child snapshot, `agent_sessions` spawn/list result, and durable spawn record
also includes its `effective_tool_policy` and bounded
`orchestration_provenance`. The latter records only `parent_inherited` or
`child_override` for sandbox, effect policy, approval authority, environment,
working directory, extension trust, tool scope, and execution limits. An
extension-requested child scope or limit is a host-validated `child_override`;
it does not transfer sandbox, broker, approval, environment, cwd, or trust
control to the extension. No paths, environment values, approval tokens,
extension identifiers, or model arguments are included.

The default team limit is ten concurrent agents including the root, depth two,
and thirty-two total agents during each owning run. Host
validation permits 2–32 concurrent agents, depth 1–8, and at most 256 total,
with total capacity never below concurrent capacity. The first-party
`octet-subagents` service that the coding product actually uses is stricter: its
children sit exactly one level below the root and are bounded to eight active
children per parent with thirty-two retained records per resource owner, and a
worker inherits the parent's full standard tool scope (`read`, `search`,
`edit`, `write`, `bash`) unless the spawn narrows it. A semaphore and ancestry
checks enforce those limits independently of model behavior; an idle worker is
reserved as `Pending` before a follow-up is published so concurrent follow-ups
cannot start overlapping runs. Each worker command channel is capped at 32;
the accepted follow-up backlog is capped at 32 messages and 4,325,376 bytes.
Accepted steering and follow-up reservations remain charged until the child
emits its durable delivery acknowledgement. Direct messages moved into a child
prompt likewise remain reserved until the prompt append succeeds; a failed append
restores them at the front. Failure, interruption, and control backpressure
requeue unacknowledged work in FIFO order rather than releasing or discarding it.
Pending direct messages are capped at 96 and 4,325,376 bytes per child, including
in-flight and prompt-delivery reservations; overflow and inputs above the 128 KiB
durable-text limit are rejected before provenance is written rather than evicting
or truncating accepted work.
Agent mailboxes retain at most 64 messages and 1 MiB; automatic status
notifications evict only the oldest unleased automatic notifications when
necessary and are dropped when no such entry can be evicted. Accepted direct
messages are never evicted, and direct messages to a full root mailbox are
rejected. `wait_agent` leases a UTF-8-safe bounded page and exposes continuation
metadata when one message spans pages. The lease commits only after the complete,
untruncated tool result is durably appended to the owning agent's session;
cancellation, persistence failure, serialization failure, or generic output
truncation restores the page. Concurrent `wait_agent` calls are capped at the
configured total-agent limit and released by cancellation-safe RAII guards.

Delegation state is stored under a descriptor-bound, owner-only random team
directory. `provenance.jsonl` records `team_started`, `agent_spawned`,
`agent_status`, `message`, `interrupt_requested`, and `team_shutdown`; child
session files and the journal are private. Directory allocation, child-session
creation, activation rollback, and cleanup are descriptor-relative and no-follow;
a failed activation removes only its exact empty private team directory and
reports both activation and rollback failures if cleanup cannot complete. Every
spawn, message, follow-up, status transition, and interrupt is appended and
`sync_data`-ed before delivery or visible state mutation. If append or sync
fails, the manager records a visible persistence diagnostic, rejects new work,
and cancels every worker rather than operating without provenance.

Cancellation propagates down ancestry. Every owning run terminal (including
normal completion, failure, max-turn termination, explicit abort, and incomplete
`Run` drop), root `Agent` drop, worker interruption/failure, closed command
channels, and team shutdown cancel worker tokens and send shutdown commands;
descendants are stopped with their parent. A normally driven root terminal also
waits up to two seconds for extension-owned descendants to settle, aggregates
each child session's durable disjoint usage and exact category cost (including
picodollar remainder), and appends one `UsageRecordKind::DelegatedAgent` entry
per child to the root session before its checkpoint. The child remains the
detailed source of truth; the root mirror is the cumulative accounting and cost-
limit ledger.

## Filesystem tools

Workspace-only path shapes reject absolute roots and parent components. On Unix, file operations canonicalize the accepted target and then walk every component using directory descriptors and `O_NOFOLLOW`. Reads open the final object nonblocking, require a regular file from descriptor metadata, and stream at most limit+1 bytes. Mutations retain the open parent descriptor, write a sibling `create_new` temporary, re-read and compare the target immediately before commit, and rename relative to the same descriptor. Parent symlink replacement therefore cannot redirect the operation.

The path guard applies to explicit built-in paths. It is not process containment: commands admitted by UnsafeHost have the current user's authority. When external paths are enabled, local file tools conservatively classify every call as a host effect so a path-resolution race cannot lower admission authority; the coding product forces external paths off under controlled policies, including `--safe-mode`.

## Resource limits

- Local file read/edit/preview: 32 MiB per file.
- Tool calls per assistant turn: 32.
- Default model-visible text per tool result: 50 KiB (host-configurable).
- Delegation provenance text per task/message/status payload: 128 KiB; configurable teams remain capped at 32 concurrent, depth 8, and 256 total agents.
- Progress: bounded messages and chunks.
- Session replay: 256 MiB and 1,000,000 records.
- Command timeout/output: host-configured with product-level upper bounds.

## Extension boundary

All tools implement `Tool` and register through `ExtensionHost`; core tools are not privileged inside the run loop. A product policy filters the host before `Agent::new`, ensuring provider definitions and executable implementations are the same set. Tool implementations own effect metadata and default to `Unknown`; provider schemas and model arguments cannot select authority. Executable extension tools classify as `Extension`, remain non-replayable and sequential, and the coding product prevents their process from starting under Controlled.
