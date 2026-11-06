use super::*;
use crate::extension_process::tests::{
    insert_test_parent, protocol_read_state_for_test, test_resource_owner, wave1_error, wave1_line,
};
use serde_json::{json, Value};

fn harness() -> (
    ProtocolReadState,
    mpsc::Receiver<WriterFrame>,
    broadcast::Receiver<ExtensionEvent>,
    ExtensionResourceOwner,
) {
    let (events, received) = broadcast::channel(16);
    let (state, frames) = protocol_read_state_for_test(ManifestContributions::default(), events);
    let owner = test_resource_owner("prompt-owner");
    insert_test_parent(&state, 1, Some(owner.clone()));
    let mut protocol = write_std_lock(&state.protocol);
    protocol.version = EXTENSION_API_VERSION_0_4.into();
    protocol
        .features
        .extend([FEATURE.into(), EXTENSION_FEATURE_SYSTEM_PROMPT_READ.into()]);
    drop(protocol);
    (state, frames, received, owner)
}

fn read(
    state: &ProtocolReadState,
    id: u64,
    parent: u64,
    owner: &ExtensionResourceOwner,
    scope: &str,
) {
    handle_protocol_line(
        &wave1_line(
            id,
            methods::CONTEXT_SYSTEM_PROMPT,
            json!({
                "parent_request_id":parent, "resource_owner":owner, "scope":scope
            }),
        ),
        state,
    )
    .unwrap();
}

fn value(frames: &mut mpsc::Receiver<WriterFrame>) -> Value {
    serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap()
}

#[test]
fn run_prompt_reads_negotiate_only_api_0_4_declared_disclosure_with_base_feature() {
    for (version, declared, base, expected) in [
        ("0.4", true, true, None),
        ("0.4", false, true, Some("unknown feature")),
        ("0.4", true, false, Some("requires system_prompt_read")),
        ("0.2", true, true, Some("unknown feature")),
    ] {
        let manifest = ExtensionManifest::parse(&format!(
            r#"name = "run-prompt-probe"
version = "0.1.0"
api_version = "{version}"
[entrypoint]
command = "probe.sh"
[capabilities]
system_prompt = {declared}
"#
        ))
        .unwrap();
        let mut features = API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect::<Vec<_>>();
        features.push(FEATURE.into());
        if base {
            features.push(EXTENSION_FEATURE_SYSTEM_PROMPT_READ.into());
        }
        let response: InitializeResponse = serde_json::from_value(json!({
            "api_version":version,
            "protocol":{"version":version, "features":features, "limits":{"max_concurrent_requests":1}}
        })).unwrap();
        let result = negotiate_contributions_with_host_services(
            &manifest,
            response,
            1,
            OfferedHostServices::default(),
        );
        if let Some(message) = expected {
            assert!(result.unwrap_err().to_string().contains(message));
        } else {
            let (_, protocol) = result.unwrap();
            assert!(protocol.supports(FEATURE));
            assert!(protocol.supports(EXTENSION_FEATURE_SYSTEM_PROMPT_READ));
        }
    }
}

#[test]
fn run_prompt_reads_answer_exact_text_and_empty_without_foreground_event_pump() {
    let (state, mut frames, mut events, owner) = harness();
    let first = state
        .prompt_reads
        .bind(owner.clone(), "exact\n文", CancellationToken::default());
    read(&state, 701, 1, &owner, first.scope());
    assert_eq!(value(&mut frames)["result"], json!({"text":"exact\n文"}));
    read(&state, 702, 0, &owner, first.scope());
    assert_eq!(value(&mut frames)["result"], json!({"text":"exact\n文"}));
    let second = state
        .prompt_reads
        .bind(owner.clone(), "", CancellationToken::default());
    drop(first);
    read(&state, 703, 1, &owner, second.scope());
    assert_eq!(value(&mut frames)["result"], json!({"text":""}));
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[test]
fn run_prompt_reads_refuse_cancel_drop_and_replaced_scope_without_frontend_fallback() {
    let (state, mut frames, mut events, owner) = harness();
    let cancel = CancellationToken::default();
    let first = state
        .prompt_reads
        .bind(owner.clone(), "private-old", cancel.clone());
    let old_scope = first.scope().to_owned();
    cancel.cancel();
    read(&state, 710, 0, &owner, &old_scope);
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
    let second = state.prompt_reads.bind(
        owner.clone(),
        "private-current",
        CancellationToken::default(),
    );
    read(&state, 711, 0, &owner, &old_scope);
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
    drop(first);
    let current_scope = second.scope().to_owned();
    read(&state, 712, 0, &owner, &current_scope);
    assert_eq!(
        value(&mut frames)["result"],
        json!({"text":"private-current"})
    );
    drop(second);
    read(&state, 713, 0, &owner, &current_scope);
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[test]
fn run_prompt_reads_refuse_wrong_owner_generation_instance_and_retirement() {
    let (state, mut frames, _, owner) = harness();
    let guard = state
        .prompt_reads
        .bind(owner.clone(), "private", CancellationToken::default());
    for (index, changed) in [
        ExtensionResourceOwner {
            session_id: "other".into(),
            ..owner.clone()
        },
        ExtensionResourceOwner {
            process_generation: 2,
            ..owner.clone()
        },
        ExtensionResourceOwner {
            extension_instance_id: "other".into(),
            ..owner.clone()
        },
    ]
    .into_iter()
    .enumerate()
    {
        read(&state, 720 + index as u64, 0, &changed, guard.scope());
        let response = value(&mut frames);
        assert_eq!(response["error"]["code"], -32002);
        assert!(!response.to_string().contains("private"));
    }
    insert_test_parent(&state, 2, Some(test_resource_owner("other")));
    read(&state, 724, 2, &owner, guard.scope());
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
    lock_std_mutex(&state.issued_resource_owners).remove(&owner);
    read(&state, 725, 0, &owner, guard.scope());
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
}

#[test]
fn run_prompt_reads_require_both_features_and_refuse_closed_draining_and_settled_parent() {
    let (state, mut frames, mut events, owner) = harness();
    let guard = state
        .prompt_reads
        .bind(owner.clone(), "private", CancellationToken::default());
    for feature in [FEATURE, EXTENSION_FEATURE_SYSTEM_PROMPT_READ] {
        write_std_lock(&state.protocol).features.remove(feature);
        read(
            &state,
            if feature == FEATURE { 730 } else { 731 },
            0,
            &owner,
            guard.scope(),
        );
        assert_eq!(
            wave1_error(&frames.try_recv().unwrap()),
            (-32601, "unsupported_feature".into())
        );
        write_std_lock(&state.protocol)
            .features
            .insert(feature.into());
    }
    state.draining.store(true, Ordering::Release);
    read(&state, 732, 0, &owner, guard.scope());
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
    state.draining.store(false, Ordering::Release);
    state.closed.store(true, Ordering::Release);
    read(&state, 733, 0, &owner, guard.scope());
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
    state.closed.store(false, Ordering::Release);
    lock_std_mutex(&state.pending).remove(&1);
    read(&state, 734, 1, &owner, guard.scope());
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32002, "not_foreground_owner".into())
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[test]
fn run_prompt_reads_refuse_oversized_and_nul_without_truncation_or_disclosure() {
    let (state, mut frames, _, owner) = harness();
    let maximum = "文".repeat(MAX_EXTENSION_SYSTEM_PROMPT_BYTES / 3) + "\t";
    assert_eq!(maximum.len(), MAX_EXTENSION_SYSTEM_PROMPT_BYTES);
    let guard = state
        .prompt_reads
        .bind(owner.clone(), &maximum, CancellationToken::default());
    read(&state, 740, 0, &owner, guard.scope());
    assert_eq!(value(&mut frames)["result"]["text"], maximum);
    for (id, text, failure) in [
        (741, maximum + "x", ExtensionRequestFailure::BoundsExceeded),
        (
            742,
            "private\0text".into(),
            ExtensionRequestFailure::InvalidRequest,
        ),
    ] {
        let guard = state
            .prompt_reads
            .bind(owner.clone(), &text, CancellationToken::default());
        read(&state, id, 0, &owner, guard.scope());
        let frame = frames.try_recv().unwrap();
        assert_eq!(wave1_error(&frame), (failure.code(), failure.name().into()));
        let response: Value = serde_json::from_slice(&frame.line).unwrap();
        assert!(!response.to_string().contains("private"));
    }
}
