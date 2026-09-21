//! Shared OpenAI custom-tool input framing. Raw grammar text is never parsed as
//! JSON: it occupies the tool schema's single required string property. Both the
//! raw and JSON-escaped buffers remain subject to the ordinary response bounds.

use crate::constrained_sampling::resolve_grammar;
use crate::error::{AiError, DecodeError};
use crate::protocol::emit_event;
use crate::stream::{ResponseBuilder, StreamEvent, MAX_TOOL_ARGUMENT_BYTES};
use crate::types::ToolDef;

fn invalid(detail: &str) -> AiError {
    DecodeError::InvalidProviderField(format!("custom tool input {detail}")).into()
}

pub(super) fn input_property(
    tools: &[ToolDef],
    name: &str,
    supports_grammar: bool,
) -> Result<Option<String>, AiError> {
    if !supports_grammar {
        return Ok(None);
    }
    tools
        .iter()
        .find(|tool| tool.name == name)
        .map(|tool| resolve_grammar(tool, true))
        .transpose()
        .map(|grammar| grammar.flatten().map(|grammar| grammar.input_property))
}

pub(super) fn replay_input(arguments: &str, property: &str) -> Result<String, AiError> {
    let arguments: serde_json::Value =
        serde_json::from_str(arguments).map_err(|_| invalid("is not a JSON object"))?;
    arguments
        .get(property)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid("requires its declared string property"))
}

fn key(index: usize) -> String {
    format!("custom_input_{index}")
}

pub(super) fn is_open(builder: &ResponseBuilder, index: usize) -> bool {
    builder.temp_buffers.contains_key(&key(index))
}

pub(super) fn start(builder: &mut ResponseBuilder, index: usize) -> Result<(), AiError> {
    if builder.ended_indices.contains(&index) || is_open(builder, index) {
        return Err(invalid("started more than once"));
    }
    let property = resolve_property(builder, index)?;
    builder.replace_temp_buffer(property_key(index), property)?;
    builder.replace_temp_buffer(key(index), String::new())
}

fn property_key(index: usize) -> String {
    format!("custom_property_{index}")
}

fn property(builder: &ResponseBuilder, index: usize) -> Result<&str, AiError> {
    builder
        .temp_buffers
        .get(&property_key(index))
        .map(String::as_str)
        .ok_or_else(|| invalid("arrived before its call"))
}

fn resolve_property(builder: &ResponseBuilder, index: usize) -> Result<String, AiError> {
    let call = builder
        .tool_call_builders
        .get(&index)
        .ok_or_else(|| invalid("arrived before its call"))?;
    Ok(input_property(
        builder.tool_definitions.as_deref().unwrap_or_default(),
        &call.name,
        // A provider `custom` call is already framed as grammar input by the
        // wire itself; the route declaration gates whether the request may
        // *declare* one, which the emission/replay paths enforce.
        true,
    )?
    .unwrap_or_else(|| "input".to_owned()))
}

pub(super) fn delta(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    index: usize,
    text: &str,
) -> Result<(), AiError> {
    if !is_open(builder, index) || builder.ended_indices.contains(&index) {
        return Err(invalid("delta arrived outside an open custom call"));
    }
    builder.append_temp_buffer_bounded(key(index), text, MAX_TOOL_ARGUMENT_BYTES)?;
    let quoted = serde_json::to_string(text).expect("a string is serializable");
    let prefix = if builder.tool_call_builders[&index].arguments_json.is_empty() {
        format!(
            "{{{}:\"",
            serde_json::to_string(property(builder, index)?).expect("a string is serializable")
        )
    } else {
        String::new()
    };
    let encoded = format!("{prefix}{}", &quoted[1..quoted.len() - 1]);
    if !encoded.is_empty() {
        emit_event(
            events,
            builder,
            StreamEvent::ToolCallArgsDelta {
                index,
                delta: encoded,
            },
        )?;
    }
    Ok(())
}

pub(super) fn finish(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    index: usize,
    complete: Option<&str>,
) -> Result<(), AiError> {
    if builder.ended_indices.contains(&index) {
        if let Some(complete) = complete {
            let prior = replay_input(
                &builder.tool_call_builders[&index].arguments_json,
                property(builder, index)?,
            )?;
            if prior != complete {
                return Err(invalid("changed after closure"));
            }
        }
        return Ok(());
    }
    let raw = builder
        .temp_buffers
        .get(&key(index))
        .ok_or_else(|| invalid("terminal arrived outside a custom call"))?;
    let suffix = match complete {
        Some(complete) => complete
            .strip_prefix(raw.as_str())
            .ok_or_else(|| invalid("changed non-monotonically"))?,
        None => "",
    }
    .to_owned();
    // An empty custom input still needs the JSON object/string prefix.
    delta(events, builder, index, &suffix)?;
    emit_event(
        events,
        builder,
        StreamEvent::ToolCallArgsDelta {
            index,
            delta: "\"}".to_owned(),
        },
    )?;
    builder.take_temp_buffer(&key(index));
    emit_event(
        events,
        builder,
        StreamEvent::ToolCallEnd {
            index,
            argument_error: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ConstrainedSampling, GrammarVariants, ModelId, Protocol, ToolCallId};

    #[test]
    fn custom_property_is_resolved_once_and_preserves_fragment_escaping() {
        let mut builder =
            ResponseBuilder::new(ModelId("test".to_owned()), Protocol::OpenAiResponses, None);
        builder.tool_definitions = Some(vec![ToolDef {
            name: "custom".to_owned(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object", "required": ["source"],
                "properties": {"source": {"type": "string"}}}),
            constrained_sampling: Some(ConstrainedSampling::Grammar {
                variants: GrammarVariants {
                    openai_lark: None,
                    openai_regex: Some("(?s).*".to_owned()),
                },
            }),
        }]);
        let mut events = Vec::new();
        emit_event(
            &mut events,
            &mut builder,
            StreamEvent::ToolCallStart {
                index: 0,
                id: ToolCallId("call".to_owned()),
                name: "custom".to_owned(),
            },
        )
        .unwrap();
        start(&mut builder, 0).unwrap();
        assert_eq!(property(&builder, 0).unwrap(), "source");
        // The immutable request definitions are no longer needed for framing.
        builder.tool_definitions = None;
        delta(&mut events, &mut builder, 0, "\"line\n").unwrap();
        delta(&mut events, &mut builder, 0, "é").unwrap();
        finish(&mut events, &mut builder, 0, Some("\"line\né")).unwrap();
        let args = &builder.tool_call_builders[&0].arguments_json;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(args).unwrap(),
            serde_json::json!({"source": "\"line\né"})
        );
        finish(&mut events, &mut builder, 0, Some("\"line\né")).unwrap();
        assert!(finish(&mut events, &mut builder, 0, Some("changed")).is_err());
        assert_eq!(builder.buffered_content_bytes, "source".len());
    }
}
