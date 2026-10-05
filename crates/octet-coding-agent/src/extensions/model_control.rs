//! Authoritative idle Pi model controls, without an App/extension rebuild.
use super::*;
use crate::app::App;
use octet_agent::extension_process::ExtensionModelControl;

pub(crate) fn apply_idle_model_control(
    app: &mut App,
    operation: &ExtensionModelControl,
) -> Result<Value, String> {
    // Dynamic declarations are acknowledged by the same native registry before
    // this request. Reconcile them before resolving the selected route.
    app.synchronize_extension_provider_catalog();
    let (model, level) = match operation {
        ExtensionModelControl::Model { provider, id } => {
            let model = app.catalog.models().find_map(|spec| {
                let model = app.catalog.resolve(&spec.id).ok()?;
                (model.spec.api_name == *id
                    && pi_provider_id(&model) == *provider
                    && model.endpoint.auth.is_configured())
                .then_some(model)
            });
            let Some(model) = model else {
                return Ok(serde_json::json!({"selected":false}));
            };
            let level = crate::app::level_from_reasoning(&app.reasoning, &app.model)
                .map_err(|error| error.to_string())?;
            (model, level)
        }
        ExtensionModelControl::Thinking { level } => (
            app.model.clone(),
            crate::config::ThinkingLevel::parse(level).map_err(|error| error.to_string())?,
        ),
    };
    let reasoning = crate::app::thinking_to_reasoning_with_subagents(
        level,
        &model,
        app.executable_extensions.has_agent_session_service(),
    )
    .map_err(|error| error.to_string())?;
    let portable = pi_thinking_level(&model, &reasoning).ok_or_else(|| {
        "unsupported_feature: selected reasoning has no Pi thinking level".to_owned()
    })?;
    let view = app
        .executable_extensions
        .provider_runtime
        .pi_model_view(&model)
        .ok_or_else(|| "unsupported_feature: selected model has no bounded Pi view".to_owned())?;
    app.agent
        .select_model_at_idle(
            model.clone(),
            reasoning.clone(),
            crate::app::reasoning_label(&reasoning),
        )
        .map_err(|error| error.to_string())?;
    app.model = model;
    app.reasoning = reasoning;
    // Config is the invocation's requested default, not a project configuration
    // write. The durable Config entry and live Agent own this selection.
    Ok(serde_json::json!({"selected":true,"model_view":view,"reasoning":portable}))
}
