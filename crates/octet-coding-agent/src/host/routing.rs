//! Static host commands and catalog routing.
//!
//! This module contains the non-running command responses. The parent facade
//! remains responsible for decoded-request dispatch and lifecycle boundaries.

use std::collections::HashMap;

use octet_ai::Modality;

use crate::app::bootstrap;

use super::policy::host_config;
use super::protocol::{RunRequest, MAX_FRAME_BYTES, PROTOCOL_VERSION};
use super::transport::Emitter;

pub(crate) async fn emit_hello(emitter: &mut Emitter<'_>) -> anyhow::Result<()> {
    emitter
        .emit(
            "hello",
            serde_json::json!({
                "sdk_version": env!("CARGO_PKG_VERSION"),
                "protocol_version": PROTOCOL_VERSION,
                "max_frame_bytes": MAX_FRAME_BYTES,
                "max_concurrent_runs": 1,
                "commands": ["hello", "models", "run", "shutdown"],
                "features": {
                    "streaming": true,
                    "persistent_sessions": true,
                    "seed_history": true,
                    "typed_media_input": true,
                    "typed_image_input": true,
                    "typed_audio_input": true,
                    "prompt_display_text": true,
                    "inline_models": true,
                    "tools": true,
                    "skills": true,
                    "extensions": true,
                    "process_group_abort": true,
                    "in_band_abort": false,
                },
            }),
        )
        .await
}

pub(crate) async fn emit_models(emitter: &mut Emitter<'_>, offline: bool) -> anyhow::Result<()> {
    let catalog = if offline {
        // The public no-network path is built through a minimal host config.
        let workspace = std::env::current_dir()?.canonicalize()?;
        let config = host_config(&RunRequest {
            run_id: "models".into(),
            session_id: None,
            workspace: workspace.clone(),
            working_dir: Some(workspace.clone()),
            session_dir: Some(workspace.join(".octet/sessions")),
            resume_session: None,
            model: "unused".into(),
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
            prompt: String::new(),
            prompt_display_text: None,
            system_prompt: None,
            reasoning: None,
            tools: Some(Vec::new()),
            allow_file_mutation: false,
            allow_external_paths: false,
            context_files: false,
            offline: true,
            max_turns: Some(1),
            max_cost_microdollars: None,
            history: Vec::new(),
            media: Vec::new(),
            image_paths: Vec::new(),
            prompt_paths: Vec::new(),
            skill_paths: Vec::new(),
            extension_paths: Vec::new(),
            enabled_extensions: Vec::new(),
            trusted_extensions: Vec::new(),
        })?;
        bootstrap::bootstrap(config)?.catalog
    } else {
        bootstrap::model_catalog()?
    };
    let mut models = catalog
        .models()
        .map(|model| {
            let effective = model.effective_input_modalities();
            let mut input_modalities = vec!["text"];
            if effective.contains(Modality::Image) {
                input_modalities.push("image");
            }
            if effective.contains(Modality::Audio) {
                input_modalities.push("audio");
            }
            serde_json::json!({
                "id": model.id.0,
                "provider": model.endpoint.0,
                "api_name": model.api_name,
                "display_name": model.display_name,
                "protocol": format!("{:?}", model.protocol),
                "context_window": model.limits.context_window,
                "max_output_tokens": model.limits.max_output_tokens,
                "tools": model.capabilities.tools,
                "vision": effective.contains(Modality::Image),
                "audio": effective.contains(Modality::Audio),
                "input_modalities": input_modalities,
                "reasoning": model.capabilities.reasoning.is_some(),
            })
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    emitter
        .emit("models", serde_json::json!({"models": models}))
        .await
}
