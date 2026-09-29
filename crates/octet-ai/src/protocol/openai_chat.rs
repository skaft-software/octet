//! OpenAI Chat Completions private wire protocol codec.
//!
//! The Chat Completions endpoint is the widest single surface in the provider
//! layer: one request tree, one response tree, one SSE frame shape, and a
//! compatibility layer that re-parses tool calls out of plain model text when
//! a server ignores `delta.tool_calls`. Each of those has a different failure
//! model, so each lives in its own sibling module and this file is only the
//! seam that names them:
//!
//! - `request` — the private `ChatCompletionsRequest` tree plus `build_request`,
//!   the single place a `Request` becomes HTTP parts. Fails before a byte
//!   leaves the process and needs no response parsing to be exercised.
//! - `response` — the `ChatCompletionsResponse` DTO tree, the completed
//!   response decoder, and the wire-value maps for stop reason and usage.
//! - `stream` — the frame DTOs, single-frame decoding, SSE frame decoding into
//!   `StreamEvent`s, and the shared `ResponseBuilder` segment helpers the
//!   other decoders also call.
//! - `compat` — the marker scanner, the Qwen XML tool-call parser and the JSON
//!   tool-envelope parser: model text that means a tool call but arrived as
//!   content. Isolated because it is the only path where emitted events depend
//!   on text the provider never labelled as a call.
//!
//! `decode_stream_json` stays here because both the response and the stream
//! half instrument it, and its decode counter is what the fixture suite reads
//! to assert that frames are parsed once and not twice.

use serde::Deserialize;

mod compat;
mod request;
mod response;
mod stream;

pub(crate) use request::build_request;
#[cfg(test)]
pub(crate) use response::decode_response;
pub(crate) use response::decode_response_with_tools;
pub(crate) use stream::decode_stream_event;

#[cfg(test)]
thread_local! {
    static CHAT_STREAM_JSON_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn decode_stream_json<'de, T: Deserialize<'de>>(data: &'de str) -> Result<T, serde_json::Error> {
    #[cfg(test)]
    CHAT_STREAM_JSON_DECODES.with(|count| count.set(count.get() + 1));
    serde_json::from_str(data)
}

#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod tests;
