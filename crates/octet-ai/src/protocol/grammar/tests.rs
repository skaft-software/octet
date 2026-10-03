//! Unit tests for `crate::protocol::grammar`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `grammar.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::grammar`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::types::{ConstrainedSampling, GrammarVariants, ModelId, Protocol, ToolCallId};

#[test]
fn custom_property_is_resolved_once_and_preserves_fragment_escaping() {
    let mut builder =
        ResponseBuilder::new(ModelId("test".to_owned()), Protocol::OpenAiResponses, None);
    builder.tool_definitions = Some(vec![ToolDef {
        async_execution: false,
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
            async_execution: false,
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
