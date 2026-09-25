//! Host run orchestration and lifecycle settlement.
//!
//! This module coordinates policy, bootstrap, session setup, prompt execution,
//! cancellation, extension lifecycle, and terminal responses. Agent-event shape
//! translation stays in `events` and path/media authority stays in its modules.

use crate::app::bootstrap::{self, LaunchSelection, SessionSelection};
use crate::modes::HostRunOutcome;

use super::events;
use super::media::load_user_input;
use super::policy::{host_config, register_inline_model, validate_run_request};
use super::protocol::{RunRequest, MAX_EVENT_TEXT_BYTES};
use super::sessions;
use super::transport::Emitter;

pub(crate) enum RunRequestOutcome {
    Completed,
    Signaled,
}

pub(crate) async fn run_request(
    emitter: &mut Emitter<'_>,
    request: RunRequest,
) -> anyhow::Result<RunRequestOutcome> {
    validate_run_request(&request)?;
    let config = host_config(&request)?;
    let system = match request.system_prompt.as_deref() {
        Some(system) => system.to_owned(),
        None => crate::resources::compose_instructions(&config)?,
    };
    let mut boot = bootstrap::bootstrap(config)?;
    let model_id = register_inline_model(&mut boot.catalog, &request)?;
    boot.config.model = Some(model_id.clone());
    let (selection, prepared_session) =
        sessions::session_selection(&boot.config.session_dir, &request)?;
    if let Some(session) = prepared_session {
        boot.set_prepared_session(session);
    }
    let session_path = match &selection {
        SessionSelection::CreateNew(path) | SessionSelection::OpenExisting(path) => path.clone(),
    };
    let new_session = matches!(selection, SessionSelection::CreateNew(_));
    let launch = LaunchSelection {
        model: model_id,
        session: selection,
        reasoning: request
            .reasoning
            .as_deref()
            .map(crate::config::parse_reasoning)
            .transpose()?
            .unwrap_or(octet_ai::ReasoningConfig::Off),
        reasoning_mode: octet_ai::ReasoningMode::Standard,
    };
    let mut app = bootstrap::build_app(boot, launch, system)?;
    if new_session {
        sessions::seed_history(&mut app, &request.history)?;
    }

    emitter
        .emit(
            "accepted",
            serde_json::json!({
                "model": request.model,
                "resolved_model": app.model.spec.id.0,
                "session_file": session_path,
                "registered_tools": app.agent.registered_tool_names(),
                "effective_tool_policy": app.config.sandbox.effective_tool_policy(
                    &app.config.workspace,
                    app.config.effect_policy,
                ),
                "extensions": app.executable_extensions.summaries(),
            }),
        )
        .await?;

    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let composition = app
        .executable_extensions
        .compose_prompt(&app.system, request.prompt.clone())
        .await?;
    for notification in &composition.notifications {
        emitter
            .emit(
                "extension_notification",
                serde_json::json!({"message": events::clip_text(notification, 16 * 1024)}),
            )
            .await?;
    }
    let pending_context_count = composition.pending_context_count;
    app.agent.set_system_prompt(composition.system);
    app.agent.set_prompt_display_text(Some(
        request
            .prompt_display_text
            .clone()
            .unwrap_or_else(|| request.prompt.clone()),
    ));
    let input = load_user_input(&request, composition.prompt, &app.model.spec)?;
    let mut run = match app.agent.prompt(input).await {
        Ok(run) => run,
        Err(error) => anyhow::bail!(
            "{}",
            octet_agent::public_error_diagnostic(
                &error,
                &app.model.endpoint.id.0,
                &app.model.spec.id.0,
            )
        ),
    };
    let extension_turn = app.executable_extensions.begin_turn().await;
    let control = run.control();
    app.executable_extensions
        .commit_prompt_context(pending_context_count);

    emitter
        .emit("started", serde_json::json!({"model": request.model}))
        .await?;
    let mut event_state = events::EventState::default();

    let outcome = loop {
        let event = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                control.abort();
                octet_agent::extension_process::terminate_bash_process_groups(
                    std::time::Duration::from_millis(400),
                )
                .await;
                break HostRunOutcome::shutdown();
            }
            event = run.next() => event,
        };
        let Some(event) = event else {
            break HostRunOutcome::stream_lost();
        };
        if let Some(outcome) = events::translate(
            event,
            &mut event_state,
            emitter,
            &app.model.endpoint.id.0,
            &app.model.spec.id.0,
        )
        .await?
        {
            break outcome;
        }
    };
    drop(run);
    app.executable_extensions
        .settle_turn(extension_turn, &outcome)
        .await;
    app.agent.set_system_prompt(app.system.clone());
    let final_output = event_state.final_output;
    let (status, terminal_error) = match &outcome {
        HostRunOutcome::Completed => ("completed", String::new()),
        HostRunOutcome::Aborted => ("blocked", "run aborted".to_owned()),
        HostRunOutcome::MaxTurns => (
            "blocked",
            "run reached the configured turn limit".to_owned(),
        ),
        HostRunOutcome::Failed(error) => ("error", error.clone()),
        HostRunOutcome::StreamLost | HostRunOutcome::Shutdown => (
            "error",
            outcome.failure_message().unwrap_or_default().to_owned(),
        ),
    };
    let terminal_head = event_state
        .terminal_head
        .or_else(|| app.agent.session().head().map(|head| head.0.clone()))
        .unwrap_or_default();
    if outcome.shutdown_requested() {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(1400),
            app.executable_extensions.shutdown(),
        )
        .await;
        octet_agent::extension_process::force_kill_registered_process_groups();
        // Process shutdown truncates the in-flight protocol request. Do not
        // emit a non-terminal `settled` event that cannot be followed by the
        // contractually required `final_result` or `protocol_error`.
        return Ok(RunRequestOutcome::Signaled);
    }
    emitter
        .emit(
            "settled",
            serde_json::json!({
                "status": status,
                "head": terminal_head,
                "error": events::clip_text(&terminal_error, 64 * 1024),
            }),
        )
        .await?;
    if outcome.allows_after_response() {
        for notification in app
            .executable_extensions
            .after_response(&final_output)
            .await
        {
            emitter
                .emit(
                    "extension_notification",
                    serde_json::json!({"message": events::clip_text(&notification, 16 * 1024)}),
                )
                .await?;
        }
    }
    app.executable_extensions.shutdown().await;
    emitter
        .emit(
            "final_result",
            serde_json::json!({
                "status": status,
                "output": events::clip_text(&final_output, MAX_EVENT_TEXT_BYTES),
                "error": events::clip_text(&terminal_error, MAX_EVENT_TEXT_BYTES),
                "filesChanged": event_state.files_changed,
                "toolCalls": event_state.tool_calls,
                "steps": event_state.steps,
                "sessionFile": session_path,
            }),
        )
        .await?;
    Ok(RunRequestOutcome::Completed)
}
