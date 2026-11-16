use super::*;
use crate::{ModelCatalog, ModelId, Protocol, UserMessage};
use base64::Engine as _;

fn clip() -> Media {
    Media::video_bytes(
        Bytes::from_static(include_bytes!("tiny.mp4")),
        "video/mp4".parse().unwrap(),
    )
}
fn model(protocol: Protocol) -> crate::Model {
    let mut model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let spec = Arc::make_mut(&mut model.spec);
    spec.protocol = protocol;
    spec.capabilities.reasoning = None;
    spec.preset = Default::default();
    if protocol == Protocol::GoogleGenerativeAi {
        spec.api_name = "gemini-video-fixture".into();
    }
    model
}
fn request() -> Request {
    Request {
        messages: vec![Message::User(UserMessage {
            content: vec![
                UserPart::Text("inspect clip".into()),
                UserPart::Media(clip()),
            ],
        })],
        ..crate::test_fixtures::base_request()
    }
}

#[test]
fn video_samples_are_timestamped_bounded_cached_and_rebuilt_from_durable_bytes() {
    let Media::Video(video) = clip() else {
        unreachable!()
    };
    let frames = video.sampled_frames().unwrap();
    assert_eq!(frames.len(), VIDEO_FRAMES);
    assert_eq!(
        frames.iter().map(|f| f.seconds).collect::<Vec<_>>(),
        [0.25, 0.75, 1.25, 1.75]
    );
    assert!(Arc::ptr_eq(
        &frames,
        &video.clone().sampled_frames().unwrap()
    ));
    let mut total = 0;
    for frame in frames.iter() {
        let crate::ImageSource::Inline(data) = &frame.image.source else {
            unreachable!()
        };
        total += data.len();
        let image = image::load_from_memory(data).unwrap();
        assert!(image.width() <= MAX_FRAME_EDGE && image.height() <= MAX_FRAME_EDGE);
        assert_eq!(frame.image.media_type, Some(mime::IMAGE_JPEG));
    }
    assert!(total <= MAX_FRAME_BYTES);
    let serialized = serde_json::to_vec(&Media::Video(video.clone())).unwrap();
    let Media::Video(resumed) = serde_json::from_slice::<Media>(&serialized).unwrap() else {
        unreachable!()
    };
    assert_eq!(resumed.data.as_ref(), include_bytes!("tiny.mp4"));
    assert!(resumed.frames.lock().unwrap().is_none());
    assert_eq!(
        serde_json::to_value(&frames[0].image).unwrap(),
        serde_json::to_value(&resumed.sampled_frames().unwrap()[0].image).unwrap()
    );
}

#[test]
fn video_wire_projection_uses_images_except_on_native_gemini() {
    for protocol in [
        Protocol::OpenAiResponses,
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages,
        Protocol::BedrockConverse,
        Protocol::PiMessages,
        Protocol::GoogleGenerativeAi,
    ] {
        let model = model(protocol);
        let req = request();
        let parts = match protocol {
            Protocol::OpenAiResponses => {
                crate::protocol::openai_responses::build_request(&model, &req)
            }
            Protocol::OpenAiChat => crate::protocol::openai_chat::build_request(&model, &req),
            Protocol::AnthropicMessages => crate::protocol::anthropic::build_request(&model, &req),
            Protocol::BedrockConverse => crate::protocol::bedrock::build_request(&model, &req),
            Protocol::PiMessages => crate::protocol::pi_messages::build_request(&model, &req),
            Protocol::GoogleGenerativeAi => crate::protocol::google::build_request(&model, &req),
            _ => unreachable!(),
        }
        .unwrap_or_else(|e| panic!("{protocol:?}: {e}"));
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap();
        let text = body.to_string();
        let original = base64::engine::general_purpose::STANDARD.encode(include_bytes!("tiny.mp4"));
        if protocol == Protocol::GoogleGenerativeAi {
            assert_eq!(
                body["contents"][0]["parts"][1]["inlineData"]["mimeType"],
                "video/mp4"
            );
            assert_eq!(
                body["contents"][0]["parts"][1]["inlineData"]["data"],
                original
            );
            let Message::User(user) = &req.messages[0] else {
                unreachable!()
            };
            let UserPart::Media(Media::Video(video)) = &user.content[1] else {
                unreachable!()
            };
            assert!(
                video.frames.lock().unwrap().is_none(),
                "native video must not need ffmpeg"
            );
        } else {
            assert!(
                text.contains("video sent as 4 frames"),
                "{protocol:?}: {text}"
            );
            assert!(text.contains("[video frame at 0.250s]"));
            assert!(!text.contains(&original));
            if protocol == Protocol::OpenAiResponses {
                assert_eq!(text.matches("input_image").count(), 4);
                assert!(!text.contains("input_video"));
            }
        }
        assert!(
            matches!(&req.messages[0], Message::User(user) if matches!(user.content[1], UserPart::Media(Media::Video(_))))
        );
    }
}

#[test]
fn video_responses_replay_retains_frame_order_after_resume() {
    let model = model(Protocol::OpenAiResponses);
    let req = request();
    let saved = serde_json::to_vec(&req.messages).unwrap();
    let messages: Vec<Message> = serde_json::from_slice(&saved).unwrap();
    let Message::User(user) = &messages[0] else {
        unreachable!()
    };
    let replay = crate::protocol::openai_responses::encode_replay_input(
        &model,
        None,
        &[crate::responses::ResponsesReplayItem::User(user.clone())],
    )
    .unwrap();
    let ordinary = crate::responses::encode_canonical_responses_input(
        &model,
        None,
        &messages,
        crate::CompatibilityMode::Strict,
    )
    .unwrap();
    assert_eq!(replay, ordinary);
    let text = serde_json::to_string(&replay).unwrap();
    let mut previous = 0;
    for timestamp in ["0.250s", "0.750s", "1.250s", "1.750s"] {
        let at = text.find(timestamp).unwrap();
        assert!(at > previous);
        previous = at;
    }
}

#[test]
fn video_bounds_missing_decoder_and_deadline_have_actionable_diagnostics() {
    let mut command = Command::new("octet-no-such-ffmpeg-executable");
    let message = run(&mut command, Instant::now() + DEADLINE)
        .unwrap_err()
        .to_string();
    assert!(message.contains("install ffmpeg and ffprobe on PATH"));
    let Media::Video(mut video) = clip() else {
        unreachable!()
    };
    video.data = Bytes::new();
    assert!(validate_video(&video)
        .unwrap_err()
        .to_string()
        .contains("10 MiB"));
    video.data = Bytes::from(vec![0; MAX_VIDEO_BYTES + 1]);
    assert!(validate_video(&video).is_err());
    #[cfg(unix)]
    {
        let error = run(Command::new("sleep").arg("2"), Instant::now()).unwrap_err();
        assert!(error.to_string().contains("30-second"));
    }
}

#[test]
fn video_model_switch_uses_placeholder_without_changing_original() {
    let mut target = model(Protocol::OpenAiResponses);
    Arc::make_mut(&mut target.spec)
        .capabilities
        .input_modalities = crate::ModalitySet::none();
    let request = request();
    let transformed = crate::transform_messages(&request.messages, &target);
    let pending =
        crate::transform::transform_request_messages_owned(request.messages.clone(), &target);
    assert_eq!(
        serde_json::to_value(&pending).unwrap(),
        serde_json::to_value(&transformed).unwrap()
    );
    let text = serde_json::to_string(&transformed).unwrap();
    assert!(text.contains("original retained"));
    assert!(text.contains("gpt-4o-mini"));
    assert!(!text.contains("Video"));
    assert!(
        matches!(&request.messages[0], Message::User(user) if matches!(user.content[1], UserPart::Media(Media::Video(_))))
    );
}

#[tokio::test]
async fn video_codex_loopback_resends_frames_from_resumed_original() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"video-fixture\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"video-fixture\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
        )))
        .expect(2).mount(&server).await;
    let mut model = model(Protocol::OpenAiResponses);
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.id = crate::EndpointId("openai-codex".into());
    endpoint.base_url = format!("{}/", server.uri()).parse().unwrap();
    endpoint.auth = crate::Auth::bearer("offline-fixture");
    endpoint.transport = crate::EndpointTransport::Http;
    endpoint.runtime.responses_profile = crate::ResponsesRuntimeProfile::Codex;
    let spec = Arc::make_mut(&mut model.spec);
    spec.endpoint = endpoint.id.clone();
    spec.api_name = "video-codex-fixture".into();
    let original = request();
    let saved = serde_json::to_vec(&original).unwrap();
    let client = crate::AiClient::new();
    client.complete(&model, original).await.unwrap();
    let resumed: Request = serde_json::from_slice(&saved).unwrap();
    client.complete(&model, resumed).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(first["input"], second["input"]);
    assert_eq!(first.to_string().matches("input_image").count(), 4);
    assert!(first.to_string().contains("video sent as 4 frames"));
    assert!(first.get("max_output_tokens").is_none());
}
