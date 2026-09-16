# octet-agent

Stateful agent loop with tool execution and event streaming for octet.

`octet-agent` sits above [`octet-ai`](../octet-ai): it reconstructs provider
requests from a persistent, branchable JSONL session, drives the model
stream, executes tool calls through a small extension boundary, persists
every semantic boundary (complete messages and individual tool results —
never streaming deltas), and emits a streaming event surface including
`OutputDelta`, completed `OutputMedia`, batched `SteeringDelivered`, tool
lifecycle events, `TurnFinished`, and `RunFinished` to the caller.

Included:

- Typed `UserInput` / `InputPart` boundary for `prompt`,
  `prompt_without_tools`, `steer`, `finish_now`, and `follow_up`: ordered text
  and media parts (`octet_ai::Media`) pass through the agent to the model
  unchanged; text-only callers remain compatible via
  `From<String>` / `From<&str>`.
- Configurable generated output modalities. Completed clips arrive as
  `AgentEvent::OutputMedia`; `Agent::complete` retains committed clips in
  `RunOutput::media` and removes output from retried or rejected attempts.
- Five built-in tools — `read`, `search`, `edit`, `write`, `bash` — registered through
  the same `Extension` boundary available to third-party tools.
- A concrete `SandboxConfig`: relative paths use the workspace and hosts may
  enable trusted-local absolute/`~/`/external paths, or opt into a workspace-only
  descriptor-bound workspace guard. It also provides mutation and unified
  command-execution gates, an execution timeout, output-byte limits, and
  process-group cleanup for cancelled child processes
  (`bash` is unix-only in v0.1 — it fails clearly rather than weakening cleanup
  on other platforms). Neither path mode is an OS sandbox: spawned processes
  run with the current user's full access. octet is a trusted local agent — see
  the repository-root `SECURITY.md`.
- `Run` + clonable `RunControl` with steering, follow-up, answer-now, and
  abort controls — built for `tokio::select!` alongside user input.
- Session checkout/branching, manual compaction, locked and synced writes, and
  torn-tail crash recovery. Read-only tools may explicitly opt into replay;
  unresolved mutating calls become durable indeterminate errors and are never
  silently repeated after an unclean crash.

See the [agent design](https://github.com/skaft-software/octet/blob/main/docs/design/octet-agent.md)
and the crate-level Rust documentation for the public API.

Tools dispatched by `Agent` receive `ToolContext::invocation()` for durable named
replay memos and bounded partial-output checkpoints. These auxiliary records use
the same private JSONL descriptor and synced mutation line as the transcript;
paired-result persistence fences and removes their live state. A journal-wide
settled-identity index prevents checkout or reopen from reissuing the same
assistant/source-index invocation; a new assistant entry is a new identity.
Only executable calls/current read waves allocate live slots, never static
per-turn-cap refusals. Hosts opt in with
`Agent::enable_session_partial_output_checkpoints("bash", interval)`; snapshots
are interval-bounded. Progress never proves completion, and interrupted
unsafe calls retain the last snapshot with an explicit unknown-outcome marker.
The standalone `DurableInvocationStore::new()` remains in-memory only. Parallel
outcomes waiting for ordered placement are not yet independently durable.

Tool-free local summaries retry transient interruptions on ordinary routes with
bounded summary backoff; retries remain distinct from a failed compaction and
preserve usage uncertainty. Qualified Codex recovery keeps its existing envelope.
Manual compaction and branch-summary inference share the public
`summarize_with_retry` / `summarize_branch_with_retry` APIs. Hosts commit returned
text once and forward their retry events; these APIs already persist billable
usage. Accepted auxiliary work stays guarded until durable billing settlement,
including cancellation during the successful response poll. An append failure
falls back to uncertainty; if storage remains unavailable, the live owner stays
budget-closed but cannot promise recovery after process exit.
Deferred-provider polling still lacks an AI transport consumer.

Hard token/cost ceilings require the actual codec-enforced output cap. Codex,
cap-omitting model presets and native Responses compact fail before dispatch
with `AgentError::OutputLimitUnavailable` under either ceiling. Declared model
limits alone are not enforceable wire bounds. Existing unknown/unpriced exposure
retains its more specific refusal. Unbounded operation remains available when
no hard ceiling is configured.

`TurnFinished.turn_cost` is the exact optional cost of the persisted assistant
response, including its total picodollar remainder. It excludes gate/summary,
child and earlier-turn charges; `None` stays unknown, never catalog-repriced zero.
