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
only when the current input leaves less space in the context window, and that
reduction reserves a small bounded headroom (1% of the window, clamped to
256–4096 tokens). The input count is an estimate, while the provider counts with
its own tokenizer and chat template; without the reserve a request sits exactly on
the boundary, where a one-token difference is a hard rejection. A real vLLM
server answered a 131072-token window with *"you requested 30896 output tokens and
your prompt contains at least 100177 input tokens, for a total of at least 131073
tokens"*.

A provider that rejects the request for size is recovered rather than surfaced
where it can be: local compaction runs at the next reducible boundary and the
request is retried, bounded like other provider retries. That includes strict
servers that answer HTTP 400/413/422 with a provider-shaped body carrying a
numeric `code`, which previously selected the permanent-failure branch. Named
policy, authorization, quota and rate-limit rejections still fail without
compacting, and a session with no reducible history reports the limit instead of
retrying. Recovery remains bounded: if the provider's own count exceeds the
estimate by more than the reserve, the request fails with the provider's message.

The authenticated Codex working-window policy caps most models, including Astra,
at 272K request tokens while retaining larger provider-advertised maxima as
discovery metadata. GPT-5.6 Luna is the model-specific exception and may use up
to a 372K working window; smaller discovered provider windows remain
authoritative. This limits repeated prompt encoding and moves long sessions to
compaction before oversized requests dominate latency/cost.

Zero or unset `compaction.max_active_tokens` removes only the additional absolute
working-set ceiling, not the model-specific Codex route cap. An explicit
smaller ceiling remains a separate setting.

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
The footer percentage uses the full model window, not a smaller configured
working set. For example, `max_active_tokens = 120000` is a 9.15% ceiling on a
1,310,720-token model; the coding-turn reserve makes the input trigger lower
still. With no cap and the default fraction, that model's threshold is about
98.75%. A process-local `/auto-compact` override or provider context-overflow
recovery can also trigger earlier compaction; inspect the active setting and
compaction reason before attributing a low percentage to the model.
The deprecated `keep_recent_turns` key is retained for old configuration; new
configuration uses `keep_recent_tokens`, not turn-count retention.

## What is retained

Local compaction writes a bounded summary only at a safe completed-turn boundary,
keeps a recent tail and active skill state, and does not rewrite ancestry. Empty,
whitespace-only, or over-128KiB local handoffs (including the host-derived file
footer) fail closed before a checkpoint is written; octet never truncates a
summary or file evidence. Resume reconstructs context from the selected parent
chain and its compaction boundary. The compact footer uses the latest provider
turn's authoritative usage, not cumulative traffic. See [session records](sessions.md#jsonl-schema)
for skill snapshots and cumulative `details.readFiles` / `details.modifiedFiles`.

Rust embedders may set `Agent::set_tool_schema_budget_bytes`; the default is
128KiB of exact serialized provider-visible tool-definition JSON. A non-empty
schema set over that limit is refused before provider I/O rather than having
individual tools omitted or rewritten. A zero budget permits only an empty tool
set.

`native-responses` instead uses provider-native opaque compaction without showing
the payload in the transcript. It requires the active OpenAI Responses endpoint
and model and never falls back to a Chat/Anthropic summary. Native route-affine
replay is distinct from a process-local WebSocket response ID.
[Transport caveats](providers.md#protocols-and-transport) and
[maintainer compaction contract](design/octet-agent.md#sessions).
