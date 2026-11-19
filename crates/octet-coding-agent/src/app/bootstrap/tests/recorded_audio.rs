//! Offline attachment/read/session-to-wire qualification for recorded audio.
use super::*;
use octet_agent::{
    Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig, Session, UserInput,
};
use octet_ai::{AiClient, AudioFormat, Auth, Media, Message, UserPart};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn response(protocol: Protocol, read: bool, file: &str) -> ResponseTemplate {
    let body = if protocol == Protocol::GoogleGenerativeAi {
        let part = if read {
            serde_json::json!({"functionCall": {"name": "read", "args": {"path": file}}})
        } else {
            serde_json::json!({"text": "heard"})
        };
        format!(
            "data: {}\n\n",
            serde_json::json!({"candidates": [{"content": {"role": "model", "parts": [part]}, "finishReason": "STOP"}]})
        )
    } else if read {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "read-clip", "type": "function", "function": {"name": "read", "arguments": serde_json::json!({"path": file}).to_string()}}]}}]}),
            serde_json::json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
        )
    } else {
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"heard\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into()
    };
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn agent(model: octet_ai::Model, session: Session, workspace: &std::path::Path) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    Agent::new(AgentConfig {
        client: AiClient::with_http_client(reqwest::Client::builder().no_proxy().build().unwrap()),
        reasoning: crate::app::default_reasoning_for_model(&model),
        model,
        session,
        system: String::new(),
        sandbox: SandboxConfig::new(workspace),
        effect_broker: EffectBroker::default(),
        extensions,
        max_turns: Some(3),
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::None,
        session_id: None,
    })
    .unwrap()
}

fn audio_parts(value: &serde_json::Value, protocol: Protocol) -> Vec<serde_json::Value> {
    let mut found = Vec::new();
    fn visit(value: &serde_json::Value, protocol: Protocol, out: &mut Vec<serde_json::Value>) {
        if let Some(object) = value.as_object() {
            let key = if protocol == Protocol::OpenAiChat {
                "input_audio"
            } else {
                "inlineData"
            };
            if let Some(audio) = object.get(key) {
                out.push(audio.clone());
            }
            for child in object.values() {
                visit(child, protocol, out);
            }
        } else if let Some(array) = value.as_array() {
            for child in array {
                visit(child, protocol, out);
            }
        }
    }
    visit(value, protocol, &mut found);
    found
}

#[tokio::test]
async fn recorded_audio_attachment_and_read_replay_identical_bytes_through_resume() {
    use crate::tui::composer::{compose, AttachmentLedger};
    use base64::Engine as _;
    for (declaration, protocol, api_name) in [
        (
            &crate::providers::OPENROUTER,
            Protocol::OpenAiChat,
            "fixture/recorded-audio-new",
        ),
        (
            &crate::providers::OPENCODE,
            Protocol::GoogleGenerativeAi,
            "gemini-recorded-audio-new",
        ),
        (
            &crate::providers::OPENCODE_GO,
            Protocol::GoogleGenerativeAi,
            "gemini-4-argon",
        ),
        (
            crate::providers::BUILTIN_PROVIDER_DECLARATIONS
                .iter()
                .find(|provider| provider.id == "gemini")
                .unwrap(),
            Protocol::GoogleGenerativeAi,
            "gemini-recorded-audio-new",
        ),
        (
            crate::providers::BUILTIN_PROVIDER_DECLARATIONS
                .iter()
                .find(|provider| provider.id == "vertex")
                .unwrap(),
            Protocol::GoogleGenerativeAi,
            "gemini-recorded-audio-new",
        ),
    ] {
        for cached in [false, true] {
            for &(format, extension, mime, bytes) in &[
                (
                    AudioFormat::Wav,
                    "wav",
                    "audio/wav",
                    &b"RIFF\x04\0\0\0WAVEoriginal"[..],
                ),
                (
                    AudioFormat::Mp3,
                    "mp3",
                    "audio/mp3",
                    &b"ID3\x04\0\0\0\0\0\0original"[..],
                ),
                (
                    AudioFormat::Aac,
                    "aac",
                    "audio/aac",
                    &b"\xff\xf1\x50\x80original"[..],
                ),
                (
                    AudioFormat::Flac,
                    "flac",
                    "audio/flac",
                    &b"fLaCoriginal"[..],
                ),
                (
                    AudioFormat::Opus,
                    "ogg",
                    "audio/ogg",
                    &b"OggS\0originalOpusHead"[..],
                ),
            ] {
                if !protocol.audio_input_formats().contains(&format) {
                    continue;
                }
                let workspace = tempfile::tempdir().unwrap();
                let file = format!("clip.{extension}");
                let path = workspace.path().join(&file);
                let attachment_bytes = [bytes, b"-attachment"].concat();
                std::fs::write(&path, &attachment_bytes).unwrap();
                let server = MockServer::start().await;
                let calls = Arc::new(AtomicUsize::new(0));
                let counter = calls.clone();
                let read_file = file.clone();
                Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(move |_: &wiremock::Request| {
                        response(
                            protocol,
                            counter.fetch_add(1, Ordering::SeqCst) == 0,
                            &read_file,
                        )
                    })
                    .mount(&server)
                    .await;
                // Newly advertised IDs deliberately have no entry in the pinned
                // models.dev snapshot: a stale snapshot must not deny live audio.
                let fresh = if declaration.id == "opencode-go" {
                    // The Go /models entry for this exact route uses MIME types, not "audio".
                    serde_json::json!({"data": [{"id": api_name, "modalities": {
                        "input": ["text", "image/png", "video/mp4", "audio/wav", "audio/mp3",
                                  "audio/aac", "audio/ogg", "audio/flac", "application/pdf"],
                        "output": ["text"]}, "capabilities": {"tools": {"supported": true}}}]})
                } else {
                    serde_json::json!({"data": [{"id": api_name,
                        "input_modalities": ["text", "audio"], "tools": true, "supported_parameters": ["tools"],
                        "context_window": 128000, "max_output_tokens": 4096}]})
                };
                let inventory = if cached {
                    serde_json::from_slice(&serde_json::to_vec(&fresh).unwrap()).unwrap()
                } else {
                    fresh
                };
                let mut catalog = ModelCatalog::default();
                crate::providers::register_private_endpoints_at_base_url(
                    &mut catalog,
                    declaration,
                    Auth::None,
                    &url::Url::parse(&format!("{}/", server.uri())).unwrap(),
                    Duration::from_secs(5),
                )
                .unwrap();
                if declaration.id == "openrouter" {
                    for spec in openrouter_models_from_response(declaration, &inventory).unwrap() {
                        catalog.register_model(spec).unwrap();
                    }
                } else {
                    register_openai_compatible_models_from_response(
                        &mut catalog,
                        declaration,
                        ModelFilter::All,
                        &inventory,
                    )
                    .unwrap();
                }
                let id = ModelId(format!("{}/{api_name}", declaration.id));
                // The picker consumes the same effective catalog spec as admission.
                let picked = catalog.models().find(|spec| spec.id == id).unwrap();
                assert!(picked
                    .effective_input_modalities()
                    .contains(octet_ai::Modality::Audio));
                assert_eq!(
                    picked.preset.audio_input_source,
                    Some(octet_ai::model_metadata::AudioInputSource::Declaration)
                );
                let model = catalog.resolve(&id).unwrap();
                assert_eq!(model.spec.protocol, protocol);
                let mut ledger = AttachmentLedger::default();
                let chip = ledger
                    .attach_media(
                        &path,
                        model.spec.effective_input_modalities(),
                        model.spec.audio_capabilities().unwrap().input_formats,
                    )
                    .unwrap();
                let input = compose(format!("before {chip} after"), &mut ledger);
                std::fs::write(&path, bytes).unwrap();
                let session_path = workspace.path().join("session.jsonl");
                let mut active = agent(
                    model.clone(),
                    Session::create(&session_path).unwrap(),
                    workspace.path(),
                );
                assert_eq!(
                    active
                        .complete(UserInput::from(input.parts))
                        .await
                        .unwrap()
                        .text,
                    "heard"
                );
                // The first request contains the attachment. The tool continuation has
                // both copies, in order, with a tool response before its adjacent audio.
                let requests = server.received_requests().await.unwrap();
                assert_eq!(requests.len(), 2);
                let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
                let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
                let expected = if protocol == Protocol::OpenAiChat {
                    serde_json::json!({"data": base64::engine::general_purpose::STANDARD.encode(bytes), "format": extension})
                } else {
                    serde_json::json!({"data": base64::engine::general_purpose::STANDARD.encode(bytes), "mimeType": mime})
                };
                let mut attachment = expected.clone();
                attachment["data"] = base64::engine::general_purpose::STANDARD
                    .encode(&attachment_bytes)
                    .into();
                assert_eq!(audio_parts(&first, protocol), vec![attachment.clone()]);
                assert_eq!(
                    audio_parts(&second, protocol),
                    vec![attachment.clone(), expected.clone()]
                );
                let serialized = second.to_string();
                assert!(serialized.contains(if protocol == Protocol::OpenAiChat {
                    "\"role\":\"tool\""
                } else {
                    "functionResponse"
                }));
                std::fs::write(&path, b"changed after read").unwrap();
                active.complete("next turn").await.unwrap();
                drop(active);
                std::fs::remove_file(&path).unwrap();
                let mut resumed = agent(
                    model,
                    Session::open(&session_path).unwrap(),
                    workspace.path(),
                );
                resumed.complete("after resume").await.unwrap();
                for request in server.received_requests().await.unwrap().iter().skip(2) {
                    let body = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(
                        audio_parts(&body, protocol),
                        vec![attachment.clone(), expected.clone()]
                    );
                }
                let context = resumed.session().context().unwrap();
                assert!(context.iter().any(|message| matches!(message, Message::User(user) if user.content.iter().any(|p| matches!(p, UserPart::Media(Media::Audio(_)))))));
            }
        }
    }
}
