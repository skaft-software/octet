# Responses control foundations

`Capabilities.responses_features` describes model authority;
`RequestRuntime.responses_features` describes endpoint authority.
`Model::responses_features()` intersects them and returns no features for other
protocols. Every feature defaults to false. Model names, Responses Lite, and V2
delegation do not enable async tools, steering, or reasoning updates.

## Async tools

`ToolDef.async_execution` and `ToolCall.async_execution` serialize as `async` and
load older records as false. Responses function/custom definitions and calls
retain the marker. Streams carry it on `ToolCallStart`; assistant-frame hydration
preserves it. Conflicting later provider markers fail rather than changing the
scheduling interpretation after dispatch.

The marker is **not execution authority**. The host still owns approval, dispatch,
job handles, cancellation, waiting and result delivery. Only a qualified,
advertised, schema-valid async call can remain unpaired across canonical
assistant turns. Synchronous calls still require results. Orphan and duplicate
calls/results fail validation. Schema-rejected calls require paired error results.
History normalization does not synthesize results for pending async work or
replace its later real output. Globally unique call IDs are required for
async-qualified Responses history. Other protocols may reuse their per-response
fallback IDs after each synchronous call has a paired result.
Completed async calls with paired results remain replayable on the same qualified
route when a later request removes the tool, disables its async advertisement or
changes its schema. Current definitions gate new/pending work, not completed
history; route qualification and strict identity/order checks still apply.

Opaque replay is still provider state with caller-owned route provenance; it
cannot reconstruct assistant-turn boundaries that were flattened away. Async
markers in opaque replay nevertheless require route/tool authority, valid call
IDs and schemas (or paired error results), and cannot reuse any visible call ID or duplicate results. A result preceding a
visible call with the same ID is rejected. Output-only delta continuations remain
valid when their call belongs to the server-retained prefix. Ordinary canonical
history remains the strict pairing authority.

## Ordered reasoning updates

Insert `ResponsesReplayItem::ConfigurationUpdate(ResponsesConfigurationUpdate {
reasoning: ReasoningConfig::Effort(ReasoningEffort::High)
})` at the chronological boundary before the next user input. The replay encoder
emits `{"type":"configuration_update","reasoning":{"effort":"high"}}`.
`encode_responses_replay` returns `Result<ResponsesInput, AiError>`: provider
`Output`/`Compacted` items cannot author configuration updates. Such items are
rejected during provider decoding, output deserialization and replay conversion,
including `ResponsesOutput::into_input`, rather than gaining host authority.
Explicitly constructed host input updates remain supported.

Keep `Request.reasoning` at the original baseline. Use
`ResponsesInput::effective_reasoning(&baseline)` to read the most recent override;
it never mutates the baseline. `validate_responses_input` checks actual advertised
wire efforts, route authority and adjacency before transport. On/budgets/Ultra
are not wire update efforts. Off is accepted only where `none` is advertised.
Invalid raw items fail rather than being discarded.

Updates cannot accompany automatic context management. Standalone compact
histories require the independently declared
`compact_reasoning_effort_updates` authority; Lite/V2 alone does not qualify it.
A host must persist the baseline and ordered updates, and change the baseline
only when its durable context-window policy explicitly establishes a new one.

## Response boundaries and sampling

`StopReason::Steered` represents native `response.incomplete` with reason
`steered`. It finishes one response; it does not flatten a successor into the
existing `ResponseStream`. Each response keeps its own terminal, usage and cost.
The separate steering transport owns multi-response control and acknowledgement.

Qualified native GPT-6 routes reject sampling/logprob controls with non-Off
effective reasoning, including model-preset overrides. Sol/Luna Off continues to
permit sampling. A third-party model with the same name gets no new authority or
native-route sampling assumptions.

Deterministic coverage lives in `src/protocol/openai_responses_gpt6_tests.rs`.
These fixtures establish wire/validation/replay contracts, not live provider
availability, tool scheduling, or end-to-end steering qualification.
