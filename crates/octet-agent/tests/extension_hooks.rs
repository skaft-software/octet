//! Host-side bounds for the retained typed extension enrichments.
use std::collections::BTreeMap;

use octet_agent::{
    EntryMetadata, ExtensionEntryMetadata, ExtensionMetadataProvenance, PostMutationContext,
    PostMutationDisposition, PostMutationKind, PostMutationState, Session, ToolProgress,
    ToolProgressDecoration, ToolProgressSink,
};
use octet_ai::{AssistantMessage, AssistantPart, EndpointId, ModelId, Protocol, StopReason, Usage};
use serde_json::{json, Value};

fn annotation(namespace: &str, value: Value) -> ExtensionEntryMetadata {
    ExtensionEntryMetadata {
        public: false,
        value,
        provenance: ExtensionMetadataProvenance {
            extension: namespace.into(),
            process_generation: Some(1),
        },
    }
}

fn persist(metadata: EntryMetadata) -> Option<EntryMetadata> {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let id = session
        .append_assistant_turn_with_metadata(
            AssistantMessage {
                content: vec![AssistantPart::Text("canonical".into())],
                model: ModelId("model".into()),
                protocol: Protocol::AnthropicMessages,
            },
            EndpointId("endpoint".into()),
            ModelId("model".into()),
            Usage::default(),
            None,
            StopReason::EndTurn,
            None,
            Some(metadata),
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(path).unwrap();
    assert!(format!("{:?}", reopened.context().unwrap()).contains("canonical"));
    reopened
        .entries()
        .iter()
        .find(|entry| entry.id == id)
        .unwrap()
        .metadata
        .clone()
}

#[test]
fn persistence_metadata_rejects_invalid_shapes_and_forged_provenance_at_durable_boundary() {
    let mut deep = Value::Null;
    for _ in 0..18 {
        deep = json!([deep]);
    }
    let values = [
        json!("x".repeat(16 * 1024)), // JSON string quotes count against encoded bytes.
        json!(vec![0; 256]),          // Root plus children exceeds the node limit.
        deep,
        json!({"x".repeat(257): 1}),
        json!("bad\u{001b}[31m"),
    ];
    for value in values {
        let metadata = EntryMetadata {
            extension_metadata: BTreeMap::from([(
                "valid.name".into(),
                annotation("valid.name", value),
            )]),
            ..Default::default()
        };
        assert!(persist(metadata).is_none());
    }
    for (namespace, provenance) in [
        ("invalid..name", "invalid..name"),
        ("valid.name", "forged.name"),
    ] {
        assert!(persist(EntryMetadata {
            extension_metadata: BTreeMap::from([(
                namespace.into(),
                annotation(provenance, json!({}))
            )]),
            ..Default::default()
        })
        .is_none());
    }
    let metadata = persist(EntryMetadata {
        display_text: Some("forged-content".into()),
        native_steering: None,
        local_synthetic_assistant: true,
        extension_metadata: BTreeMap::from([(
            "valid.name".into(),
            annotation("valid.name", json!("x".repeat(16 * 1024 - 2))),
        )]),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(metadata.extension_metadata.len(), 1);
    assert!(metadata.display_text.is_none());
    assert!(!metadata.local_synthetic_assistant);
    assert!(metadata.public_extension_metadata().is_empty());
}

#[test]
fn persistence_metadata_namespace_and_aggregate_caps_are_independent() {
    for (value, expected) in [(json!(null), 32), (json!("x".repeat(16 * 1024 - 2)), 8)] {
        let extension_metadata = (0..40)
            .map(|index| {
                let namespace = format!("extension.n{index:02}");
                (namespace.clone(), annotation(&namespace, value.clone()))
            })
            .collect();
        assert_eq!(
            persist(EntryMetadata {
                extension_metadata,
                ..Default::default()
            })
            .unwrap()
            .extension_metadata
            .len(),
            expected
        );
    }
}

#[test]
fn progress_decoration_enforces_utf8_bounds_controls_and_nonblocking_backpressure() {
    let label = "é".repeat(128);
    let detail = "é".repeat(2048);
    assert!(ToolProgressDecoration::new(&label, Some(detail.clone())).is_some());
    for (label, detail) in [
        (String::new(), None),
        ("é".repeat(129), None),
        ("label".into(), Some("é".repeat(2049))),
        ("bad\u{0085}".into(), None),
        ("label".into(), Some("bad\nline".into())),
    ] {
        assert!(ToolProgressDecoration::new(label, detail).is_none());
    }
    let (sink, mut receiver) = ToolProgressSink::bounded_channel();
    for _ in 0..1000 {
        assert!(sink.decoration(&label, Some(detail.clone())));
    }
    let mut received = 0;
    while let Ok(event) = receiver.try_recv() {
        let ToolProgress::Decoration(value) = event else {
            panic!("unexpected progress");
        };
        assert_eq!(value.label().len(), 256);
        assert_eq!(value.detail().unwrap().len(), 4096);
        received += 1;
    }
    assert_eq!(received, 64, "bounded sink does not buffer the flood");
}

#[test]
fn post_mutation_contexts_and_dispositions_are_opaque_bounded_and_normalized() {
    let context = |id: &str, resources: Vec<String>, generation| {
        PostMutationContext::new(
            id,
            PostMutationKind::Resource,
            resources,
            generation,
            PostMutationState::Committed,
        )
    };
    assert!(context("mutation:one", vec!["resource:a".into()], 0).is_none());
    assert!(context("/private/path", vec![], 1).is_none());
    assert!(context("mutation:one", vec!["/private/path".into()], 1).is_none());
    assert!(context("mutation:one", vec!["resource:a".into(); 33], 1).is_none());
    let normalized = context(
        "mutation:one",
        vec![
            "resource:b".into(),
            "resource:a".into(),
            "resource:a".into(),
        ],
        1,
    )
    .unwrap();
    assert_eq!(
        normalized.affected_resources(),
        ["resource:a", "resource:b"]
    );
    assert!(PostMutationDisposition::request_rescan(Vec::new()).is_none());
    assert!(PostMutationDisposition::request_rescan(vec!["resource:a".into(); 33]).is_none());
    assert!(PostMutationDisposition::request_rescan(["/private/path".into()]).is_none());
}
