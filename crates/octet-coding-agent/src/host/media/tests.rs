use super::*;

use std::collections::HashMap;
use std::path::PathBuf;

use octet_agent::InputPart;
use octet_ai::{AudioFormat, Capabilities, EndpointId, Modality, ModalitySet, ModelSpec, Protocol};

use super::super::protocol::{MediaInput, RunRequest};

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

fn model_with_inputs(protocol: Protocol, input_modalities: ModalitySet) -> ModelSpec {
    ModelSpec {
        preset: Default::default(),
        id: octet_ai::ModelId("test-model".into()),
        endpoint: EndpointId("test-provider".into()),
        api_name: "test-model".into(),
        display_name: None,
        protocol,
        capabilities: Capabilities {
            responses_features: Default::default(),
            input_modalities,
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
            reasoning: None,
            responses_lite: false,
            agent_delegation: None,
            structured_output: false,
            deferred_tool_loading: false,
        },
        limits: octet_ai::ModelLimits {
            context_window: 32_768,
            max_output_tokens: 4_096,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    }
}

#[test]
fn image_input_is_typed_and_confined_to_the_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let inside_image = workspace.path().join("resume.jpg");
    let outside_image = outside.path().join("resume.jpg");
    std::fs::write(&inside_image, b"image").unwrap();
    std::fs::write(&outside_image, b"image").unwrap();

    let mut request = base_request(workspace.path().to_path_buf());
    request.image_paths.push(inside_image);
    let model = model_with_inputs(
        Protocol::OpenAiChat,
        ModalitySet::none().with(Modality::Image),
    );
    let input = load_user_input(&request, "inspect".into(), &model).unwrap();
    assert!(matches!(
        input.parts.as_slice(),
        [
            InputPart::Text(_),
            InputPart::Media(octet_ai::Media::Image(_))
        ]
    ));

    request.image_paths = vec![outside_image];
    let error = load_user_input(&request, "inspect".into(), &model).unwrap_err();
    assert!(error
        .to_string()
        .contains("outside the configured workspace"));
}

#[test]
fn ordered_image_and_audio_inputs_remain_typed() {
    let workspace = tempfile::tempdir().unwrap();
    let first_image = workspace.path().join("first.png");
    let audio = workspace.path().join("music.wav");
    let second_image = workspace.path().join("second.jpg");
    std::fs::write(&first_image, b"image-one").unwrap();
    std::fs::write(&audio, b"audio").unwrap();
    std::fs::write(&second_image, b"image-two").unwrap();

    let mut request = base_request(workspace.path().to_path_buf());
    request.media = vec![
        MediaInput::Image { path: first_image },
        MediaInput::Audio { path: audio },
        MediaInput::Image { path: second_image },
    ];
    let model = model_with_inputs(
        Protocol::OpenAiChat,
        ModalitySet::none()
            .with(Modality::Image)
            .with(Modality::Audio),
    );
    let input = load_user_input(&request, "compare".into(), &model).unwrap();

    assert!(matches!(
        input.parts.as_slice(),
        [
            InputPart::Text(_),
            InputPart::Media(octet_ai::Media::Image(_)),
            InputPart::Media(octet_ai::Media::Audio(audio)),
            InputPart::Media(octet_ai::Media::Image(_)),
        ] if audio.format == AudioFormat::Wav
    ));
}

#[test]
fn audio_requires_a_route_effectively_supporting_its_format() {
    let workspace = tempfile::tempdir().unwrap();
    let audio = workspace.path().join("music.wav");
    std::fs::write(&audio, b"audio").unwrap();
    let mut request = base_request(workspace.path().to_path_buf());
    request.media = vec![MediaInput::Audio { path: audio }];

    let image_only = model_with_inputs(
        Protocol::OpenAiChat,
        ModalitySet::none().with(Modality::Image),
    );
    let error = load_user_input(&request, "listen".into(), &image_only).unwrap_err();
    assert!(error
        .to_string()
        .contains("does not support native WAV audio input"));

    let advertised_on_unsupported_protocol = model_with_inputs(
        Protocol::OpenAiResponses,
        ModalitySet::none().with(Modality::Audio),
    );
    let error = load_user_input(
        &request,
        "listen".into(),
        &advertised_on_unsupported_protocol,
    )
    .unwrap_err();
    assert!(error.to_string().contains("through OpenAiResponses"));
}
