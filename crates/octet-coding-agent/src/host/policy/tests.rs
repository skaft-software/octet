use super::*;

use std::collections::HashMap;
use std::path::PathBuf;

use octet_agent::{PolicyValueSource, ToolPolicyProvenance};
use octet_ai::Protocol;

use super::super::protocol::{
    HostInputModality, MediaInput, RunRequest, SeedMessage, SeedRole, MAX_CUSTOM_HEADER_BYTES,
    MAX_HISTORY_MESSAGES, MAX_PROMPT_DISPLAY_BYTES,
};

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
fn controlled_host_config_keeps_model_tools_workspace_relative() {
    let workspace = tempfile::tempdir().unwrap();
    let mut request = base_request(workspace.path().to_path_buf());
    request.allow_external_paths = true;

    let config = host_config(&request).unwrap();

    assert_eq!(config.effect_policy, octet_agent::EffectPolicy::Controlled);
    assert!(!config.sandbox.allow_external_paths);
    assert!(
        !config.experimental_streamable_http_mcp,
        "the host request/session boundary cannot enable experimental remote MCP"
    );
    assert_eq!(
        config.sandbox.policy_provenance,
        ToolPolicyProvenance::all(PolicyValueSource::HostRequest)
    );
}

#[test]
fn run_request_rejects_unbounded_history() {
    let mut request = base_request(PathBuf::from("."));
    request.history = (0..=MAX_HISTORY_MESSAGES)
        .map(|_| SeedMessage {
            role: SeedRole::User,
            text: "x".into(),
        })
        .collect();
    assert!(validate_run_request(&request).is_err());
}

#[test]
fn run_request_rejects_malformed_or_unbounded_display_text() {
    let mut request = base_request(PathBuf::from("."));
    request.prompt_display_text = Some("x".repeat(MAX_PROMPT_DISPLAY_BYTES + 1));
    assert!(validate_run_request(&request).is_err());

    request.prompt_display_text = Some("caller\u{1b}[31m".into());
    assert!(validate_run_request(&request).is_err());

    request.prompt_display_text = Some(String::new());
    assert!(validate_run_request(&request).is_ok());
}

#[test]
fn inline_routes_reject_unsafe_urls_modes_and_headers() {
    for url in [
        "file:///tmp/provider",
        "https://user@example.com/v1",
        "https://example.com/v1?token=secret",
        "https://example.com/v1#fragment",
    ] {
        assert!(parse_inline_base_url(url).is_err(), "accepted {url}");
    }
    assert_eq!(
        inline_protocol(Some("openai_responses")).unwrap(),
        Protocol::OpenAiResponses
    );
    assert!(inline_protocol(Some("unknown")).is_err());

    let mut headers = HashMap::from([("Connection".to_owned(), "close".to_owned())]);
    assert!(build_inline_headers(&headers).is_err());
    headers = HashMap::from([("x-test".to_owned(), "x".repeat(1024))]);
    assert!(build_inline_headers(&headers).is_ok());
    headers.insert("x-extra".to_owned(), "x".repeat(MAX_CUSTOM_HEADER_BYTES));
    assert!(build_inline_headers(&headers).is_err());
}

#[test]
fn inline_media_validation_is_route_effective_and_actionable() {
    let mut request = base_request(PathBuf::from("."));
    request.base_url = Some("https://example.com/v1".into());
    request.input_modalities = vec![HostInputModality::Audio];
    request.media = vec![MediaInput::Audio {
        path: PathBuf::from("voice.flac"),
    }];

    let error = {
        request.provider_mode = Some("openai-compatible".into());
        validate_run_request(&request).unwrap_err()
    };
    assert!(error.to_string().contains("only WAV or MP3"));
    assert!(error.to_string().contains("FLAC"));

    request.provider_mode = Some("openai-responses".into());
    let error = validate_run_request(&request).unwrap_err();
    assert!(error.to_string().contains("OpenAiResponses"));
    assert!(error.to_string().contains("WAV or MP3"));

    request.provider_mode = Some("openai-compatible".into());
    request.media = vec![MediaInput::Audio {
        path: PathBuf::from("voice.wav"),
    }];
    request.input_modalities.clear();
    let error = validate_run_request(&request).unwrap_err();
    assert!(error
        .to_string()
        .contains("input_modalities to include \"audio\""));

    request.input_modalities = vec![HostInputModality::Audio];
    assert!(validate_run_request(&request).is_ok());
}
