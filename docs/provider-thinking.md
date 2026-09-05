# Provider thinking

Thinking is an endpoint capability, not a property inferred universally from a
model name. These are implementation contracts; synthetic tests do not establish
live-provider availability or acceptance.

## Choices and defaults

- Discovered exact values and defaults survive caches and model selection.
  Missing metadata, unknown metadata and explicit false are distinct. Explicit
  false disables inference from a known route; malformed assertions fail closed.
- Exact sets preserve holes: `low, high` does not imply `medium`. Off is distinct
  from Minimal, and an always-on contract has only On.
- CLI/config and persisted choices may normalize to a supported choice. Core
  generation requests do not silently clamp unsupported values, even in Lossy
  mode. Product labels describe the effective selection.
- Ultra requires explicit support and V2 delegation. The coding product also
  requires its trusted, enabled observing subagents extension. Responses encodes
  supported Ultra as `max`; offline Codex metadata removes dynamic Ultra/V2
  authority without inventing a replacement choice.
- Genuine provider display names are preserved. A missing label is not persisted
  as a fabricated raw-ID label that overrides the built-in display-name registry.

## Wire profiles

| Route/contract | Encoding |
| --- | --- |
| OpenAI-compatible exact effort | Exact advertised `reasoning_effort` value |
| Custom `none/default` binary | Off sends `none`; On omits the effort parameter |
| Always-on model | On sends no reasoning control; explicit core Off is rejected |
| Explicit Qwen enable profile | `enable_thinking` boolean |
| Explicit Qwen chat-template profile | `chat_template_kwargs.enable_thinking`, with the configured `preserve_thinking` setting |
| DeepSeek toggle | Native `thinking.type`; not an unrelated Qwen control |
| OpenRouter | Nested `reasoning` control |
| Together | Typed `reasoning.enabled`, plus effort only when its profile supports it |
| Google native | Native thinking level or token-budget control, according to the selected capability; unsupported Off is not silently omitted |
| Anthropic / Bedrock token thinking | Native enabled thinking and token budget; budget must leave output space for an answer |

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

Unit and loopback tests cover exact choices, controls, caches, stream assembly,
continuation and pre-network rejection. They use synthetic inputs and private
state, not user credentials or live inference. Live-provider demonstrations
remain deferred, not waived. Local focused checks are not complete release qualification. Native compact construction rejects unsupported selections
before resource reservation, and raw compact controls are checked before
credential resolution or network access. Retained opaque-state accounting also
covers replacements without double-counting already buffered signatures.
Final release CI and live-provider acceptance remain separate gates.
