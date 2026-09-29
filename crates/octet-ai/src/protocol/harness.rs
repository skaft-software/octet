//! Shared, offline harness for the codec fixture suites.
//!
//! Feeds a captured or hand-authored `.sse` payload through the real
//! [`sse::SseDecoder`] and a codec's `decode_stream_event`, optionally
//! re-chunking the bytes at an arbitrary boundary, then runs the resulting
//! events through [`crate::stream::guard`] so every fixture also exercises
//! the design §8 state-machine invariants (including `PrematureEof`).
//!
//! This lives in its own file because it is shared infrastructure for three
//! codec fixture suites rather than an assertion set of its own: it is the
//! only place in the crate that knows how to drive a stream end to end.

use std::sync::Arc;

use crate::catalog::Model;
use crate::error::AiError;
use crate::pricing::Pricing;
use crate::protocol::sse::{SseDecoder, SseEvent};
use crate::stream::{guard, ResponseBuilder, StreamEvent};
use crate::types::{
    Capabilities, Endpoint, EndpointId, Modality, ModalitySet, ModelId, ModelLimits, ModelSpec,
    Protocol, ReasoningCapability, ReasoningControl, Response, ToolDef,
};

/// A codec's per-event streaming decoder.
pub(crate) type DecodeFn =
    fn(&Model, &SseEvent, &mut ResponseBuilder) -> Result<Vec<StreamEvent>, AiError>;

/// A fully capable model for the given protocol. Decoding never consults
/// capabilities, so a permissive model keeps decode fixtures focused.
pub(crate) fn model(protocol: Protocol, pricing: Option<Pricing>) -> Model {
    let input = ModalitySet::none()
        .with(Modality::Image)
        .with(Modality::Audio);
    let output = ModalitySet::none().with(Modality::Audio);
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("fixture-model".to_string()),
        endpoint: EndpointId("fixture-ep".to_string()),
        api_name: "fixture-api-name".to_string(),
        display_name: None,
        protocol,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities: input,
            output_modalities: output,
            tools: true,
            parallel_tool_calls: true,
            reasoning: Some(ReasoningCapability {
                options: None,
                control: ReasoningControl::Effort,
                exposes_text: true,
                preserves_state: true,
                effort_budgets: None,
                openai_chat_mode: crate::types::OpenAiChatReasoningMode::Standard,
                min_effort: crate::types::ReasoningEffort::Minimal,
                max_effort: crate::types::ReasoningEffort::High,
            }),
            responses_lite: false,
            agent_delegation: None,
            structured_output: true,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window: 200_000,
            max_output_tokens: 8192,
        },
        pricing,
        cache: crate::types::CacheCompatibility::default(),
    };
    let endpoint = Endpoint {
        id: EndpointId("fixture-ep".to_string()),
        base_url: url::Url::parse("https://api.example.test/v1/").unwrap(),
        auth: crate::auth::Auth::none(),
        default_headers: http::HeaderMap::new(),
        transport: crate::types::EndpointTransport::Http,
        runtime: crate::types::RequestRuntime::default(),
        timeout: std::time::Duration::from_secs(30),
    };
    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(endpoint),
    }
}

/// Feed `data` through the SSE decoder + codec in `chunk`-byte slices
/// (`chunk == 0` means one shot), returning the raw codec event sequence and
/// the first error (if any). Events emitted before the error are preserved.
fn drive_raw_configured(
    model: &Model,
    decode: DecodeFn,
    data: &[u8],
    chunk: usize,
    buffer_ambiguous_compatibility_content: bool,
    tool_definitions: Option<&[ToolDef]>,
) -> (Vec<StreamEvent>, Option<AiError>) {
    let mut dec = SseDecoder::new();
    let mut builder = ResponseBuilder::new(
        model.spec.id.clone(),
        model.spec.protocol,
        model.spec.pricing.clone(),
    );
    builder.strict_tool_sampling = crate::protocol::strict_mode_for(model);
    if let Some(tool_definitions) = tool_definitions {
        if let Err(error) = builder.set_tool_definitions(tool_definitions) {
            return (Vec::new(), Some(error));
        }
    }
    builder.set_buffer_ambiguous_compatibility_content(buffer_ambiguous_compatibility_content);
    let mut out = Vec::new();

    let slices: Vec<&[u8]> = if chunk == 0 {
        vec![data]
    } else {
        data.chunks(chunk).collect()
    };
    for slice in slices {
        let sses = match dec.push(slice).map_err(AiError::Decode) {
            Ok(s) => s,
            Err(e) => return (out, Some(e)),
        };
        for sse in sses {
            match decode(model, &sse, &mut builder) {
                Ok(evs) => out.extend(evs),
                Err(e) => return (out, Some(e)),
            }
        }
    }
    match dec.finish().map_err(AiError::Decode) {
        Ok(Some(sse)) => match decode(model, &sse, &mut builder) {
            Ok(evs) => out.extend(evs),
            Err(e) => return (out, Some(e)),
        },
        Ok(None) => {}
        Err(e) => return (out, Some(e)),
    }
    (out, None)
}

pub(crate) fn drive_raw(
    model: &Model,
    decode: DecodeFn,
    data: &[u8],
    chunk: usize,
) -> (Vec<StreamEvent>, Option<AiError>) {
    drive_raw_configured(model, decode, data, chunk, false, None)
}

/// Like [`drive_raw`] but pipes the events through [`guard`], surfacing
/// state-machine violations and `PrematureEof`. Returns the guarded event
/// sequence or the first error encountered by codec or guard.
pub(crate) async fn drive(
    model: &Model,
    decode: DecodeFn,
    data: &[u8],
    chunk: usize,
) -> Result<Vec<StreamEvent>, AiError> {
    let (events, trailing_err) = drive_raw(model, decode, data, chunk);
    collect_guarded(events, trailing_err).await
}

/// Like [`drive`], but installs the immutable request schema snapshot used
/// by production response assembly.
pub(crate) async fn drive_with_tools(
    model: &Model,
    decode: DecodeFn,
    data: &[u8],
    chunk: usize,
    tool_definitions: &[ToolDef],
) -> Result<Vec<StreamEvent>, AiError> {
    let (events, trailing_err) =
        drive_raw_configured(model, decode, data, chunk, false, Some(tool_definitions));
    collect_guarded(events, trailing_err).await
}

/// Drives a codec with ambiguous content-tool compatibility explicitly
/// enabled, mirroring a lossy production request.
pub(crate) async fn drive_with_compatibility_buffering(
    model: &Model,
    decode: DecodeFn,
    data: &[u8],
    chunk: usize,
) -> Result<Vec<StreamEvent>, AiError> {
    let (events, trailing_err) = drive_raw_configured(model, decode, data, chunk, true, None);
    collect_guarded(events, trailing_err).await
}

async fn collect_guarded(
    events: Vec<StreamEvent>,
    trailing_err: Option<AiError>,
) -> Result<Vec<StreamEvent>, AiError> {
    use futures_util::StreamExt;

    let base = futures_util::stream::iter(events.into_iter().map(Ok));
    let mut guarded = if let Some(err) = trailing_err {
        // Preserve the codec error as the stream's terminal item, after its
        // real event prefix, so guard validates the prefix too.
        let tail = futures_util::stream::iter(std::iter::once(Err(err)));
        guard(base.chain(tail))
    } else {
        guard(base)
    };

    let mut collected = Vec::new();
    while let Some(item) = guarded.next().await {
        collected.push(item?);
    }
    Ok(collected)
}

/// Extract the single terminal `Finished` response from a guarded sequence.
pub(crate) fn finished(events: &[StreamEvent]) -> &Response {
    events
        .iter()
        .find_map(|e| match e {
            StreamEvent::Finished(r) => Some(r),
            _ => None,
        })
        .expect("event sequence must contain exactly one Finished")
}
