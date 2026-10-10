use super::*;
use octet_agent::{EntryId, EntryMetadata};
use octet_ai::{
    AssistantMessage, AssistantPart, Cost, EndpointId, Message, ModelId, Protocol, Usage,
    UserMessage, UserPart,
};
use serde_json::json;

fn store(root: &tempfile::TempDir) -> SessionStore {
    let path = root.path().canonicalize().unwrap();
    SessionStore::new(&path.join("sessions"), &path)
}
fn user(text: &str) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: vec![UserPart::Text(text.into())],
    }))
}
fn native_fixture(root: &tempfile::TempDir) -> (SessionStore, PathBuf, EntryId) {
    let store = store(root);
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("original.jsonl");
    let mut s = Session::create(&path).unwrap();
    s.initialize_header(store.workspace().unwrap(), None)
        .unwrap();
    let first = s
        .append(user("branch root sk-syntheticsecret123456"))
        .unwrap();
    let answer = s
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("first answer".into())],
            model: ModelId("model".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    s.record_assistant_usage(
        answer.clone(),
        EndpointId("route".into()),
        ModelId("model".into()),
        Usage {
            input_tokens: 4,
            output_tokens: 2,
            total_tokens: 6,
            ..Default::default()
        },
        Some(Cost {
            total: 7,
            total_picodollars_remainder: 11,
            ..Default::default()
        }),
    )
    .unwrap();
    s.checkpoint(first.clone()).unwrap();
    s.checkout(first.clone()).unwrap();
    s.append(EntryValue::BranchSummary {
        summary: "handoff from first branch".into(),
        from_entry: answer,
        details: Default::default(),
    })
    .unwrap();
    s.append_with_metadata(
        user("second branch"),
        Some(EntryMetadata {
            extension_metadata: [(
                "private.extension".into(),
                octet_agent::ExtensionEntryMetadata {
                    public: false,
                    value: json!("do-not-export"),
                    provenance: octet_agent::ExtensionMetadataProvenance {
                        extension: "private.extension".into(),
                        process_generation: Some(5),
                    },
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }),
    )
    .unwrap();
    s.checkout(first.clone()).unwrap();
    drop(s);
    store.rename("original", "Named fixture").unwrap();
    store
        .set_tags("original", vec!["synthetic".into()])
        .unwrap();
    (store, path, first)
}

#[test]
fn redacted_roundtrip_preserves_whole_graph_checked_out_head_summary_context_and_accounting() {
    let root = tempfile::tempdir().unwrap();
    let (store, source, head) = native_fixture(&root);
    let before = std::fs::read(&source).unwrap();
    let original = Session::open_read_only(&source).unwrap();
    let export = crate::session_commands::export_portable(
        &store,
        "original",
        Some(root.path().join("snapshot.json")),
        root.path(),
        false,
        false,
    )
    .unwrap();
    let result = import_session(
        &store,
        &export.destination.canonicalize().unwrap(),
        store.workspace().unwrap(),
    )
    .unwrap();
    assert_ne!(result.destination, source);
    assert_ne!(result.id, "original");
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let imported = Session::open(&result.destination).unwrap();
    assert_eq!(imported.header().unwrap().id, result.id);
    assert_eq!(imported.head(), Some(head));
    assert_eq!(imported.entries().len(), original.entries().len());
    for (a, b) in original.entries().iter().zip(imported.entries()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.parent, b.parent);
    }
    assert_eq!(imported.checkpoints(), original.checkpoints());
    assert_eq!(imported.usage_records(), original.usage_records());
    assert_eq!(imported.total_cost_microdollars(), 7);
    assert_eq!(imported.total_cost_picodollars_remainder(), 11);
    assert_eq!(
        store.load_metadata(&result.id).unwrap().name.as_deref(),
        Some("Named fixture")
    );
    assert!(imported.entries().iter().any(|e| matches!(&e.value,EntryValue::BranchSummary { summary, .. } if summary == "handoff from first branch")));
    assert!(!std::fs::read_to_string(&result.destination)
        .unwrap()
        .contains("syntheticsecret"));
    assert!(!std::fs::read_to_string(&result.destination)
        .unwrap()
        .contains("do-not-export"));
    assert!(serde_json::to_string(&imported.context().unwrap())
        .unwrap()
        .contains("branch root [REDACTED]"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&result.destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn jsonl_roundtrip_retains_graph_but_reports_no_pi_compatibility() {
    let root = tempfile::tempdir().unwrap();
    let (store, source, head) = native_fixture(&root);
    let report = crate::session_commands::export_jsonl(
        &store,
        "original",
        Some(root.path().join("native.jsonl")),
        root.path(),
        false,
        false,
    )
    .unwrap();
    let result = import_session(
        &store,
        &report.destination.canonicalize().unwrap(),
        store.workspace().unwrap(),
    )
    .unwrap();
    let restored = Session::open(&result.destination).unwrap();
    assert_eq!(restored.head(), Some(head));
    assert_eq!(
        restored.entries().len(),
        Session::open_read_only(&source).unwrap().entries().len()
    );
    assert_eq!(result.source_format, "octet-jsonl");
}

fn records() -> Vec<Value> {
    vec![
        json!({"type":"entry","id":"a","parent":null,"value":serde_json::to_value(user("hi")).unwrap()}),
        json!({"type":"head","id":"a"}),
    ]
}
fn package(records: Vec<Value>) -> Value {
    json!({"format":"octet-session-export","version":1,"exported_at_unix_seconds":1,"source_id":"original","source_title":"hi","metadata":{},"redacted":true,"redaction_count":0,"records":records})
}

#[test]
fn bad_json_version_duplicate_id_parent_cycle_head_and_truncated_head_fail_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let source = root.path().canonicalize().unwrap().join("input.json");
    let mut wrong_version = package(records());
    wrong_version["version"] = json!(2);
    let mut duplicate = records();
    duplicate.insert(1, duplicate[0].clone());
    let mut dangling = records();
    dangling[0]["parent"] = json!("missing");
    let mut cycle = records();
    cycle[0]["parent"] = json!("a");
    let mut bad_head = records();
    bad_head[1]["id"] = json!("absent");
    let mut truncated = records();
    truncated.pop();
    let mut unknown = records();
    unknown[0]["future"] = json!(true);
    let cases = vec![
        b"{\"format\":".to_vec(),
        serde_json::to_vec(&wrong_version).unwrap(),
        serde_json::to_vec(&package(duplicate)).unwrap(),
        serde_json::to_vec(&package(dangling)).unwrap(),
        serde_json::to_vec(&package(cycle)).unwrap(),
        serde_json::to_vec(&package(bad_head)).unwrap(),
        serde_json::to_vec(&package(truncated)).unwrap(),
        serde_json::to_vec(&package(unknown)).unwrap(),
        b"{\"type\":\"entry\",\"id\":\"a\",\"parent\":null,\"parent\":\"b\"}\n".to_vec(),
    ];
    for bytes in cases {
        std::fs::write(&source, &bytes).unwrap();
        assert!(import_session(&store, &source, store.workspace().unwrap()).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert!(
            !store.dir().exists(),
            "invalid input must not publish/create store"
        );
    }
}

#[test]
fn unknown_fields_at_buffered_record_depths_fail_before_publication_even_if_empty() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let source = root.path().canonicalize().unwrap().join("unknown.json");
    let mut fixture = records();
    fixture[0]["metadata"] = json!({});
    fixture.insert(
        1,
        json!({"type":"usage", "record": {
            "kind":{"kind":"cache_warm"},
            "usage":serde_json::to_value(Usage::default()).unwrap()
        }}),
    );
    for (index, pointer) in [
        (0, ""),
        (0, "/metadata"),
        (0, "/value"),
        (0, "/value/User"),
        (1, ""),
        (1, "/record"),
        (1, "/record/kind"),
        (1, "/record/usage"),
        (2, ""),
    ] {
        for unknown in [
            json!(true),
            Value::Null,
            json!(false),
            json!({}),
            json!([]),
            json!(""),
        ] {
            let mut graph = fixture.clone();
            graph[index].pointer_mut(pointer).unwrap()["future"] = unknown;
            for bytes in [
                serde_json::to_vec(&package(graph.clone())).unwrap(),
                encode_jsonl(&graph).unwrap(),
            ] {
                std::fs::write(&source, &bytes).unwrap();
                let error =
                    import_session(&store, &source, store.workspace().unwrap()).unwrap_err();
                assert!(
                    error.to_string().contains("Octet record"),
                    "{index}:{pointer}: {error}"
                );
                assert_eq!(std::fs::read(&source).unwrap(), bytes);
                assert!(
                    !store.dir().exists(),
                    "unknown fields must not create the store"
                );
            }
        }
    }
    for unknown in [Value::Null, json!(false), json!({}), json!([]), json!("")] {
        let mut data = package(records());
        data["metadata"]["future"] = unknown;
        let bytes = serde_json::to_vec(&data).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        assert!(import_session(&store, &source, store.workspace().unwrap()).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert!(!store.dir().exists());
    }
}

#[test]
fn explicit_legacy_none_false_and_empty_metadata_defaults_remain_importable() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let source = root.path().canonicalize().unwrap().join("legacy.json");
    let mut graph = records();
    graph[0]["timestamp_unix_ms"] = Value::Null;
    graph[0]["metadata"] = json!({
        "custom_message":null, "native_steering":null, "prompt_model":null,
        "prompt_model_source":null, "prompt_color":null, "display_text":null,
        "run_outcome":null, "tool_output":null, "tool_composition":null,
        "replay_safe_tool_calls":null, "tool_started_unix_ms":null,
        "tool_finished_unix_ms":null, "local_synthetic_assistant":false,
        "extension_metadata":{}
    });
    graph.insert(
        1,
        json!({"type":"entry", "id":"b", "parent":"a",
        "metadata":null, "timestamp_unix_ms":null,
        "value":serde_json::to_value(user("legacy")).unwrap()}),
    );
    graph[2]["id"] = json!("b");
    let mut data = package(graph);
    data["metadata"] = json!({"name":null,"tags":[],"pinned":false,"archived":false,
        "trashed_at_ms":null,"purge_after_ms":null,
        "forked_from_session_id":null,"forked_from_entry_id":null});
    let bytes = serde_json::to_vec(&data).unwrap();
    std::fs::write(&source, &bytes).unwrap();
    let report = import_session(&store, &source, store.workspace().unwrap()).unwrap();
    assert_eq!(
        Session::open_read_only(report.destination)
            .unwrap()
            .entries()
            .len(),
        2
    );
    assert_eq!(std::fs::read(source).unwrap(), bytes);
}

#[test]
fn nested_known_skipped_fields_and_opaque_json_are_not_lossy() {
    let opaque = json!({"future":null,"false":false,"empty":{},"array":[],
        "nested":{"future":true}});
    let cases = vec![
        json!({"type":"entry","id":"b","parent":"a","value":{
            "type":"message","Assistant":{"content":[{"ToolCall":{
                "id":"call","name":"read","arguments_json":"{}",
                "async":false,"argument_error":null}}],"model":"model","protocol":"open_ai_chat"}}}),
        json!({"type":"entry","id":"b","parent":"a","value":{
            "type":"message","User":{"content":[{"ToolResult":{
                "tool_call_id":"call","content":[],"is_error":false,"added_tool_names":null}}]}}}),
        json!({"type":"entry","id":"b","parent":"a","value":{
            "type":"config","model":null,"reasoning":null,"reasoning_mode":null}}),
        json!({"type":"entry","id":"b","parent":"a","value":{
            "type":"compaction","summary":"summary","first_kept":"a","snapcompact":null}}),
        json!({"type":"entry","id":"b","parent":"a","value":{
            "type":"message","User":{"content":[{"Media":{"Image":{
                "source":{"Inline":[1,2,3]},"media_type":null,"detail":null}}}]}}}),
        json!({"type":"entry","id":"b","parent":"a","value":{
            "type":"responses_steering","endpoint":"route","model":"model",
            "operation":"operation","local_id":1,"input":{"content":[{"ToolResult":{
                "tool_call_id":"call","content":[],"is_error":false,"added_tool_names":null}}]},
            "state":null,"completed":null}}),
        json!({"type":"entry","id":"b","parent":"a",
            "value":serde_json::to_value(user("opaque")).unwrap(),"metadata":{
                "run_outcome":{"status":"completed","message":null},
                "tool_output":{"structured_content":opaque,"metadata":null},
                "custom_message":{"custom_type":"notice","content":"opaque","display":false,"details":opaque},
                "extension_metadata":{"test.namespace":{"public":true,"value":opaque,
                    "provenance":{"extension":"test.namespace","process_generation":null}}},
                "tool_composition":{"kind":"call_finished","parent":"call","id":"nested",
                    "tool":"read","ok":true,"duration_ms":0,"effect":null,"allowed":true,
                    "denial_code":null,"delivery_text":null}}}),
        json!({"type":"entry","id":"b","parent":"a",
            "value":serde_json::to_value(user("opaque")).unwrap(),"metadata":{
                "tool_output":{"structured_content":null,"metadata":opaque}}}),
        json!({"type":"entry","id":"b","parent":"a","value":{
            "type":"responses_turn","assistant":"a","endpoint":"route","model":"model",
            "output":[{"type":"future_provider_item","payload":opaque}]}}),
        json!({"type":"usage","record":{"kind":{"kind":"cache_warm"},
            "usage":serde_json::to_value(Usage::default()).unwrap(),"stop_reason":null}}),
        json!({"type":"cache_warm","record":{"attempt":1,"endpoint":"route","model":"model",
            "state":"completed","at_unix_ms":1,"anchor":null}}),
        json!({"type":"usage_uncertainty","record":{"endpoint":"route","model":"model",
            "operation":"assistant_turn"},"bound":null}),
        json!({"type":"usage_uncertainty","record":{"endpoint":"route","model":"model",
            "operation":"assistant_turn"},"bound":{"tokens":1,"cost_microdollars":null}}),
    ];
    for record in cases {
        let mut graph = records();
        if record["type"] == "entry" {
            graph[1]["id"] = json!("b");
        }
        graph.insert(1, record.clone());
        validate_records(&graph).unwrap_or_else(|error| panic!("{record}: {error}"));
        let typed: SessionRecord = serde_json::from_value(record.clone()).unwrap();
        let decoded = serde_json::to_value(typed).unwrap();
        for pointer in [
            "/metadata/tool_output/structured_content",
            "/metadata/tool_output/metadata",
            "/metadata/custom_message/details",
            "/metadata/extension_metadata/test.namespace/value",
            "/value/output/0/payload",
        ] {
            if let Some(source) = record.pointer(pointer) {
                // Option<Value> metadata intentionally normalizes explicit None.
                if !source.is_null() || pointer.ends_with("structured_content") {
                    assert_eq!(decoded.pointer(pointer), Some(source), "{pointer}");
                }
            }
        }
        // The allowlist cannot absorb an unknown default-looking key nearby.
        let mut unknown = graph.clone();
        unknown[1]["future"] = Value::Null;
        assert!(validate_records(&unknown).is_err());
        if record["type"] == "entry"
            && record["value"]["type"] == "message"
            && (record["value"]["Assistant"]["content"][0]
                .get("ToolCall")
                .is_some()
                || record["value"]["User"]["content"][0]
                    .get("ToolResult")
                    .is_some())
        {
            let role = if record["value"].get("Assistant").is_some() {
                "Assistant"
            } else {
                "User"
            };
            let part = if role == "Assistant" {
                "ToolCall"
            } else {
                "ToolResult"
            };
            for default in [Value::Null, json!(false), json!({}), json!([])] {
                let mut unknown = graph.clone();
                unknown[1]["value"][role]["content"][0][part]["future"] = default;
                assert!(validate_records(&unknown).is_err());
            }
        }
    }
}

#[test]
fn known_field_names_are_not_allowed_at_the_wrong_path_or_variant() {
    for (pointer, field, value) in [
        ("", "reasoning_mode", Value::Null),
        ("/value", "reasoning_mode", Value::Null),
        ("/value/User", "prompt_model", Value::Null),
        ("/value/User", "extension_metadata", json!({})),
        ("/value/User", "local_synthetic_assistant", json!(false)),
    ] {
        let mut graph = records();
        graph[0].pointer_mut(pointer).unwrap()[field] = value;
        assert!(validate_records(&graph).is_err(), "{pointer}/{field}");
    }
}

#[test]
fn semantic_checkpoint_corruption_and_torn_jsonl_tail_do_not_publish() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let source = root.path().canonicalize().unwrap().join("input.jsonl");
    let mut record = records();
    record.push(json!({"type":"checkpoint","prompt":"missing","head":"a"}));
    std::fs::write(&source, encode_jsonl(&record).unwrap()).unwrap();
    assert!(import_session(&store, &source, store.workspace().unwrap()).is_err());
    assert!(std::fs::read_dir(store.dir()).unwrap().all(|e| e
        .unwrap()
        .path()
        .extension()
        .is_none_or(|x| x != "jsonl")));
    let mut bytes = encode_jsonl(&records()).unwrap();
    bytes.extend_from_slice(b"{\"type\":");
    std::fs::write(&source, bytes).unwrap();
    assert!(import_session(&store, &source, store.workspace().unwrap()).is_err());
}

#[test]
fn no_clobber_existing_source_current_export_and_atomic_destination() {
    let root = tempfile::tempdir().unwrap();
    let (store, source, _) = native_fixture(&root);
    let original = std::fs::read(&source).unwrap();
    assert!(crate::session_commands::export_portable(
        &store,
        "original",
        Some(source.clone()),
        root.path(),
        false,
        false
    )
    .is_err());
    let first = import_session(&store, &source, store.workspace().unwrap()).unwrap();
    let second = import_session(&store, &source, store.workspace().unwrap()).unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(first.destination, second.destination);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert!(octet_agent::secure_fs::write_private_atomic_if_unchanged(
        &first.destination,
        None,
        b"clobber",
        MAX_SESSION_FILE_BYTES
    )
    .is_err());
    assert!(Session::open(&first.destination).is_ok());
}

#[cfg(unix)]
#[test]
fn importer_rejects_file_and_parent_symlinks_and_oversized_files() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let base = root.path().canonicalize().unwrap();
    let actual = base.join("actual");
    std::fs::create_dir(&actual).unwrap();
    let input = actual.join("input");
    std::fs::write(&input, serde_json::to_vec(&package(records())).unwrap()).unwrap();
    symlink(&input, base.join("file-link")).unwrap();
    symlink(&actual, base.join("dir-link")).unwrap();
    for p in [base.join("file-link"), base.join("dir-link/input")] {
        assert!(import_session(&store, &p, store.workspace().unwrap()).is_err());
    }
    let huge = base.join("huge");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(MAX_SESSION_FILE_BYTES as u64 + 1)
        .unwrap();
    assert!(import_session(&store, &huge, store.workspace().unwrap()).is_err());
}

fn pi_fixture() -> Vec<Value> {
    let stamp = "2026-07-22T11:22:33.444Z";
    vec![
        json!({"type":"session","version":3,"id":"pi-original","cwd":"/synthetic","timestamp":stamp}),
        json!({"type":"model_change","id":"m","parentId":null,"timestamp":stamp,"provider":"openai","modelId":"test"}),
        json!({"type":"message","id":"u","parentId":"m","timestamp":stamp,"message":{"role":"user","content":"hello","timestamp":1}}),
        json!({"type":"message","id":"a","parentId":"u","timestamp":stamp,"message":{"role":"assistant","content":[{"type":"thinking","thinking":"plan"},{"type":"text","text":"working"},{"type":"toolCall","id":"call","name":"read","arguments":{"path":"synthetic.txt"}}],"api":"openai-completions","provider":"openai","model":"test","usage":{"input":3,"output":2,"cacheRead":1,"cacheWrite":0,"totalTokens":6,"cost":{"input":0.0000003,"output":0.0000002,"cacheRead":0.0000001,"cacheWrite":0,"total":0.0000006}},"stopReason":"toolUse","timestamp":2}}),
        json!({"type":"message","id":"t","parentId":"a","timestamp":stamp,"message":{"role":"toolResult","toolCallId":"call","toolName":"read","content":[{"type":"text","text":"file text"}],"details":{"lines":2},"isError":false,"timestamp":3}}),
        json!({"type":"branch_summary","id":"b","parentId":"u","timestamp":stamp,"fromId":"t","summary":"read synthetic file","details":{"readFiles":["synthetic.txt"],"modifiedFiles":[]}}),
        json!({"type":"message","id":"v","parentId":"b","timestamp":stamp,"message":{"role":"user","content":[{"type":"text","text":"continue"}],"timestamp":4}}),
    ]
}

#[test]
fn actual_pi_v3_user_assistant_tool_result_branch_summary_preserve_tree_and_reported_usage() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let source = root.path().canonicalize().unwrap().join("pi.jsonl");
    std::fs::write(&source, encode_jsonl(&pi_fixture()).unwrap()).unwrap();
    let result = import_session(&store, &source, store.workspace().unwrap()).unwrap();
    let session = Session::open(&result.destination).unwrap();
    assert_eq!(session.entries().len(), 6);
    assert_eq!(session.head(), Some(EntryId("v".into())));
    assert_eq!(
        session.entry(&EntryId("b".into())).unwrap().parent,
        Some(EntryId("u".into()))
    );
    assert_eq!(session.usage_records().len(), 1);
    assert_eq!(session.usage_records()[0].usage.total_tokens, 6);
    assert_eq!(session.total_cost_microdollars(), 0);
    assert_eq!(session.total_cost_picodollars_remainder(), 600000);
    assert!(matches!(
        session.entry(&EntryId("t".into())).unwrap().value,
        EntryValue::Message(Message::User(_))
    ));
    assert!(serde_json::to_string(&session.context().unwrap())
        .unwrap()
        .contains("read synthetic file"));
    assert!(!serde_json::to_string(&session.context().unwrap())
        .unwrap()
        .contains("file text"));
    assert!(session
        .entry(&EntryId("t".into()))
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .tool_output
        .is_some());
}

#[test]
fn pi_unsupported_records_and_replay_fields_are_explicit_errors_not_silent_loss() {
    let mut fixture = pi_fixture();
    fixture.push(json!({"type":"context_edit","id":"edit","parentId":"v","timestamp":"2026-07-22T11:22:33.444Z","targetId":"u","replacement":null}));
    assert!(pi::convert(fixture)
        .unwrap_err()
        .to_string()
        .contains("unsupported Pi semantic record"));
    let mut fixture = pi_fixture();
    fixture[3]["message"]["content"][0]["thinkingSignature"] = json!("opaque-synthetic");
    assert!(pi::convert(fixture)
        .unwrap_err()
        .to_string()
        .contains("thinkingSignature"));
    let mut fixture = pi_fixture();
    fixture[3]["message"]["api"] = json!("openai-responses");
    assert!(pi::convert(fixture)
        .unwrap_err()
        .to_string()
        .contains("Responses"));
    let mut fixture = pi_fixture();
    fixture[3]["message"]
        .as_object_mut()
        .unwrap()
        .remove("usage");
    let (records, _, _, warnings) = pi::convert(fixture).unwrap();
    assert!(records.iter().any(|r| r["type"] == "usage_uncertainty"));
    assert!(!records.iter().any(|r| r["type"] == "usage"));
    assert!(warnings.iter().any(|w| w.contains("unknown, not zero")));
}

#[test]
fn source_header_and_metadata_cannot_claim_local_identity_archive_or_fork_authority() {
    let root = tempfile::tempdir().unwrap();
    let (store, source, _) = native_fixture(&root);
    let mut data = package(strict_json::jsonl(&std::fs::read(&source).unwrap()).unwrap());
    data["metadata"] = json!({"name":"Portable name", "tags":["tag"], "archived":true,
        "trashed_at_ms":1,"purge_after_ms":2,"forked_from_session_id":"foreign",
        "forked_from_entry_id":"foreign-entry"});
    data["records"][0]["header"]["parent_session"] = json!("/never/open/foreign.jsonl");
    let input = root.path().canonicalize().unwrap().join("metadata.json");
    std::fs::write(&input, serde_json::to_vec(&data).unwrap()).unwrap();
    let result = import_session(&store, &input, store.workspace().unwrap()).unwrap();
    let imported = Session::open_read_only(&result.destination).unwrap();
    let header = imported.header().unwrap();
    assert_eq!(header.id, result.id);
    assert_eq!(header.cwd, store.workspace().unwrap());
    assert_eq!(header.parent_session, None);
    let metadata = store.load_metadata(&result.id).unwrap();
    assert_eq!(metadata.name.as_deref(), Some("Portable name"));
    assert_eq!(metadata.tags, vec!["tag"]);
    assert!(!metadata.archived);
    assert_eq!(metadata.trashed_at_ms, None);
    assert_eq!(metadata.purge_after_ms, None);
    assert_eq!(metadata.forked_from_session_id, None);
    assert_eq!(metadata.forked_from_entry_id, None);
}

#[test]
fn custom_pi_messages_use_actual_core_projection_and_inert_details() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let mut fixture = pi_fixture();
    fixture.push(json!({"type":"custom_message","id":"custom","parentId":"v",
        "timestamp":"2026-07-22T11:22:33.444Z","customType":"synthetic.notice",
        "content":[{"type":"text","text":"first"},{"type":"text","text":"second"}],
        "display":false,"details":null}));
    let input = root.path().canonicalize().unwrap().join("custom.jsonl");
    std::fs::write(&input, encode_jsonl(&fixture).unwrap()).unwrap();
    let report = import_session(&store, &input, store.workspace().unwrap()).unwrap();
    let session = Session::open_read_only(report.destination).unwrap();
    let entry = session.entry(&EntryId("custom".into())).unwrap();
    let custom = entry
        .metadata
        .as_ref()
        .unwrap()
        .custom_message
        .as_ref()
        .unwrap();
    assert!(!custom.display);
    assert_eq!(custom.details, Some(Value::Null));
    assert_eq!(
        serde_json::to_value(custom.user_parts()).unwrap(),
        serde_json::to_value(vec![
            UserPart::Text("first".into()),
            UserPart::Text("second".into())
        ])
        .unwrap()
    );
    assert!(serde_json::to_string(&session.context().unwrap())
        .unwrap()
        .contains("second"));
    let tool = session.entry(&EntryId("t".into())).unwrap();
    let provenance = serde_json::to_value(
        tool.metadata
            .as_ref()
            .unwrap()
            .tool_output
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert!(provenance.to_string().contains("toolName"));
}

#[test]
fn pi_compaction_label_info_config_and_usage_retain_context_and_provenance() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let stamp = "2026-07-22T11:22:33.444Z";
    let mut fixture = pi_fixture();
    fixture.extend([
        json!({"type":"thinking_level_change","id":"think","parentId":"v","timestamp":stamp,"thinkingLevel":"high"}),
        json!({"type":"compaction","id":"compact","parentId":"think","timestamp":stamp,"summary":"kept summary","firstKeptEntryId":"v","tokensBefore":123,"fromHook":false,"details":{"readFiles":["synthetic.txt"],"modifiedFiles":[]}}),
        json!({"type":"label","id":"label","parentId":"compact","timestamp":stamp,"targetId":"v","label":"Bookmark"}),
        json!({"type":"session_info","id":"name","parentId":"label","timestamp":stamp,"name":"Pi named session"}),
        json!({"type":"usage","id":"warm","parentId":"name","timestamp":stamp,"kind":"cache_warm","provider":"openai","model":"test","note":"warm note","usage":fixture[3]["message"]["usage"].clone()}),
    ]);
    let input = root.path().canonicalize().unwrap().join("pi-compact.jsonl");
    std::fs::write(&input, encode_jsonl(&fixture).unwrap()).unwrap();
    let report = import_session(&store, &input, store.workspace().unwrap()).unwrap();
    let session = Session::open_read_only(report.destination).unwrap();
    assert_eq!(session.head(), Some(EntryId("warm".into())));
    let context = serde_json::to_string(&session.context().unwrap()).unwrap();
    assert!(context.contains("kept summary"));
    assert!(context.contains("continue"));
    assert!(!context.contains("file text"));
    assert_eq!(
        store.load_metadata(&report.id).unwrap().name.as_deref(),
        Some("Pi named session")
    );
    assert_eq!(session.entry_label(&EntryId("v".into())), Some("Bookmark"));
    assert_eq!(session.usage_records().len(), 2);
    assert!(serde_json::to_string(session.entries())
        .unwrap()
        .contains("tokensBefore"));
    assert!(serde_json::to_string(session.entries())
        .unwrap()
        .contains("warm note"));
}

#[test]
fn unsupported_pi_semantics_fail_closed_at_import_boundary_without_publication() {
    let root = tempfile::tempdir().unwrap();
    let store = store(&root);
    let input = root
        .path()
        .canonicalize()
        .unwrap()
        .join("unsupported.jsonl");
    for kind in ["custom", "context_edit", "future_entry"] {
        let mut fixture = pi_fixture();
        fixture.push(json!({"type":kind,"id":"unsupported","parentId":"v","timestamp":"2026-07-22T11:22:33.444Z"}));
        let bytes = encode_jsonl(&fixture).unwrap();
        std::fs::write(&input, &bytes).unwrap();
        assert!(import_session(&store, &input, store.workspace().unwrap())
            .unwrap_err()
            .to_string()
            .contains("unsupported Pi semantic record"));
        assert_eq!(std::fs::read(&input).unwrap(), bytes);
        assert!(!store.dir().exists());
    }
    for role in ["system", "bashExecution", "custom", "future_role"] {
        let mut fixture = pi_fixture();
        fixture[2]["message"]["role"] = json!(role);
        std::fs::write(&input, encode_jsonl(&fixture).unwrap()).unwrap();
        assert!(import_session(&store, &input, store.workspace().unwrap()).is_err());
        assert!(!store.dir().exists());
    }
}

#[test]
fn export_validates_captured_data_including_hidden_branch_corruption_and_duplicate_fields() {
    let root = tempfile::tempdir().unwrap();
    let (store, source, _) = native_fixture(&root);
    let original = std::fs::read(&source).unwrap();
    let destination = root.path().join("must-not-publish.json");
    let mut duplicate = original.clone();
    duplicate.extend_from_slice(b"{\"type\":\"head\",\"id\":\"001\",\"id\":\"001\"}");
    let mut hidden_dangling = strict_json::jsonl(&original).unwrap();
    hidden_dangling
        .iter_mut()
        .find(|r| r["type"] == "entry" && r["value"]["type"] == "branch_summary")
        .unwrap()["parent"] = json!("missing");
    let mut missing_head = strict_json::jsonl(&original).unwrap();
    missing_head.push(json!({"type":"entry","id":"pending","parent":"001","value":serde_json::to_value(user("uncommitted")).unwrap()}));
    for bytes in [
        duplicate,
        encode_jsonl(&hidden_dangling).unwrap(),
        encode_jsonl(&missing_head).unwrap(),
    ] {
        std::fs::write(&source, &bytes).unwrap();
        assert!(crate::session_commands::export_portable(
            &store,
            "original",
            Some(destination.clone()),
            root.path(),
            false,
            false
        )
        .is_err());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert!(!destination.exists());
    }
}

#[test]
fn raw_export_and_import_exclude_private_authority_even_when_secrets_are_requested() {
    let root = tempfile::tempdir().unwrap();
    let (store, source, _) = native_fixture(&root);
    let original = std::fs::read(&source).unwrap();
    let report = crate::session_commands::export_portable(
        &store,
        "original",
        Some(root.path().join("raw.json")),
        root.path(),
        true,
        false,
    )
    .unwrap();
    let exported = std::fs::read_to_string(&report.destination).unwrap();
    assert!(exported.contains("sk-syntheticsecret123456"));
    assert!(!exported.contains("do-not-export"));
    let imported = import_session(
        &store,
        &report.destination.canonicalize().unwrap(),
        store.workspace().unwrap(),
    )
    .unwrap();
    let session = Session::open_read_only(&imported.destination).unwrap();
    for entry in session.entries() {
        let metadata = entry.metadata.as_ref().unwrap();
        assert!(metadata.extension_metadata.is_empty());
        assert!(metadata.tool_composition.is_none());
        assert_eq!(metadata.replay_safe_tool_calls.as_ref().unwrap().len(), 0);
    }
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn export_torn_tail_after_a_durable_head_is_reported_without_mutating_source() {
    let root = tempfile::tempdir().unwrap();
    let (store, source, _) = native_fixture(&root);
    let mut bytes = std::fs::read(&source).unwrap();
    bytes.extend_from_slice(b"{\"type\":");
    std::fs::write(&source, &bytes).unwrap();
    let report = crate::session_commands::export_jsonl(
        &store,
        "original",
        Some(root.path().join("torn-export.jsonl")),
        root.path(),
        false,
        false,
    )
    .unwrap();
    assert!(report.ignored_torn_tail);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    let imported = import_session(
        &store,
        &report.destination.canonicalize().unwrap(),
        store.workspace().unwrap(),
    )
    .unwrap();
    assert!(Session::open_read_only(imported.destination).is_ok());
}

#[test]
fn inactive_branch_compaction_cannot_retain_a_sibling_boundary() {
    let mut graph = records();
    graph.insert(1, json!({"type":"entry","id":"sibling","parent":"a","value":serde_json::to_value(user("sibling")).unwrap()}));
    graph.insert(
        2,
        json!({"type":"entry","id":"compact","parent":"a","value":{
        "type":"compaction","summary":"summary","first_kept":"sibling"}}),
    );
    assert!(validate_records(&graph)
        .unwrap_err()
        .to_string()
        .contains("not an ancestor"));
    graph[2]["value"]["first_kept"] = json!("a");
    validate_records(&graph).unwrap();
}
