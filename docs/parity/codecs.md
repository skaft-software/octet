# Codec-depth parity detail (rows 1a.2, 1c.1–1c.10, 1e.1–1e.3)

Owner: codec depth worker. Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`. This file owns the exact
upstream/source/test anchors and per-subitem outcomes for the codec rows in
[`README.md`](./README.md). Nothing here is a live-qualification claim.

Scope note: only `crates/octet-ai/src/protocol/**`, `stream.rs`, `types.rs`,
`lib.rs`, `validate.rs`, `error.rs`, `crates/octet-ai/tests/**` and this doc are
edited by this worker. `catalog.rs`, `declarations/` and `discovery.rs` belong
to other workers and are read-only here; several rows are blocked on primitives
that only those files can add.

## Landed

### 1c.1 Strict JSON-schema + grammar/Lark/regex custom tools

- Upstream: `packages/ai/src/api/constrained-sampling.ts` (whole file);
  `openai-completions.ts:1467-1505`; `openai-responses-shared.ts:357-395`;
  `anthropic-messages.ts:1424-1450`; `bedrock-converse-stream.ts:1105-1125`;
  `google-shared.ts:315-370`; `mistral-conversations.ts:749-762`.
- Octet: `crates/octet-ai/src/constrained_sampling.rs` (`make_strict_json_schema`,
  `resolve_json_schema_strict`, `resolve_grammar`, `function_tool_parameters`).
  `ToolDef.constrained_sampling: Option<ConstrainedSampling>` with
  `JsonSchema { strict: prefer|require }` and `Grammar { variants }`.
- Wire behavior implemented per codec:
  - Chat (`protocol/openai_chat.rs`): function tools carry `strict`; grammar
    tools become `type:"custom"` with `custom.format.grammar{syntax,definition}`.
  - Responses (`protocol/openai_responses.rs`): same, `map_responses_tools` is
    now fallible.
  - Anthropic / Bedrock / Google / Mistral: strict rewrites the tool schema;
    Bedrock adds `toolSpec.strict`, Google selects `VALIDATED` function-calling
    mode. Grammar is only emitted where the wire defines a `custom` tool
    (Chat, Responses); elsewhere a `Grammar` request falls back to an ordinary
    function tool, exactly like Pi when `supportsOpenAIGrammarTools` is false.
- `require` that cannot be honored returns
  `AiError::Unsupported(UnsupportedError::ConstrainedSampling(_))` — never a
  silent downgrade.
- Tests: `constrained_sampling::tests::*`,
  `protocol::openai_chat::tests::constrained_sampling_emits_strict_and_grammar_custom_tools`,
  `protocol::openai_chat::tests::required_constrained_sampling_that_cannot_be_honored_is_rejected`,
  `protocol::cross_protocol_tests::constrained_sampling_wire_shape_across_codecs`.
- Deviation: Pi gates strict/grammar on a per-model `compat` record
  (`supportsStrictMode`, `supportsOpenAIGrammarTools`); octet has no such field,
  so strict is preferred everywhere and grammar is preferred on the two OpenAI
  wire families. Adding the compat field needs `catalog.rs`.

### 1c.2 Deferred tools / tool-search / tool_reference

- Upstream: `types.ts:731` (`supportsToolReferences`), `anthropic-messages.ts`
  `splitDeferredTools`.
- Outcome: the unsupported capability is **removed, not faked**. `openai_chat.rs`
  no longer excludes announced `added_tool_names`; `validate.rs` rejects
  `Capabilities::deferred_tool_loading == true`; `types.rs` documents the flag as
  reserved and required false. No code path advertises native deferred loading
  or Anthropic `tool_reference`. Tests:
  `validate::matrix_tests::deferred_tool_loading_is_rejected_instead_of_hiding_schemas`,
  `protocol::openai_chat::tests::unsupported_deferred_tool_loading_rejects_instead_of_hiding_schemas`.

### 1c.3 Anthropic betas (caller merge)

- Upstream: `anthropic-messages.ts:978-1020` (`getBetaFeatures`).
- Landed: a caller-supplied `anthropic-beta` default header is authoritative and
  deduplicated across repeated/comma-joined values. Test:
  `protocol::anthropic::tests::caller_anthropic_beta_list_is_authoritative_and_deduplicated`.
- Blocked: OAuth (`claude-code-20250219`, `oauth-2025-04-20`), fine-grained tool
  streaming, interleaved thinking, and mid-conversation effort betas require an
  Anthropic `compat` record (`supportsMidConvoEffort`, `forceAdaptiveThinking`)
  and OAuth-token detection, neither of which exists in `ModelSpec`/auth without
  editing other workers' files.

### 1c.4 Anthropic refusal fallback blocks

- Upstream: `anthropic-messages.ts:613-620` (fallback block), `1470-1485`
  (refusal stop details).
- Landed: `AnthropicResponseContentBlock::Fallback {}`. Pre-content fallback is
  transparent; mid-output fallback fails closed with
  `UnsupportedError::MidOutputModelFallback`. Tests
  `protocol::anthropic::fixture_tests::pre_content_fallback_is_transparent`,
  `...::mid_output_fallback_is_rejected` with fixtures
  `tests/fixtures/anthropic/fallback_{pre_content,mid_output}.sse`.
- Blocked: fallback-model pricing (`compat.allowedFallbackModels[].cost`) and
  refusal `stop_details.explanation` errorMessage need the Anthropic compat
  record and `AssistantMessage` fields (1c.10).

### 1c.8 Mistral Conversations

- Upstream: `mistral-conversations.ts:196-204, 360-380, 510-525, 893-910`
  (`usesReasoningEffort`, `usesPromptModeReasoning`, `mapReasoningEffort`,
  `reasoning_effort`/`prompt_mode` wire remap).
- Landed: the two FAILURES.md:38 assertions are fixed — the codec classifies the
  base URL (HTTPS / literal-loopback only, no userinfo/query/fragment) and the
  missing-finish case as required. `cargo test -p octet-ai --test mistral_current`
  -> 16 passed.
- Blocked: native `reasoning_effort` / `prompt_mode` emission. `catalog.rs:368`
  rejects any reasoning capability on `Protocol::MistralConversations`, and
  choosing between `reasoning_effort` and `prompt_mode` needs a per-model Mistral
  reasoning profile. Both live in `catalog.rs` (other worker).

## Blocked rows (exact missing primitive)

| Row | Missing primitive | Owning file |
| --- | --- | --- |
| 1a.2 radius/pi-messages | New `Protocol::PiMessages` codec + client dispatch + catalog registration. Upstream `api/pi-messages.ts`: POST `<base>/messages` `{model,context,options}`, SSE `start/text_*/thinking_*/toolcall_*/done/error`, terminal usage, `rewrite` diagnostics, `providerThinkingLevel`, native block-end replacement. Not an OpenAI alias. | client dispatch lives in `client.rs`; registration in `catalog.rs`/`declarations` |
| 1c.5 Bedrock profiles | Profile-ARN region resolution, application-inference-profile, web-identity and bearer-token auth | `auth.rs` (not in this worker's paths) |
| 1c.6 Codex transport | Per-request `sse`/`websocket`/`websocket-cached`/`auto`, connect deadline, debug stats | `responses_ws.rs`, `client.rs` |
| 1c.7 Azure | Deployment map + per-call deployment/base-URL/resource/API-version overrides | `catalog.rs`, `client.rs` |
| 1c.9 xAI Responses | encrypted-reasoning replay plumbing for the Responses shared module | `responses.rs` |
| 1c.10 response metadata | `AssistantMessage.responseModel` / `providerThinkingLevel` / `rawStopReason` / `diagnostics` / `ToolResult.usage` are public `Response` fields consumed across `octet-agent`/`octet-coding-agent`; adding them changes every constructor outside this worker's paths | `types.rs` + downstream crates |
| 1e.1 faux provider | deferred pending/ready/failed/cancelled handles + deferred stop reason | `client.rs`/new module |
| 1e.2 frame encoder/reducer | assistant-message frame encode/reduce + durable partial republish | `stream.rs` (candidate, not yet scoped) |
| 1e.3 image generation | image-generation API, OpenRouter adapter, generated image-model catalog + generator | new module + `catalog.rs` + generator script |

`1e.2` is the most likely next codec-owned row: `stream.rs` holds the
`ResponseBuilder`, which is the natural home for a frame reducer, and no other
worker owns that file.
