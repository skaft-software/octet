# Context and compaction

[Documentation](README.md) · [Sessions](sessions.md) · [Configuration](configuration.md)

Request compaction at the next safe boundary:

```text
/compact
```

For a handoff instead of more investigation:

```text
/answer Summarize the goal, completed changes, unresolved questions, and tests run.
```

Keep the task's goal and constraints in ordinary conversation or a
[prompt template](instructions.md#prompt-templates); this guide does not introduce
a separate goal store or an undocumented goal command.

## Request budgeting

Before every model turn, octet estimates the complete next provider-visible
request, including instructions, history, and the exact enabled tool schemas.
The generic `threshold_fraction = 1.0` uses the context window with a fixed
16K coding-turn reserve (or the larger advertised reasoning floor), not an
additional percentage buffer. `max_active_tokens` can impose a smaller working
set. The advertised maximum output is still the request ceiling; it is reduced
only when the current input leaves less space in the context window.

The supplied source reference describes authenticated Codex budgeting at Pi's
272K request window while retaining the provider-advertised maximum as discovery
metadata; smaller provider windows remain authoritative. This limits repeated
prompt encoding and moves long sessions to compaction before oversized requests
dominate latency/cost.

Zero or unset `compaction.max_active_tokens` removes only the additional absolute
working-set ceiling, not the Codex route cap. An explicit smaller ceiling remains
a separate setting.

## Settings

In `~/.octet/config.toml`:

```toml
[compaction]
mode = "local" # disabled, local, or native-responses
threshold_fraction = 1.0
# max_active_tokens = 200000 # Optional smaller working set; zero/unset uses model limit.
keep_recent_tokens = 20000
compact_model = "openrouter/anthropic/claude-haiku-4.5"
```

| Setting | Contract |
| --- | --- |
| `mode` | Product default is `local`, not native Responses. `disabled` disables automatic compaction. |
| `threshold_fraction` | Generic default `1.0`; applies alongside any smaller absolute ceiling. |
| `max_active_tokens` | Optional smaller active-context limit. Zero is equivalent to unset; see the Codex reconciliation note above. |
| `keep_recent_tokens` | Documented configuration value `20000`; recent-tail retention is approximately token-bounded. |
| `compact_model` | Optional model used for local summaries; example above is a selection, not a guaranteed available provider. |

Environment controls include `OCTET_COMPACTION_MODE`,
`OCTET_COMPACTION_THRESHOLD_FRACTION`, and `OCTET_COMPACTION_MAX_ACTIVE_TOKENS`.
Legacy `enabled = true` and `OCTET_AUTO_COMPACT=true` still select `local`.
The deprecated `keep_recent_turns` key is retained for old configuration; new
configuration uses `keep_recent_tokens`, not turn-count retention.

## What is retained

Local compaction writes a bounded summary only at a safe completed-turn boundary,
keeps a recent tail and active skill state, and does not rewrite ancestry.
Resume reconstructs context from the selected parent chain and its compaction
boundary. The compact footer uses the latest provider turn's authoritative usage,
not cumulative traffic. See [session records](sessions.md#jsonl-schema) for skill
snapshots and cumulative `details.readFiles` / `details.modifiedFiles`.

`native-responses` instead uses provider-native opaque compaction without showing
the payload in the transcript. It requires the active OpenAI Responses endpoint
and model and never falls back to a Chat/Anthropic summary. Native route-affine
replay is distinct from a process-local WebSocket response ID.
[Transport caveats](providers.md#protocols-and-transport) and
[maintainer compaction contract](design/octet-agent.md#sessions).
