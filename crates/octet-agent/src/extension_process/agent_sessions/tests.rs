use super::*;
use pretty_assertions::assert_eq;

fn issued_owner(state: &ProtocolReadState, session: &str) -> ExtensionResourceOwner {
    let owner = ExtensionResourceOwner {
        session_id: session.into(),
        extension_instance_id: state.instance_id.clone(),
        process_generation: state.generation,
    };
    lock_std_mutex(&state.issued_resource_owners).insert(owner.clone());
    owner
}

fn negotiate(state: &ProtocolReadState) {
    let mut protocol = write_std_lock(&state.protocol);
    protocol.version = EXTENSION_API_VERSION_0_4.into();
    protocol.features.extend(
        [
            EXTENSION_FEATURE_AGENT_SESSIONS,
            EXTENSION_FEATURE_AGENT_SESSION_EVENTS_V1,
            EXTENSION_FEATURE_AGENT_SESSION_LIFETIME_V1,
        ]
        .map(str::to_owned),
    );
}

#[test]
fn legacy_child_calls_still_require_a_live_parent() {
    let (events, _) = broadcast::channel(8);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_2.into();
    let owner = issued_owner(&state, "owner");
    insert_test_parent(&state, 10, Some(owner.clone()));
    let call = |id| {
        register_agent_session_request(
            &state,
            ExtensionRequestId::Number(id),
            10,
            methods::AGENT_LIST,
            None,
        )
    };
    let accepted = call(1).unwrap().unwrap();
    assert_eq!(accepted.parent_request_id, Some(10));
    assert_eq!(accepted.resource_owner, Some(owner));
    lock_std_mutex(&state.pending).remove(&10);
    assert!(call(2).unwrap().is_none());
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()).0,
        JSON_RPC_REQUEST_CANCELLED
    );
}

#[test]
fn retained_child_owner_is_explicit_negotiated_and_generation_bound() {
    let (events, _) = broadcast::channel(8);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    let owner = issued_owner(&state, "owner");
    let register = |id, owner| {
        register_agent_session_request(
            &state,
            ExtensionRequestId::Number(id),
            10,
            methods::AGENT_STOP,
            Some(owner),
        )
    };
    assert!(register(1, owner.clone()).unwrap().is_none());
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32601);
    negotiate(&state);
    let mut foreign = owner.clone();
    foreign.process_generation += 1;
    assert!(register(2, foreign).unwrap().is_none());
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32002);
    let accepted = register(3, owner.clone()).unwrap().unwrap();
    assert_eq!(accepted.parent_request_id, None);
    assert_eq!(accepted.resource_owner.as_ref(), Some(&owner));
    lock_std_mutex(&state.issued_resource_owners).remove(&owner);
    assert!(register(4, owner).unwrap().is_none());
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32002);
}

#[test]
fn retained_child_calls_do_not_bypass_live_or_cancelled_parent_ownership() {
    let (events, _) = broadcast::channel(8);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    negotiate(&state);
    let owner = issued_owner(&state, "owner");
    let other = issued_owner(&state, "other");
    insert_test_parent(&state, 10, Some(owner.clone()));
    let register = |id, owner| {
        register_agent_session_request(
            &state,
            ExtensionRequestId::Number(id),
            10,
            methods::AGENT_STOP,
            Some(owner),
        )
    };
    assert!(register(1, other).unwrap().is_none());
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32002);
    let accepted = register(2, owner.clone()).unwrap().unwrap();
    assert_eq!(
        accepted.parent_request_id,
        Some(10),
        "no live-parent lifetime exemption"
    );
    lock_std_mutex(&state.pending).remove(&10);
    lock_std_mutex(&state.tombstones).insert(10, Duration::from_secs(60));
    assert!(register(3, owner).unwrap().is_none());
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32002);
}

#[tokio::test]
async fn child_events_and_stop_have_real_dispatch_and_strict_wire_gates() {
    let (events, _) = broadcast::channel(8);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    let owner = issued_owner(&state, "owner");
    let call = |id, method, params| {
        handle_protocol_line(&wave1_line(id, method, params), &state).unwrap();
    };
    call(
        1,
        methods::AGENT_EVENTS,
        serde_json::json!({"parent_request_id":10,
        "resource_owner":owner, "target":"agent-1", "after_sequence":0}),
    );
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32601);
    negotiate(&state);
    call(
        2,
        methods::AGENT_EVENTS,
        serde_json::json!({"parent_request_id":10,
        "resource_owner":owner, "target":"agent-1", "after_sequence":0, "timeout_ms":25001}),
    );
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32602);
    call(
        3,
        methods::AGENT_STOP,
        serde_json::json!({"parent_request_id":10,
        "resource_owner":owner, "target":"agent-1", "authority":"full"}),
    );
    assert_eq!(wave1_error(&frames.try_recv().unwrap()).0, -32602);
    for (id, method) in [(4, methods::AGENT_EVENTS), (5, methods::AGENT_STOP)] {
        let mut params = serde_json::json!({"parent_request_id":10, "resource_owner":owner,
            "target":"agent-1"});
        if method == methods::AGENT_EVENTS {
            params["after_sequence"] = serde_json::json!(0);
        }
        call(id, method, params);
        let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .unwrap()
            .unwrap();
        let response: serde_json::Value = serde_json::from_slice(&frame.line).unwrap();
        assert_eq!(response["error"]["code"], -32002);
        assert_eq!(
            response["error"]["message"],
            "agent session service is not bound to this host-owned resource owner"
        );
        assert!(
            response.get("result").is_none(),
            "no fabricated child success"
        );
    }
}

#[test]
fn child_profiles_require_api_04_and_an_authorized_agent_service() {
    for version in [EXTENSION_API_VERSION_0_2, EXTENSION_API_VERSION_0_4] {
        let manifest = ExtensionManifest::parse(&format!(
            "name = 'child-profile'\nversion = '0.1.0'\napi_version = '{version}'\n[entrypoint]\ncommand = 'child-profile'\n"
        )).unwrap();
        let response = || InitializeResponse {
            api_version: version.into(),
            tools: Vec::new(),
            commands: Vec::new(),
            tool_renderers: Vec::new(),
            shortcuts: Vec::new(),
            protocol: Some(ExtensionProtocolResponse {
                version: version.into(),
                features: API_0_2_REQUIRED_FEATURES
                    .iter()
                    .copied()
                    .chain([
                        EXTENSION_FEATURE_AGENT_SESSIONS,
                        EXTENSION_FEATURE_AGENT_SESSION_EVENTS_V1,
                        EXTENSION_FEATURE_AGENT_SESSION_LIFETIME_V1,
                    ])
                    .map(str::to_owned)
                    .collect(),
                limits: ExtensionProtocolLimits {
                    resource_refs_v1: None,
                    max_concurrent_requests: 1,
                },
                lifecycle_events: Vec::new(),
            }),
        };
        assert!(
            negotiate_contributions_with_host_services(
                &manifest,
                response(),
                DEFAULT_PENDING_REQUESTS,
                OfferedHostServices::default()
            )
            .is_err()
        );
        let offered = OfferedHostServices {
            agent_sessions: true,
            ..OfferedHostServices::default()
        };
        assert_eq!(
            negotiate_contributions_with_host_services(
                &manifest,
                response(),
                DEFAULT_PENDING_REQUESTS,
                offered
            )
            .is_ok(),
            version == EXTENSION_API_VERSION_0_4
        );
        if version == EXTENSION_API_VERSION_0_4 {
            let mut missing_base = response();
            missing_base
                .protocol
                .as_mut()
                .unwrap()
                .features
                .retain(|f| f != EXTENSION_FEATURE_AGENT_SESSIONS);
            assert!(
                negotiate_contributions_with_host_services(
                    &manifest,
                    missing_base,
                    DEFAULT_PENDING_REQUESTS,
                    OfferedHostServices {
                        agent_sessions: true,
                        ..OfferedHostServices::default()
                    }
                )
                .is_err()
            );
        }
    }
}
