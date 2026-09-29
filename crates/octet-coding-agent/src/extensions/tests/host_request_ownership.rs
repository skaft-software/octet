//! Host requests, and the fence that decides who may answer them.
//!
//! An extension can ask the host to read input, run a command, or show a UI, but
//! only while it owns the foreground session. These tests cover request
//! validation bounds, the feature/operation tables, refusal of reserved host key
//! bindings, the foreground-owner fence, and the rule that status contributions
//! never leak into the ambient message stream.

use super::*;

#[test]
fn host_request_validation_enforces_bounds_and_required_names() {
    assert!(validate_host_request(&HostRequestOperation::Composer(
        ExtensionComposerOperation::Get
    ))
    .is_ok());

    let oversized = "x".repeat(MAX_EXTENSION_COMPOSER_TEXT_BYTES + 1);
    let error = validate_host_request(&HostRequestOperation::Composer(
        ExtensionComposerOperation::Set { text: oversized },
    ))
    .unwrap_err();
    assert!(matches!(error.0, ExtensionRequestFailure::BoundsExceeded));

    let names = vec!["read".to_owned(); MAX_HOST_REQUEST_TOOL_NAMES + 1];
    let error = validate_host_request(&HostRequestOperation::ActiveTools { names }).unwrap_err();
    assert!(matches!(error.0, ExtensionRequestFailure::BoundsExceeded));

    let error = validate_host_request(&HostRequestOperation::ActiveTools {
        names: vec![String::new()],
    })
    .unwrap_err();
    assert!(matches!(error.0, ExtensionRequestFailure::InvalidRequest));

    let error = validate_host_request(&HostRequestOperation::SessionEntry(
        ExtensionSessionEntryOperation::Append {
            entry_type: "note".to_owned(),
            data: serde_json::json!({ "note": "y".repeat(MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES) }),
        },
    ))
    .unwrap_err();
    assert!(matches!(error.0, ExtensionRequestFailure::BoundsExceeded));
}

#[test]
fn host_request_features_and_operation_names_match_the_wire_contract() {
    assert_eq!(
        host_request_feature(&HostRequestOperation::Composer(
            ExtensionComposerOperation::Insert {
                text: String::new(),
            },
        )),
        "composer"
    );
    assert_eq!(EXTENSION_FEATURE_SHORTCUTS, "shortcuts");
    assert_eq!(EXTENSION_FEATURE_SESSION_ENTRIES, "session_entries");
    assert_eq!(EXTENSION_FEATURE_MESSAGE_INJECTION, "message_injection");
    assert_eq!(EXTENSION_FEATURE_ACTIVE_TOOLS, "active_tools");
    assert_eq!(
        host_request_operation_name(&HostRequestOperation::ActiveTools { names: Vec::new() }),
        "active_tools"
    );
}

#[test]
fn reserved_host_bindings_are_refused_with_a_typed_error() {
    match dynamic_shortcut_binding("ctrl+shift+c") {
        Ok(_) => panic!("the host keymap reserves ctrl+shift+c"),
        Err((outcome, diagnostic)) => {
            assert!(matches!(outcome, ExtensionRequestOutcome::Failed(..)));
            assert!(diagnostic.contains("reserves this binding"));
        }
    }
    assert!(dynamic_shortcut_binding("ctrl+shift+p").is_ok());
    assert!(dynamic_shortcut_binding("shift+p").is_err());
}

#[test]
fn host_request_owner_fence_requires_the_foreground_owner() {
    let owner = octet_agent::extension_process::ExtensionResourceOwner {
        session_id: "owner-a".into(),
        extension_instance_id: "instance-a".into(),
        process_generation: 3,
    };
    assert!(host_request_owner_is_foreground(
        Some(&owner),
        Some("owner-a")
    ));
    assert!(!host_request_owner_is_foreground(
        Some(&owner),
        Some("owner-b")
    ));
    assert!(!host_request_owner_is_foreground(Some(&owner), None));
    assert!(!host_request_owner_is_foreground(None, Some("owner-a")));
}

#[test]
fn status_contributions_never_become_ambient_messages() {
    let (sender, receiver) = broadcast::channel(4);
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(receiver);
    sender
        .send(ExtensionEvent::StatusContributed {
            contribution: octet_agent::extension_process::ExtensionStatusContribution {
                surface: ExtensionUiSurface::Footer,
                text: "persistent extension status".into(),
                style_role: None,
                priority: 0,
            },
        })
        .unwrap();

    assert!(extensions.drain_events().is_empty());
}
