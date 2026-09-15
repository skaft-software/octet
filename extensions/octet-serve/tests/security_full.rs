use bytes::Bytes;
use octet_serve_backend::{
    CommandId, DeviceId, ErrorCode, HostId, PromptInput, ProtocolValidation, ResourceStore,
    ResourceStoreError, SanitizedError, ServiceError, SessionCommand, SessionCommandEnvelope,
    SessionId, MAX_PROMPT_BYTES,
};
use tempfile::tempdir;

fn prompt_command(session_id: SessionId, command_id: &str) -> SessionCommandEnvelope {
    SessionCommandEnvelope::new(
        HostId::new("security-full-host").unwrap(),
        DeviceId::new("security-full-device").unwrap(),
        session_id,
        CommandId::new(command_id).unwrap(),
        1,
        Some(1),
        SessionCommand::SubmitPrompt {
            input: PromptInput {
                text: "bounded security prompt".into(),
                attachments: Vec::new(),
                document_ids: Vec::new(),
                project_file_ids: Vec::new(),
            },
        },
    )
}

#[test]
fn resources_are_opaque_session_scoped_and_reopenable() {
    let directory = tempdir().unwrap();
    let owner = SessionId::new("security-resource-owner").unwrap();
    let other = SessionId::new("security-resource-other").unwrap();
    assert!(SessionId::new("../private-session").is_err());

    let store = ResourceStore::open(directory.path()).unwrap();
    let reference = store
        .register(
            &owner,
            "tool-call-full",
            "stdout",
            r"..\private\report.txt",
            "text/plain",
            Bytes::from_static(b"authoritative bytes"),
        )
        .unwrap();

    assert_eq!(reference.display_name, "report.txt");
    assert_eq!(reference.handle.len(), 64);
    assert!(reference
        .handle
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    assert_eq!(reference.sha256.len(), 64);
    assert!(reference
        .sha256
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));

    assert_eq!(
        store.content(&other, &reference.handle),
        Err(ResourceStoreError::NotFound)
    );
    assert_eq!(
        store.content(&owner, "../report.txt"),
        Err(ResourceStoreError::NotFound)
    );
    assert_eq!(
        store.register(
            &owner,
            "tool-call-full",
            "../stdout",
            "report.txt",
            "text/plain",
            Bytes::from_static(b"rejected slot"),
        ),
        Err(ResourceStoreError::InvalidBoundary)
    );

    let content = store.content(&owner, &reference.handle).unwrap();
    assert_eq!(content.display_name, "report.txt");
    assert_eq!(content.media_type, "text/plain");
    assert_eq!(content.bytes, Bytes::from_static(b"authoritative bytes"));
    assert_eq!(content.sha256, reference.sha256);

    assert_eq!(
        store.register(
            &owner,
            "tool-call-full",
            "stdout",
            "different-name.txt",
            "text/plain",
            Bytes::from_static(b"changed bytes"),
        ),
        Err(ResourceStoreError::Storage)
    );

    let handle = reference.handle.clone();
    drop(store);
    let reopened = ResourceStore::open(directory.path()).unwrap();
    assert_eq!(reopened.content(&owner, &handle).unwrap(), content);
    assert_eq!(
        reopened.content(&other, &handle),
        Err(ResourceStoreError::NotFound)
    );
}

#[test]
fn public_command_dtos_fail_closed_at_unknown_and_size_boundaries() {
    let session_id = SessionId::new("security-command-session").unwrap();
    let command = prompt_command(session_id.clone(), "strict-command");
    let mut wire = serde_json::to_value(&command).unwrap();
    wire.as_object_mut().unwrap().insert(
        "privatePath".into(),
        serde_json::Value::String("/tmp/secret".into()),
    );
    assert!(serde_json::from_value::<SessionCommandEnvelope>(wire).is_err());

    let mut oversized = prompt_command(session_id.clone(), "oversized-command");
    let SessionCommand::SubmitPrompt { input } = &mut oversized.command else {
        panic!("fixture command must remain a prompt");
    };
    input.text = "x".repeat(MAX_PROMPT_BYTES + 1);
    assert!(oversized.validate().is_err());

    let mut missing_generation = prompt_command(session_id, "missing-generation");
    missing_generation.expected_actor_generation = None;
    assert!(missing_generation.validate().is_err());
}

#[test]
fn internal_service_failures_are_sanitized_and_public_errors_reject_extra_fields() {
    let error = ServiceError::OwnerLost.into_public();
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(
        error.message,
        "The session host could not complete the request."
    );
    assert!(!error.message.contains("OwnerLost"));

    let mut wire = serde_json::to_value(&error).unwrap();
    wire.as_object_mut().unwrap().insert(
        "privateSource".into(),
        serde_json::Value::String("/home/user/.config/token".into()),
    );
    assert!(serde_json::from_value::<SanitizedError>(wire).is_err());

    let controls = SanitizedError::public(ErrorCode::InvalidBoundary, "bad\nmessage\t");
    assert!(!controls
        .message
        .chars()
        .any(|character| character.is_control()));
}
