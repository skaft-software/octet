# Additive Pi codec/stream execution receipt

Reference inspected read-only: `/Users/achumukundan/github/earendil-works/pi`
HEAD `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`. No TS vendoring,
reference edits, commits, or credential-policy changes.

## Progress

- Read full Pi AI README and `api/pi-messages.ts`, octet parity ledger, AI
  README/design and provider docs. Source/test inspection continues.
- Observed `AI_PATHS_RELEASED` in `EXECUTION-providers.md` before any AI edit.
  Ownership now `crates/octet-ai/**` except discovery source/test; preserved
  existing Mistral URL/EOF fixes and discovery exports.
- Radius is **not** an OpenAI alias. Upstream uses POST `/messages` with
  `{model,context,options}` and native Pi assistant SSE events (including
  authoritative block-end replacement, terminal usage, rewrite diagnostics).
  Missing codec/runtime is release-blocking; do not register it as Chat.

This is incremental, not a completion/parity claim. Final per-row outcomes and
checks will be recorded in `docs/parity/codecs.md` and below.

## 2025 — adopted partial work (codecs2) + Mistral 1c.8 gate

- `git diff --stat -- crates/octet-ai`: 16 files; crate compiles clean
  (`cargo check -p octet-ai` -> Finished, no warnings).
- Adopted partial: `constrained_sampling.rs` module + `ToolDef.constrained_sampling`
  field; deferred-tool-loading removal in `openai_chat.rs` (now reject, not hide);
  Anthropic caller-beta normalization; Mistral HTTPS/loopback base-URL guard.
- Ran `cargo test -p octet-ai --test mistral_current`: **16 passed; 0 failed**.
  The two FAILURES.md:38 assertions
  (`invalid_destinations_reject_before_credentials_without_echoing_url_secrets`,
  `eof_is_not_native_done_even_after_valid_arguments`) now pass — codec base-URL
  classification + missing-finish classification are correct.

## 1c.1 constrained sampling — strict JSON-schema + grammar custom tools

- Wired `ToolDef.constrained_sampling` into all six codec families via new
  `constrained_sampling::function_tool_parameters` + `resolve_grammar`.
  - `protocol/openai_chat.rs`: function tools carry `strict`; grammar tools emit
    `type:"custom"` with `custom.format.grammar{syntax,definition}`.
  - `protocol/openai_responses.rs`: `map_responses_tools` now `Result`; function
    tools carry `strict`; grammar tools emit Responses `custom` tools.
  - `protocol/anthropic.rs`, `protocol/bedrock.rs` (adds `toolSpec.strict`),
    `protocol/google.rs` (adds `VALIDATED` function-calling mode),
    `protocol/mistral_conversations.rs`: strict rewrites the tool schema.
- Prefer/require semantics: `require` that cannot be honored returns
  `AiError::Unsupported(UnsupportedError::ConstrainedSampling(..))` — never a
  silent downgrade.
- Tests added: `protocol::openai_chat::tests::constrained_sampling_emits_strict_and_grammar_custom_tools`,
  `...::required_constrained_sampling_that_cannot_be_honored_is_rejected`,
  `protocol::cross_protocol_tests::constrained_sampling_wire_shape_across_codecs`,
  fixed `constrained_sampling::tests::strict_schema_closes_objects_and_nullifies_optional_properties`.
- Command: `cargo test -p octet-ai --lib constrained_sampling` -> 6 passed;
  `cargo test -p octet-ai --lib constrained_sampling_wire_shape` -> 1 passed.

## 1c.2 / 1c.3 / 1c.8 status

- 1c.2 deferred tools: unsupported claim removed end-to-end. `openai_chat.rs`
  no longer excludes announced `added_tool_names`; `validate.rs` rejects
  `Capabilities::deferred_tool_loading == true`; `types.rs` docs state the flag
  must be false. New test:
  `validate::matrix_tests::deferred_tool_loading_is_rejected_instead_of_hiding_schemas`.
  Anthropic `tool_reference`/tool-search emit paths are NOT implemented and are
  NOT claimed (no code path advertises them). `cargo test -p octet-ai --lib
  deferred_tool_loading` -> 3 passed.
- 1c.3 Anthropic betas: caller `anthropic-beta` list is authoritative +
  deduplicated across repeated/comma-joined header values. Test:
  `protocol::anthropic::tests::caller_anthropic_beta_list_is_authoritative_and_deduplicated`
  -> ok. OAuth/fine-grained/interleaved betas and mid-conversation effort are
  BLOCKED: they require an Anthropic `compat` profile (`supportsMidConvoEffort`,
  `supportsToolReferences`, `forceAdaptiveThinking`) that octet's `ModelSpec`
  does not carry; adding it needs `catalog.rs` (owned by another worker).
- 1c.8 Mistral: the two FAILURES.md:38 assertions are fixed (see top). Native
  `reasoning_effort`/`prompt_mode` emission is BLOCKED: `catalog.rs:368` rejects
  ANY reasoning capability on `Protocol::MistralConversations`
  ("This codec does not yet map native reasoning controls or content"), so no
  model can enable it without editing `catalog.rs` (owned by another worker) plus
  a Mistral reasoning wire-profile primitive.

## 1c.4 Anthropic refusal fallback blocks (partial, bounded)

- `AnthropicResponseContentBlock` gained a `Fallback {}` variant. A `fallback`
  block before any content is transparent (skipped, marker carries no content);
  a fallback after content already streamed fails closed with the new
  `UnsupportedError::MidOutputModelFallback`.
- Fixtures: `tests/fixtures/anthropic/fallback_pre_content.sse`,
  `fallback_mid_output.sse`. Tests
  `protocol::anthropic::fixture_tests::pre_content_fallback_is_transparent` and
  `...::mid_output_fallback_is_rejected`.
- Command `cargo test -p octet-ai --lib fallback` -> 3 passed (incl. unrelated
  proxy test).
- NOT done (blocked): fallback-model *pricing* (`allowedFallbackModels[].cost`)
  needs an `AnthropicCompat.allowedFallbackModels` profile on `ModelSpec`; and
  `message_start.input_transformations` / `responseModel` capture need the
  `AssistantMessage.responseModel` field from row 1c.10. Both require
  `catalog.rs` + public `Response` API changes outside a codec-only edit.

## CHANGELOG-ready bullets

- Add strict JSON-schema and grammar/Lark/regex constrained sampling for tools
  across the Chat, Responses, Anthropic, Bedrock, Google and Mistral codecs;
  `require` that cannot be honored fails closed.
- Stop advertising unsupported deferred tool loading: announced `added_tool_names`
  no longer hide schemas, and `deferred_tool_loading = true` is rejected.
- Anthropic `anthropic-beta` caller lists are authoritative and deduplicated.
- Reject unsupported Anthropic mid-output model fallbacks; transparent pre-content
  fallbacks are skipped.

## Formatting + final verification

- Ran `rustfmt --edition 2021` ONLY on this worker's exclusive files (listed
  below). Note: the base commit is not rustfmt-clean (`cargo fmt --all --check`
  flags 287 workspace files), so reformatting these files reflowed pre-existing
  code in some cases (largest: `protocol/mistral_conversations.rs`,
  `tests/mistral_current.rs`). Behavior-neutral and confined to this worker's
  paths; `rustfmt --check` on those files now exits 0.
- `cargo test -p octet-ai`: lib 333 passed; bedrock_current 5; client_batch 3;
  client_compact 18; client_complete 1; client_stream 37; coverage_manifest 3;
  google_current 2; mistral_current 16; provider_discovery 4; public_api 1;
  smoke 1; doc 1. **0 failed.**
- `cargo check -p octet-agent`: clean (Exit 0).
- Pre-existing breakage OUTSIDE this worker's paths (recorded, not fixed):
  `cargo check --workspace` fails in `octet-coding-agent`
  (`modes/interactive.rs` non-exhaustive matches for `InputAction::FocusGained`/
  `FocusLost` and `commands::Command::Fast`). Unrelated to `octet-ai`.
- Files touched by this worker: `src/constrained_sampling.rs`, `src/error.rs`,
  `src/lib.rs`, `src/stream.rs`, `src/types.rs`, `src/validate.rs`,
  `src/protocol/{anthropic,bedrock,google,mistral_conversations,openai_chat,openai_responses,cross_protocol_tests}.rs`,
  `tests/*.rs`, `tests/fixtures/anthropic/fallback_*.sse`,
  `docs/parity/codecs.md`, this file.
- Generator scripts: none run — no generated artifact (`models/*.json`) was
  edited or regenerated by this worker.

## Rows landed vs blocked

Landed: 1c.1 (six codec families), 1c.2 (claim removed), 1c.3 caller-beta merge,
1c.4 fallback blocks, 1c.8 failing-assertion fixes.
Blocked (exact primitive named in `docs/parity/codecs.md`): 1a.2 radius/pi-messages
(client dispatch + catalog registration owned elsewhere), 1c.3 OAuth/
fine-grained/interleaved/mid-convo betas (Anthropic compat record), 1c.4
fallback pricing (compat record), 1c.5 (auth.rs), 1c.6 (responses_ws/client),
1c.7 (catalog/client), 1c.8 reasoning_effort (catalog.rs:368 + Mistral reasoning
profile), 1c.9 (responses.rs), 1c.10 (public Response fields across downstream
crates), 1e.1 (client.rs), 1e.3 (new module + catalog + generator). 1e.2
(stream.rs frame reducer) is not yet scoped but is the next codec-owned row.

## 2025 — ownership widened (all crates/octet-ai/** except declarations/** + catalog.rs)

Priority queue: 1e.2 -> 1c.9 -> 1c.6 -> 1c.5 -> 1c.10 -> 1e.1 -> 1c.7 -> 1e.3.
1a.2 radius: implement codec+dispatch, record one-line catalog registration, do
not edit catalog.rs. No workspace-wide rustfmt.

## 1e.2 assistant message frame encoder/reducer (LANDED)

- New module `crates/octet-ai/src/assistant_frame.rs` mirroring Pi
  `packages/ai/src/utils/assistant-message-frame.ts`:
  `AssistantMessageFrame` (serde enum), `AssistantMessageFrameEncoder`
  (StreamEvent -> frames, fail-closed on delta-before-start/block-reuse/
  event-after-terminal), `reduce_assistant_message_frames` (rebuild partial
  AssistantMessage from a frame prefix; None when no Start).
- Public API re-exported from `lib.rs`; added to `tests/public_api.rs` import list.
- Durability is the serde round trip: frames serialize/deserialize and reduce to
  the same partial message. Terminal (`Finished`/`Usage`/`ProviderLifecycle`) emit
  no frame, so a frame sequence is partial progress only.
- Command: `cargo test -p octet-ai --lib assistant_frame` -> 5 passed
  (`frames_round_trip_through_serde_and_reduce_to_the_partial_message`,
  `truncated_prefix_reduces_to_partial_progress`,
  `empty_before_start_is_not_a_message`, `delta_before_block_start_is_rejected`,
  `delta_after_block_end_is_rejected_by_the_reducer`).
- Not done (other crate): the agent-harness durable republish (persist frames
  between deltas and republish the partial) lives in octet-agent, outside
  crates/octet-ai.
