# Context and compaction

[Documentation](README.md) · [Sessions](sessions.md) · [Configuration](configuration.md)

```text
/compact
```

For a handoff instead of more investigation:

```text
/answer Summarize the goal, completed changes, unresolved questions, and tests run.
```

Keep the goal and constraints in the conversation or a [prompt
template](instructions.md#prompt-templates). There's no separate goal store or
goal command.

## Request budgeting

Before every model turn, octet estimates the whole next request: instructions,
history and the exact enabled tool schemas. With the default
`threshold_fraction = 1.0`, the limit is the context window minus a fixed 16K
reserve for the turn (or the model's larger advertised reasoning floor). There's
no extra percentage buffer. `max_active_tokens` sets a smaller working set if
you want one. The model's advertised maximum output is still the request
ceiling, and it only shrinks when the input leaves less room in the window. When
it does shrink, octet keeps a small headroom (1% of the window, clamped to 256
to 4096 tokens), because the provider counts tokens its own way.

If a provider rejects a request for size, octet recovers where it can. Local
compaction runs at the next reducible boundary and the request is retried,
bounded like other provider retries. Named policy, authorization, quota and
rate-limit rejections still fail without compacting, and a session with no
reducible history reports the limit instead of retrying.

<details>
<summary>Why the headroom, and what counts as a size rejection</summary>

The input count is an estimate. The provider counts with its own tokenizer and
chat template, so without a reserve a request sits exactly on the boundary,
where a one-token difference is a hard rejection. A real vLLM server answered a
131072-token window with "you requested 30896 output tokens and your prompt
contains at least 100177 input tokens, for a total of at least 131073 tokens".

Size recovery includes strict servers that answer HTTP 400, 413 or 422 with a
provider-shaped body carrying a numeric `code`, which used to take the
permanent-failure branch. Recovery stays bounded: if the provider's own count
exceeds the estimate by more than the reserve, the request fails with the
provider's message.

</details>

<details>
<summary>Codex budgeting</summary>

The authenticated Codex working-window policy caps most models, including Astra,
at 272K request tokens, and keeps larger provider-advertised maximums as
discovery metadata. GPT-5.6 Luna is the model-specific exception and may use up
to a 372K working window. Smaller discovered provider windows still win. This
limits repeated prompt encoding and moves long sessions to compaction before
oversized requests dominate latency and cost. A zero or unset
`compaction.max_active_tokens` removes only the extra working-set ceiling, not
the model-specific Codex route cap. A smaller explicit ceiling stays a separate
setting.

</details>

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

| Setting | What it does |
| --- | --- |
| `mode` | `local` by default, not native Responses. `disabled` turns off automatic compaction. |
| `threshold_fraction` | Default `1.0`. Applies alongside any smaller absolute ceiling. |
| `max_active_tokens` | Optional smaller active-context limit. Zero is the same as unset. |
| `keep_recent_tokens` | `20000` in the example. The recent tail is kept to roughly that many tokens. |
| `compact_model` | Optional model for local summaries. The example is a choice, not a guaranteed available provider. |

Environment variables: `OCTET_COMPACTION_MODE`,
`OCTET_COMPACTION_THRESHOLD_FRACTION` and `OCTET_COMPACTION_MAX_ACTIVE_TOKENS`.
The old `enabled = true` and `OCTET_AUTO_COMPACT=true` still select `local`. The
deprecated `keep_recent_turns` key still loads from old config. New config uses
`keep_recent_tokens`.

The footer percentage uses the full model window, not a smaller configured
working set. For example, `max_active_tokens = 120000` is a 9.15% ceiling on a
1,310,720-token model, and the coding-turn reserve makes the input trigger lower
still. With no cap and the default fraction, that model's threshold is about
98.75%. A process-local `/auto-compact` override or provider context-overflow
recovery can also trigger compaction earlier, so check the active setting and
the compaction reason before blaming the model for a low percentage.

## What is retained

Local compaction writes a bounded summary only at a safe, completed-turn
boundary. It keeps a recent tail and the active skill state, and doesn't rewrite
history. An empty, whitespace-only or over-128KiB handoff (including the
host-derived file footer) fails closed before a checkpoint is written, and octet
never truncates a summary or file evidence. Resume rebuilds context from the
selected parent chain and its compaction boundary. The compact footer shows the
latest provider turn's usage, not cumulative traffic. [Session
records](sessions.md#jsonl-schema) cover skill snapshots and the cumulative
`details.readFiles` and `details.modifiedFiles`.

`native-responses` uses the provider's opaque compaction instead, without
showing the payload in the transcript. It needs the active OpenAI Responses
endpoint and model, and never falls back to a Chat or Anthropic summary. Native
route-affine replay is different from a process-local WebSocket response ID. See
[transport caveats](providers.md#protocols-and-transport) and the [compaction
design](design/octet-agent.md#sessions).

<details>
<summary>Responses replay, the tool-schema budget and snap-compact</summary>

- When a normal Responses turn has no complete same-route output sidecar (older
  sessions, or a completed response without terminal `output`), octet replays
  the canonical conversation instead and sends the current effective reasoning
  effort as the request baseline. It doesn't replay opaque reasoning updates
  without their complete history. Ordinary Responses model switches keep the
  canonical conversation but can't replay opaque output or reasoning updates
  from the previous route. `native-responses` mode still needs complete
  same-route replay, and refuses such a switch until a valid local replay
  boundary exists.
- Rust embedders may call `Agent::set_tool_schema_budget_bytes`. The default is
  128KiB of exact serialized provider-visible tool-definition JSON. A non-empty
  schema set over the limit is refused before provider I/O, rather than having
  tools omitted or rewritten. A zero budget permits only an empty tool set.
- The local [octet-snap-compact
  extension](../extensions/octet-snap-compact/README.md) can replace the
  parent-model summary call with deterministic PNG frames, when you enable it
  and the active model accepts images. Its checkpoint keeps the source text for
  later re-compaction, but a vision model's context gets the frames instead of
  that text. Text-only routes use the normal summarizer until a bitmap
  checkpoint exists, and continuing from such a checkpoint requires switching
  back to a vision route. Rendering is capped at 120 seconds across all source
  chunks and frame validation. A timeout, a cancel or an image context limit
  fails closed without discarding history. Successive source sections keep
  explicit boundaries. It doesn't change `native-responses` mode.

</details>
