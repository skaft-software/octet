//! Host-side policy answers: status, MCP gating, and snapshot wire names.
//!
//! Covers the compatibility marker first-party status reports when delegation
//! telemetry is missing, the host-owned streamable-HTTP MCP feature gate, and the
//! operation-name/feature tables that must match the wire contract exactly (they
//! are what the `context_snapshot_operations_match_the_wire_contract` and
//! `mcp_policy_target_requires_*` tests pin against).

use super::*;

#[test]
fn first_party_status_marks_missing_delegation_telemetry_incompatible() {
    let (schema, compatibility) = extension_compatibility(
        SUBAGENTS_EXTENSION_NAME,
        true,
        &[EXTENSION_FEATURE_AGENT_SESSIONS.to_owned()],
        None,
    );
    assert_eq!(schema, None);
    assert!(compatibility.contains("rebuild/reinstall"));

    let (schema, compatibility) = extension_compatibility(
        SUBAGENTS_EXTENSION_NAME,
        true,
        &[
            EXTENSION_FEATURE_AGENT_SESSIONS.to_owned(),
            EXTENSION_FEATURE_DELEGATION_TELEMETRY.to_owned(),
        ],
        None,
    );
    assert_eq!(schema.as_deref(), Some(DELEGATION_TELEMETRY_SCHEMA));
    assert_eq!(compatibility, "compatible");
}

#[test]
fn streamable_http_mcp_gate_is_host_owned() {
    let mut descriptor = DiscoveredExtension {
        manifest: ExtensionManifest::parse(
            r#"
name = "octet-mcp"
version = "0.1.0"
api_version = "0.2"

[entrypoint]
command = "fixture"
args = ["--keep", "--experimental-streamable-http-mcp"]
"#,
        )
        .unwrap(),
        manifest_path: PathBuf::from("/tmp/octet-mcp/extension.toml"),
        source: ExtensionSource::Explicit,
        activation: Default::default(),
    };

    apply_experimental_streamable_http_mcp_gate(&mut descriptor, false);
    assert_eq!(
        descriptor.manifest.entrypoint.args,
        vec!["--keep".to_owned()]
    );

    apply_experimental_streamable_http_mcp_gate(&mut descriptor, true);
    assert_eq!(
        descriptor.manifest.entrypoint.args,
        vec![
            "--keep".to_owned(),
            EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT.to_owned(),
        ]
    );
}

#[test]
fn context_snapshot_operations_match_the_wire_contract() {
    let session_context =
        HostRequestOperation::ContextSnapshot(ExtensionContextOperation::SessionManager);
    assert_eq!(host_request_feature(&session_context), "session_context");
    assert_eq!(
        host_request_operation_name(&session_context),
        "session_manager"
    );
    assert_eq!(EXTENSION_FEATURE_SESSION_CONTEXT, "session_context");

    let pending = HostRequestOperation::ContextSnapshot(ExtensionContextOperation::PendingMessages);
    assert_eq!(host_request_feature(&pending), "session_context");
    assert_eq!(host_request_operation_name(&pending), "pending_messages");

    let prompt = HostRequestOperation::ContextSnapshot(ExtensionContextOperation::SystemPrompt);
    assert_eq!(host_request_feature(&prompt), "system_prompt_read");
    assert_eq!(host_request_operation_name(&prompt), "system_prompt");
    assert_eq!(EXTENSION_FEATURE_SYSTEM_PROMPT_READ, "system_prompt_read");
    assert!(validate_host_request(&session_context).is_ok());
    assert!(validate_host_request(&pending).is_ok());
    assert!(validate_host_request(&prompt).is_ok());
}

#[test]
fn mcp_policy_target_requires_server_scoped_tool_and_object_arguments() {
    let valid = serde_json::json!({"server":"my-server", "tool":"mcp_my_server_mutate_0123456789", "arguments":{}});
    assert!(mcp_policy_target(&valid).is_some());
    // A short server label can share a namespace prefix. It is deliberately
    // not a per-server permission grant: the full published tool and exact
    // arguments must still match the host's active call digest.
    let overlapping = serde_json::json!({"server":"my", "tool":"mcp_my_server_mutate_0123456789", "arguments":{}});
    assert_eq!(mcp_policy_target(&valid), mcp_policy_target(&overlapping));
    for invalid in [
        serde_json::json!({"server":"other", "tool":"mcp_my_server_mutate_0123456789", "arguments":{}}),
        serde_json::json!({"server":"../server", "tool":"mcp_server_mutate", "arguments":{}}),
        serde_json::json!({"server":"", "tool":"mcp__mutate", "arguments":{}}),
        serde_json::json!({"server":"my-server", "tool":"bash", "arguments":{}}),
        serde_json::json!({"server":"my-server", "tool":"mcp_my_server_mutate", "arguments":[]}),
    ] {
        assert!(mcp_policy_target(&invalid).is_none(), "{invalid}");
    }
}
