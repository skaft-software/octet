use super::*;

use std::collections::HashMap;
use std::path::PathBuf;

use super::super::protocol::{RunRequest, MAX_ID_BYTES};

fn base_request(workspace: PathBuf) -> RunRequest {
    RunRequest {
        run_id: "run".into(),
        session_id: None,
        workspace,
        working_dir: None,
        session_dir: None,
        resume_session: None,
        model: "model".into(),
        provider: None,
        base_url: None,
        api_key: None,
        custom_headers: HashMap::new(),
        provider_mode: None,
        context_window_tokens: None,
        max_output_tokens: None,
        vision: false,
        input_modalities: Vec::new(),
        supports_reasoning: false,
        prompt: "prompt".into(),
        prompt_display_text: None,
        system_prompt: None,
        reasoning: None,
        tools: None,
        allow_file_mutation: true,
        allow_external_paths: false,
        context_files: true,
        offline: true,
        max_turns: None,
        max_cost_microdollars: None,
        history: Vec::new(),
        media: Vec::new(),
        image_paths: Vec::new(),
        prompt_paths: Vec::new(),
        skill_paths: Vec::new(),
        extension_paths: Vec::new(),
        enabled_extensions: Vec::new(),
        trusted_extensions: Vec::new(),
    }
}

#[test]
fn ids_are_bounded_and_cannot_be_paths() {
    assert!(super::valid_session_id("session-1.turn_2"));
    assert!(!super::valid_session_id("session:stream"));
    assert!(!super::valid_session_id("../session"));
    assert!(!super::valid_session_id(""));
    assert!(!super::valid_session_id(&"x".repeat(MAX_ID_BYTES + 1)));
}

#[test]
fn session_paths_are_confined_and_regular() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sessions");
    let outside = root.path().join("outside.jsonl");
    std::fs::write(&outside, b"").unwrap();
    let mut request = base_request(root.path().to_path_buf());
    request.resume_session = Some(outside);
    let error = session_selection(&sessions, &request).unwrap_err();
    assert!(error
        .to_string()
        .contains("must stay inside the configured session directory"));

    let directory = sessions.join("not-a-file.jsonl");
    std::fs::create_dir(&directory).unwrap();
    request.resume_session = Some(directory);
    let error = session_selection(&sessions, &request).unwrap_err();
    assert!(error.to_string().contains("must be a regular file"));
}

#[cfg(unix)]
#[test]
fn session_paths_reject_final_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let target = sessions.join("target.jsonl");
    let link = sessions.join("session.jsonl");
    std::fs::write(&target, b"").unwrap();
    symlink(&target, &link).unwrap();

    let mut request = base_request(root.path().to_path_buf());
    request.resume_session = Some(link);
    let error = session_selection(&sessions, &request).unwrap_err();
    assert!(error.to_string().contains("must not be a symbolic link"));
}
