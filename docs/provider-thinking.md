# Provider thinking

Thinking is an endpoint capability, not a property inferred universally from a
model name. These are implementation contracts; synthetic tests do not establish
live-provider availability or acceptance.

## Choices and defaults

- Discovered exact values and defaults survive caches and model selection.
  Missing metadata, unknown metadata and explicit false are distinct. Explicit
  false disables inference from a known route; malformed assertions fail closed.
  Built-in discovery can supplement an absent contract from the pinned,
  exact provider/model models.dev record; this does not advertise availability
  or enrich custom/Codex inventories. Existing declaration-owned wire profiles,
  not names or booleans, determine which semantic options can be consumed.
- CLI configuration keeps an unset preference distinct from explicit Off until
  model selection. New launches use the endpoint default or its first supported
  enabled choice; absent usable reasoning metadata stays Off without inventing
  wire controls. Explicit choices and resumed-session precedence remain intact.
  Serve catalog defaults follow the same rule. This is an
  [unreleased product fix](providers.md#defaults-unreleased), not a change to
  core `ReasoningConfig::Off` or native-host protocol 1 defaults.
- Exact sets preserve holes: `low, high` does not imply `medium`. Off is distinct
  from Minimal, and an always-on contract has only On.
- CLI/config and persisted choices may normalize to a supported choice. Core
  generation requests do not silently clamp unsupported values, even in Lossy
  mode. Product labels describe the effective selection.
- Host-generated terminal-gate and local-compaction requests prefer Off only
  when the exact model contract accepts it; otherwise they use the advertised
  default. Native Responses compaction preserves the active reasoning selection.
- Ultra requires explicit support and V2 delegation. The coding product also
  requires its trusted, enabled observing subagents extension. The Responses
  codec's standalone Ultra effort mapping is `max`; the agent's V2 runtime
  uses `xhigh` reasoning with delegation rather than that standalone mapping.
  Offline Codex metadata removes dynamic Ultra/V2 authority without inventing
  a replacement choice.
- Genuine provider display names are preserved. A missing label is not persisted
  as a fabricated raw-ID label that overrides the built-in display-name registry.

## Mid-conversation changes (unreleased)

On a Responses route whose **model and endpoint** explicitly qualify reasoning
updates, `/thinking` queues the exact advertised choice without cancelling the
root run or rebuilding its active Agent. The latest pending choice takes effect
at the next response boundary. The UI distinguishes queued from durable
host-selected effort; neither label claims provider acknowledgement or measured
cache reuse. The wire baseline stays pinned while ordered `configuration_update`
items carry subsequent changes. Resume restores the durable effective choice.

Idle changes use the same durable control. Settings persistence failures remain
visible. Explicit interactive choices reject unsupported efforts (including
Codex Off when absent, and Ultra without advertised V2 and live observation)
rather than silently clamping them. Live configuration updates accept ordinary
effort only; Ultra is not a pure effort update. Changing into or out of Ultra/V2
in an already-pinned session requires a new session; rejection preserves both
the current session and the startup preference. Fresh qualified Ultra sessions
remain supported. Startup/config normalization retains its existing policy.
Unqualified, public-compatible, and unknown routes keep the ordinary selector and idle-boundary fallback; names alone grant no controls.
Native async execution and active WebSocket steering are separate capabilities;
public OpenAI qualification does not establish Codex support.

## Wire profiles

| Route/contract | Encoding |
| --- | --- |
| OpenAI-compatible exact effort | Exact advertised `reasoning_effort` value |
| Custom `none/default` binary | Off sends `none`; On omits the effort parameter |
| Always-on model | On sends no reasoning control; explicit core Off is rejected |
| Explicit Qwen enable profile | `enable_thinking` boolean |
| Explicit Qwen chat-template profile | `chat_template_kwargs.enable_thinking`, with the configured `preserve_thinking` setting |
| DeepSeek toggle/effort | Native `thinking.type`, exact advertised effort, and `reasoning_content` replay; not an unrelated Qwen control |
| OpenRouter | Exact enabled effort in nested `reasoning`, or `enabled: true` for a boolean-only contract; Off omits the object (provider default, **not** guaranteed disabled). Mandatory endpoints reject explicit core Off; summaries select their advertised default. |
| Together | Typed `reasoning.enabled`, plus effort only when its profile supports it |
| Google native | Native thinking level or token-budget control, according to the selected capability; unsupported Off is not silently omitted |
| Anthropic / Bedrock token thinking | Native enabled thinking and token budget; budget must leave output space for an answer |

The pinned direct `deepseek-flash` supplement names DeepSeek V4.1 Flash and
preserves Off/low/high/max, without inventing medium, xhigh, or a default. Source
metadata does not universally prove Chat effort, native Messages thinking, or
Google budget support. See the [rich source audit](../crates/octet-ai/models/SOURCES.md).

A custom server's explicit profile does not change the contract of the same
model hosted elsewhere. In particular, Cerebras **`qwen-3.8-27b`** uses
`none/low/medium/high`, defaults to high, accepts the compatible system role, and
replays separated reasoning under `assistant.reasoning`. It does not receive
Qwen-native fields, guessed xhigh effort, or a chat-template override. See the
[catalog source supplement](../crates/octet-ai/models/SOURCES.md).

## Continuation and privacy

Bedrock Converse preserves signed reasoning text and redacted byte blocks for
the same model/protocol. Fragmented signatures remain exact. Redacted stream
chunks are decoded separately before their bytes are combined. A tool result
comes from the supplied result message, not a guessed success response.

Malformed or unsigned Bedrock reasoning blocks fail. Strict mode rejects
incompatible continuation state; Lossy mode drops it with diagnostics rather
than turning opaque state into plaintext. Buffers remain under the existing
aggregate response limit. Opaque replay fields serialize unchanged for durable
continuation, but their Debug representations are redacted.

## Qualification boundary

`cargo test --locked -p octet-coding-agent --test reasoning_defaults` exercises
fresh CLI processes with isolated HOME/workspace/configuration and a loopback
Chat Completions fixture. It checks effective wire controls and durable choices
for binary, exact-effort, always-on, absent and unsupported metadata, all explicit
configuration sources, a real workspace `read` tool round trip, and restart/resume.
It does not run a real model or establish live LM Studio/OpenRouter acceptance.

Unit and loopback tests cover exact choices, controls, caches, stream assembly,
continuation and pre-network rejection. They use synthetic inputs and private
state, not user credentials or live inference. Live-provider demonstrations are
optional and recorded separately for each release; these synthetic checks do not
establish them. Local focused checks are not complete release qualification.
Native compact construction rejects unsupported
selections before resource reservation, and raw compact controls are checked
before credential resolution or network access. Retained opaque-state accounting
also covers replacements without double-counting already buffered signatures.
Final release CI remains required; live-provider acceptance is a separate opt-in
check, not a release prerequisite.
