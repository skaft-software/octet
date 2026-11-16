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
        model,
        session,
        system: String::new(),
        sandbox: SandboxConfig::new(workspace),
        effect_broker: EffectBroker::default(),
        extensions,
        max_turns: Some(3),
        reasoning: octet_ai::ReasoningConfig::Off,
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
    for protocol in [Protocol::OpenAiChat, Protocol::GoogleGenerativeAi] {
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
            let mut catalog = ModelCatalog::builtin().unwrap();
            let mut spec = (*catalog
                .resolve(&ModelId("gpt-4o-mini".into()))
                .unwrap()
                .spec)
                .clone();
            spec.id = ModelId("recorded-audio".into());
            spec.api_name = "fixture-audio".into();
            spec.protocol = protocol;
            spec.preset.audio_input = Some(true);
            spec.capabilities.reasoning = None;
            catalog.register_model(spec).unwrap();
            let mut model = catalog.resolve(&ModelId("recorded-audio".into())).unwrap();
            Arc::make_mut(&mut model.endpoint).base_url =
                url::Url::parse(&format!("{}/", server.uri())).unwrap();
            Arc::make_mut(&mut model.endpoint).auth = Auth::None;
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
