#![allow(missing_docs)]

//! Chronological, cursor-free fallback for dumb, redirected, unknown, and
//! explicitly plain terminals. Explicit `--print` remains the raw response-only
//! scripting surface; this mode preserves the interactive product's execution
//! structure without ANSI cursor control.

use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::time::Duration;

use octet_agent::{AgentEvent, OutputChannel};
use octet_ai::ToolCallId;
use tokio::time::MissedTickBehavior;

use sexy_tui_rs::{sanitize_text, ControlPictures, SanitizeOptions};

use crate::app::bootstrap::{build_app, resolve_launch_print, Bootstrap};
use crate::modes::{timestamp, HostRunOutcome, RUN_STREAM_LOST_MESSAGE};
use crate::presentation::{
    format_duration, is_hidden_tool_detail, provider_lifecycle_label,
    summarize_tool_with_workspace, tool_failure_reason, tool_result_is_failure, RunOutcome,
    RunPhase, RunTracker,
};
use crate::resources::{compose_instructions, expand_skill_command};
use crate::tui::theme::OctetTheme;

#[derive(Debug, PartialEq, Eq)]
enum PromptExit {
    Finished(HostRunOutcome),
}

#[derive(Default)]
struct ProviderAttemptOutput {
    pending: String,
}

impl ProviderAttemptOutput {
    fn observe(&mut self, event: &AgentEvent) -> Option<String> {
        match event {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text,
            } => self.pending.push_str(text),
            AgentEvent::ProviderRetry { .. } | AgentEvent::CandidateRejected { .. } => {
                self.pending.clear()
            }
            AgentEvent::TurnFinished { .. } => return Some(std::mem::take(&mut self.pending)),
            _ => {}
        }
        None
    }
}

#[derive(Default)]
struct InteractiveExitStatus {
    first_failure: Option<HostRunOutcome>,
}

impl InteractiveExitStatus {
    fn observe(&mut self, exit: PromptExit) -> bool {
        match exit {
            PromptExit::Finished(HostRunOutcome::Shutdown) => false,
            PromptExit::Finished(HostRunOutcome::Completed) => true,
            PromptExit::Finished(failure) => {
                self.first_failure.get_or_insert(failure);
                true
            }
        }
    }

    fn finish(self) -> anyhow::Result<()> {
        match self.first_failure {
            Some(failure) => crate::modes::print::classify_finish(failure),
            None => Ok(()),
        }
    }
}

fn finish_one_shot(exit: PromptExit) -> anyhow::Result<()> {
    match exit {
        PromptExit::Finished(finished) => crate::modes::print::classify_finish(finished),
    }
}

fn safe_text(raw: &str) -> String {
    sanitize_text(
        raw,
        SanitizeOptions {
            controls: ControlPictures::Ascii,
            preserve_newlines: true,
            preserve_tabs: true,
        },
    )
    .into_owned()
}

fn style_log(theme: &OctetTheme, line: &str) -> String {
    let line = safe_text(line);
    if line.starts_with("[working]") {
        theme.fg("model_accent", &line)
    } else if line.starts_with("[failed]") || line.contains(" failed") {
        theme.fg("error", &line)
    } else if line.starts_with("[needs input]") || line.starts_with("[completed with warnings]") {
        theme.fg("warning", &line)
    } else {
        line
    }
}

fn write_log(
    output: &mut impl Write,
    response_open: &mut bool,
    theme: &OctetTheme,
    line: &str,
) -> std::io::Result<()> {
    if *response_open {
        writeln!(output)?;
        *response_open = false;
    }
    writeln!(output, "{}", style_log(theme, line))
}

fn write_prompt(output: &mut impl Write, theme: &OctetTheme, prompt: &str) -> std::io::Result<()> {
    for (index, line) in prompt.lines().enumerate() {
        let marker = if index == 0 {
            theme.fg("model_accent", ">")
        } else {
            " ".into()
        };
        writeln!(output, "{marker} {}", safe_text(line))?;
    }
    if prompt.is_empty() {
        writeln!(output, "{}", theme.fg("model_accent", ">"))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptPresentation {
    Explicit,
    TerminalEcho,
}

fn present_prompt(
    output: &mut impl Write,
    theme: &OctetTheme,
    prompt: &str,
    presentation: PromptPresentation,
) -> std::io::Result<()> {
    match presentation {
        PromptPresentation::Explicit => write_prompt(output, theme, prompt),
        PromptPresentation::TerminalEcho => Ok(()),
    }
}

fn outcome_text(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Completed { elapsed, summary } => {
            let mut parts = vec![format!("[completed] {}", format_duration(*elapsed))];
            if summary.files_changed > 0 {
                parts.push(format!(
                    "{} file{} changed",
                    summary.files_changed,
                    if summary.files_changed == 1 { "" } else { "s" }
                ));
            }
            parts.join(" - ")
        }
        RunOutcome::CompletedWithWarnings {
            elapsed, warnings, ..
        } => format!(
            "[completed with warnings] {} warning{} - {}",
            warnings,
            if *warnings == 1 { "" } else { "s" },
            format_duration(*elapsed)
        ),
        RunOutcome::Failed { elapsed, reason } => {
            format!("[failed] {reason} - {}", format_duration(*elapsed))
        }
        RunOutcome::Interrupted { elapsed } => {
            format!("[interrupted] {}", format_duration(*elapsed))
        }
        RunOutcome::NeedsInput { prompt } => format!("[needs input] {prompt}"),
    }
}

async fn run_prompt(
    app: &mut crate::app::App,
    prompt: String,
    output: &mut impl Write,
    theme: &OctetTheme,
    tracker: &mut RunTracker,
    presentation: PromptPresentation,
) -> anyhow::Result<PromptExit> {
    let prompt = match crate::prompts::render_configured(app, &prompt)? {
        Some(rendered) => {
            if app.config.debug_prompt {
                writeln!(output, "{}", crate::prompts::debug_expansion(&rendered))?;
            }
            rendered.text
        }
        None => prompt,
    };
    let display_prompt = prompt.clone();
    let prompt = match expand_skill_command(
        app.skills.as_ref(),
        &prompt,
        &app.agent.registered_tool_names(),
    ) {
        Ok(Some(expanded)) => expanded,
        Ok(None) => prompt,
        Err(error) => {
            eprintln!("warning: failed to expand /skill: command: {error}");
            prompt
        }
    };
    present_prompt(output, theme, &display_prompt, presentation)?;
    let run_id = tracker
        .begin_for_model(&app.model.endpoint.id.0, &app.model.spec.id.0)
        .expect("fresh tracker cannot have an active run");

    if let Some(limit) = app.config.max_cost_microdollars {
        if app.agent.session().total_cost_microdollars() >= limit {
            let reason = format!(
                "Session cost limit of {} reached.",
                crate::commands::format_microdollars_cents(limit)
            );
            let outcome = tracker.fail(run_id, reason.clone()).expect("active run");
            writeln!(output, "{}", style_log(theme, &outcome_text(&outcome)))?;
            output.flush()?;
            return Ok(PromptExit::Finished(HostRunOutcome::Failed(reason)));
        }
    }

    // Capacity checks and compaction happen inside the cancellable Agent run.
    if let Some(limit) = app.config.max_cost_microdollars {
        if app.agent.session().total_cost_microdollars() >= limit {
            let reason = format!(
                "Session cost limit of {} reached.",
                crate::commands::format_microdollars_cents(limit)
            );
            let outcome = tracker.fail(run_id, reason.clone()).expect("active run");
            writeln!(output, "{}", style_log(theme, &outcome_text(&outcome)))?;
            output.flush()?;
            return Ok(PromptExit::Finished(HostRunOutcome::Failed(reason)));
        }
    }

    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let composition = app
        .executable_extensions
        .compose_prompt(&app.system, prompt.clone())
        .await?;
    let pending_context_count = composition.pending_context_count;
    for notification in composition.notifications {
        writeln!(
            output,
            "{}",
            style_log(theme, &format!("[extension] {notification}"))
        )?;
    }
    app.agent.set_system_prompt(composition.system);
    app.agent.set_prompt_display_text(Some(display_prompt));
    let mut run = match app.agent.prompt(composition.prompt).await {
        Ok(run) => run,
        Err(error) => {
            // Pending extension context remains uncommitted. A later TTY
            // prompt recomposes from `app.system` before touching the Agent.
            let reason = octet_agent::public_error_diagnostic(
                &error,
                &app.model.endpoint.id.0,
                &app.model.spec.id.0,
            );
            let outcome = tracker.fail(run_id, reason.clone()).expect("active run");
            writeln!(output, "{}", style_log(theme, &outcome_text(&outcome)))?;
            output.flush()?;
            return Ok(PromptExit::Finished(HostRunOutcome::Failed(reason)));
        }
    };
    let extension_turn = app.executable_extensions.begin_turn().await;
    app.executable_extensions
        .commit_prompt_context(pending_context_count);
    tracker.awaiting_provider(run_id);
    let control = run.control();
    let mut last_run_cost = 0u64;
    writeln!(
        output,
        "{}",
        style_log(
            theme,
            &format!("[working] Waiting for {}", app.model.endpoint.id.0)
        )
    )?;
    output.flush()?;

    let mut tools: HashMap<ToolCallId, (String, serde_json::Value)> = HashMap::new();
    let mut response_open = false;
    let mut last_phase = tracker.current().map(|run| run.phase().clone());
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    heartbeat.tick().await;
    let mut response_text = String::new();
    let mut attempt_output = ProviderAttemptOutput::default();
    let outcome = loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                control.abort();
                octet_agent::extension_process::terminate_bash_process_groups(
                    Duration::from_millis(400),
                )
                .await;
                break HostRunOutcome::shutdown();
            }
            event = run.next() => {
                let Some(event) = event else {
                    let reason = RUN_STREAM_LOST_MESSAGE;
                    let outcome = tracker
                        .fail(run_id, reason)
                        .expect("active run");
                    write_log(output, &mut response_open, theme, &outcome_text(&outcome))?;
                    break HostRunOutcome::stream_lost();
                };
                let accepted_output = attempt_output.observe(&event);
                let update = tracker.apply_event(run_id, &event);
                match &event {
                    AgentEvent::OutputDelta { channel: OutputChannel::Text, .. } => {}
                    AgentEvent::OutputDelta { channel: OutputChannel::Reasoning, .. } => {
                        if !matches!(last_phase, Some(RunPhase::Thinking)) {
                            write_log(output, &mut response_open, theme, "[working] Thinking")?;
                        }
                    }
                    AgentEvent::OutputMedia { .. } => {}
                    AgentEvent::ProviderLifecycle { lifecycle } => {
                        // Lifecycle telemetry is diagnostic-only. Keep it out
                        // of this mode's response/log stdout so a caller can
                        // still separate answer text from endpoint readiness.
                        crate::output::stderr!(
                            "[working] {}",
                            provider_lifecycle_label(&app.model.endpoint.id.0, lifecycle)
                        );
                    }
                    AgentEvent::ProviderRetry {
                        attempt,
                        max_attempts,
                        error,
                        ..
                    } => {
                        write_log(
                            output,
                            &mut response_open,
                            theme,
                            &format!(
                                "[retry] {error}; discarding partial response and retrying ({attempt}/{max_attempts})"
                            ),
                        )?;
                    }
                    AgentEvent::ProviderUsageUncertain => {
                        crate::output::stderr!("warning: provider usage and cost are uncertain for this session; all subsequent numeric usage/cost values are known subtotals, not complete totals (including after resume).");
                    }
                    AgentEvent::ProviderOperationRetry { operation, attempt, max_attempts, delay, error } => {
                        let operation = serde_json::to_value(operation).expect("provider operation serializes");
                        let limit = max_attempts.map(|limit| format!("/{limit}")).unwrap_or_default();
                        crate::output::stderr!(
                            "[provider operation retry: {}] attempt {attempt}{limit}; next attempt in at least {}s: {error}",
                            operation.as_str().expect("provider operation is a string"), delay.as_secs_f64()
                        );
                    }
                    AgentEvent::ProviderWaitingForNetwork { attempt, delay, error } => {
                        crate::output::stderr!(
                            "[waiting for network] attempt {attempt}; next attempt in at least {}s: {error}",
                            delay.as_secs_f64()
                        );
                    }
                    AgentEvent::CandidateRejected {
                        run_cost_microdollars,
                        ..
                    } => {
                        last_run_cost = *run_cost_microdollars;
                    }
                    AgentEvent::CompactionStarted { .. } => {
                        write_log(
                            output,
                            &mut response_open,
                            theme,
                            "[working] Compacting context",
                        )?;
                    }
                    AgentEvent::CompactionFinished { result, .. } => match result {
                        Ok(_) => write_log(
                            output,
                            &mut response_open,
                            theme,
                            "[working] Context compacted",
                        )?,
                        Err(error) => write_log(
                            output,
                            &mut response_open,
                            theme,
                            &format!("[failed] Compaction failed: {error}"),
                        )?,
                    },
                    AgentEvent::ToolStarted { id, name, args } => {
                        let display = summarize_tool_with_workspace(
                            name,
                            args,
                            Some(&app.config.workspace),
                        );
                        write_log(
                            output,
                            &mut response_open,
                            theme,
                            &format!("[working] {}", display.active),
                        )?;
                        tools.insert(id.clone(), (name.clone(), args.clone()));
                    }
                    // Policy details are available in telemetry and the
                    // headless host protocol; plain output keeps the existing
                    // concise tool lifecycle and model-facing failure text.
                    AgentEvent::ToolPolicyDecision { .. } | AgentEvent::ToolProgress { .. } => {}
                    AgentEvent::ToolFinished { id, result, .. } => {
                        let (name, args) = tools
                            .remove(id)
                            .unwrap_or_else(|| ("tool".into(), serde_json::Value::Null));
                        let display = summarize_tool_with_workspace(
                            &name,
                            &args,
                            Some(&app.config.workspace),
                        );
                        if tool_result_is_failure(&name, result) {
                            let reason = tool_failure_reason(&name, result)
                                .unwrap_or_else(|| "tool failed".into());
                            write_log(
                                output,
                                &mut response_open,
                                theme,
                                &format!("[{}] {} - {reason}", display.plain_tag, display.failure),
                            )?;
                            if let Err(error) = result {
                                for line in error
                                    .message
                                    .lines()
                                    .skip(1)
                                    .filter(|line| !is_hidden_tool_detail(line))
                                    .take(12)
                                {
                                    write_log(
                                        output,
                                        &mut response_open,
                                        theme,
                                        &format!("  {}", safe_text(line)),
                                    )?;
                                }
                            }
                        } else {
                            write_log(
                                output,
                                &mut response_open,
                                theme,
                                &format!("[{}] {}", display.plain_tag, display.success),
                            )?;
                        }
                    }
                    AgentEvent::TurnFinished {
                        message,
                        session_cost_microdollars,
                        run_cost_microdollars,
                        ..
                    } => {
                        if let Some(accepted_output) = accepted_output {
                            write!(output, "{}", safe_text(&accepted_output))?;
                            if !accepted_output.is_empty() {
                                response_open = !accepted_output.ends_with('\n');
                            }
                        }
                        response_text.clear();
                        response_text.push_str(&crate::extensions::assistant_text(message));
                        let turn_cost = run_cost_microdollars.saturating_sub(last_run_cost);
                        if app
                            .config
                            .cost_warning_microdollars
                            .is_some_and(|threshold| turn_cost >= threshold)
                        {
                            write_log(
                                output,
                                &mut response_open,
                                theme,
                                &format!(
                                    "[warning] turn cost {} reached the {} threshold",
                                    crate::commands::format_microdollars(turn_cost),
                                    crate::commands::format_microdollars_cents(
                                        app.config.cost_warning_microdollars.unwrap_or_default()
                                    )
                                ),
                            )?;
                        }
                        last_run_cost = *run_cost_microdollars;
                        if let (Some(limit), Some(total)) =
                            (app.config.max_cost_microdollars, *session_cost_microdollars)
                        {
                            if total >= limit {
                                write_log(
                                    output,
                                    &mut response_open,
                                    theme,
                                    &format!(
                                        "[failed] Session cost limit of {} reached.",
                                        crate::commands::format_microdollars_cents(limit)
                                    ),
                                )?;
                                control.abort();
                            }
                        }
                    }
                    AgentEvent::SteeringDelivered { .. }
                    | AgentEvent::FollowUpDelivered { .. }
                    | AgentEvent::DelegationUpdated { .. }
                    | AgentEvent::TurnStarted => {}
                    AgentEvent::RunFinished { reason, .. } => {
                        let outcome = HostRunOutcome::from_finish_reason(
                            reason,
                            &app.model.endpoint.id.0,
                            &app.model.spec.id.0,
                        );
                        if let Some(outcome) = update.outcome {
                            write_log(output, &mut response_open, theme, &outcome_text(&outcome))?;
                        }
                        break outcome;
                    }
                }
                last_phase = tracker.current().map(|current| current.phase().clone());
                output.flush()?;
            }
            _ = heartbeat.tick(), if tracker.is_active() => {
                if let Some(current) = tracker.current() {
                    let label = match current.phase() {
                        RunPhase::Preparing { summary } => summary.clone(),
                        RunPhase::AwaitingProvider { provider } => format!("waiting for {provider}"),
                        RunPhase::ProviderLifecycle {
                            provider,
                            state,
                            detail,
                        } => {
                            let lifecycle = octet_ai::ProviderLifecycle {
                                state: *state,
                                detail: detail.clone(),
                            };
                            crate::output::stderr!(
                                "[working] {} - {}",
                                provider_lifecycle_label(provider, &lifecycle),
                                format_duration(current.phase_elapsed_at(std::time::Instant::now()))
                            );
                            continue;
                        }
                        RunPhase::Thinking => "thinking".into(),
                        RunPhase::StreamingResponse => "writing response".into(),
                        RunPhase::PreparingToolCall => "preparing tool call".into(),
                        RunPhase::RunningTool { summary } => summary.clone(),
                        RunPhase::AwaitingApproval { prompt } => format!("approval required - {prompt}"),
                        RunPhase::Finished(_) => continue,
                    };
                    write_log(
                        output,
                        &mut response_open,
                        theme,
                        &format!("[working] {label} - {}", format_duration(current.phase_elapsed_at(std::time::Instant::now()))),
                    )?;
                    output.flush()?;
                }
            }
        }
    };
    drop(run);
    app.executable_extensions
        .settle_turn(extension_turn, &outcome)
        .await;
    app.agent.set_system_prompt(app.system.clone());
    if outcome.allows_after_response() {
        for notification in app
            .executable_extensions
            .after_response(&response_text)
            .await
        {
            write_log(
                output,
                &mut response_open,
                theme,
                &format!("[extension] {notification}"),
            )?;
        }
    }
    let presentation = app.executable_extensions.presentation_text();
    if !presentation.is_empty() {
        write_log(
            output,
            &mut response_open,
            theme,
            &format!("[extension state]\n{presentation}"),
        )?;
    }
    if outcome.shutdown_requested() {
        let _ = tokio::time::timeout(
            Duration::from_millis(1400),
            app.executable_extensions.shutdown(),
        )
        .await;
        octet_agent::extension_process::force_kill_registered_process_groups();
        output.flush()?;
        return Ok(PromptExit::Finished(outcome));
    }
    writeln!(output)?;
    output.flush()?;
    Ok(PromptExit::Finished(outcome))
}

/// Run the chronological fallback. A positional prompt is one-shot. Without
/// one, piped stdin becomes one prompt; a TTY reads one line at each `> `.
pub async fn run_plain(boot: Bootstrap, initial_prompt: Option<String>) -> anyhow::Result<()> {
    let launch = resolve_launch_print(&boot, &timestamp())?;
    let system = compose_instructions(&boot.config)?;
    let mut theme = crate::tui::theme::load_theme(&boot.config);
    let mut app = build_app(boot, launch, system)?;
    crate::tui::theme::apply_model_lab(&mut theme, crate::tui::theme::model_lab(&app.model));
    let mut tracker = RunTracker::default();
    let mut output = std::io::stdout().lock();
    writeln!(
        output,
        "OCTET - {}/{}{}",
        safe_text(&app.model.endpoint.id.0),
        theme.fg("model_accent", &safe_text(&app.model.spec.id.0)),
        match crate::app::reasoning_label(&app.reasoning).as_str() {
            "off" => String::new(),
            level => format!(" - {level}"),
        }
    )?;
    writeln!(
        output,
        "Workspace - {}\n",
        safe_text(&app.config.workspace.display().to_string())
    )?;

    if let Some(prompt) = initial_prompt.filter(|prompt| !prompt.trim().is_empty()) {
        let exit = run_prompt(
            &mut app,
            prompt,
            &mut output,
            &theme,
            &mut tracker,
            PromptPresentation::Explicit,
        )
        .await?;
        return finish_one_shot(exit);
    }

    if app.config.prompt_template.is_some() && std::io::stdin().is_terminal() {
        let exit = run_prompt(
            &mut app,
            String::new(),
            &mut output,
            &theme,
            &mut tracker,
            PromptPresentation::Explicit,
        )
        .await?;
        return finish_one_shot(exit);
    }

    if !std::io::stdin().is_terminal() {
        let mut prompt = String::new();
        std::io::stdin().read_to_string(&mut prompt)?;
        if prompt.trim().is_empty() {
            anyhow::bail!("plain mode needs a positional prompt or text on stdin");
        }
        let exit = run_prompt(
            &mut app,
            prompt,
            &mut output,
            &theme,
            &mut tracker,
            PromptPresentation::Explicit,
        )
        .await?;
        return finish_one_shot(exit);
    }

    // Stdin is a TTY here, but its echo does not reach redirected stdout.
    let presentation = if output.is_terminal() {
        PromptPresentation::TerminalEcho
    } else {
        PromptPresentation::Explicit
    };

    // A blocking terminal read must not prevent coordinated signal cleanup.
    // Keep the OS read on a dedicated thread and let this async owner select
    // between the next line and the level-triggered shutdown notification.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<std::io::Result<Option<String>>>(1);
    std::thread::Builder::new()
        .name("octet-plain-input".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                let mut line = String::new();
                let item = match input.read_line(&mut line) {
                    Ok(0) => Ok(None),
                    Ok(_) => Ok(Some(line)),
                    Err(error) => Err(error),
                };
                let finished = !matches!(&item, Ok(Some(_)));
                if input_tx.blocking_send(item).is_err() || finished {
                    break;
                }
            }
        })?;
    let mut exit_status = InteractiveExitStatus::default();
    loop {
        write!(output, "{} ", theme.fg("model_accent", ">"))?;
        output.flush()?;
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                octet_agent::extension_process::terminate_bash_process_groups(
                    Duration::from_millis(400),
                )
                .await;
                let _ = tokio::time::timeout(
                    Duration::from_millis(1400),
                    app.executable_extensions.shutdown(),
                )
                .await;
                octet_agent::extension_process::force_kill_registered_process_groups();
                output.flush()?;
                return Ok(());
            }
            next = input_rx.recv() => next,
        };
        let Some(next) = next else {
            break;
        };
        let Some(prompt) = next? else {
            break;
        };
        let prompt = prompt.trim_end_matches(['\r', '\n']).to_owned();
        if prompt.is_empty() {
            continue;
        }
        let exit = run_prompt(
            &mut app,
            prompt,
            &mut output,
            &theme,
            &mut tracker,
            presentation,
        )
        .await?;
        if !exit_status.observe(exit) {
            return exit_status.finish();
        }
    }
    exit_status.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::RunSummary;
    use octet_ai::{AssistantMessage, ModelId, Protocol, Usage};

    fn completed_turn() -> AgentEvent {
        AgentEvent::TurnFinished {
            message: AssistantMessage {
                content: Vec::new(),
                model: ModelId("test-model".into()),
                protocol: Protocol::OpenAiChat,
            },
            stop_reason: octet_ai::StopReason::EndTurn,
            turn_usage: Usage::default(),
            usage: Usage::default(),
            session_cost_microdollars: None,
            run_cost_microdollars: 0,
        }
    }

    #[test]
    fn plain_outcomes_are_ascii_and_explicit() {
        let completed = outcome_text(&RunOutcome::Completed {
            elapsed: Duration::from_millis(1200),
            summary: RunSummary {
                files_changed: 2,
                tool_calls: 3,
                warnings: 0,
            },
        });
        assert_eq!(completed, "[completed] 1.2s - 2 files changed");
        assert!(completed.is_ascii());

        let failed = outcome_text(&RunOutcome::Failed {
            elapsed: Duration::from_secs(2),
            reason: "command exited 1".into(),
        });
        assert_eq!(failed, "[failed] command exited 1 - 2.0s");
    }

    #[test]
    fn plain_text_neutralizes_terminal_controls() {
        assert_eq!(safe_text("a\x1b[31m\x07"), "a^[[31m<BEL>");
        assert_eq!(safe_text("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(safe_text("a\u{202e}b"), "a<U+202E>b");
    }

    #[test]
    fn interactive_terminal_echo_owns_prompt_presentation() {
        let theme = crate::tui::theme::test_theme();
        let mut interactive_output = b"> first prompt\n> second prompt\n".to_vec();
        let echoed = interactive_output.clone();

        present_prompt(
            &mut interactive_output,
            &theme,
            "first prompt",
            PromptPresentation::TerminalEcho,
        )
        .unwrap();
        present_prompt(
            &mut interactive_output,
            &theme,
            "second prompt",
            PromptPresentation::TerminalEcho,
        )
        .unwrap();

        assert_eq!(interactive_output, echoed);
        let interactive_output = String::from_utf8(interactive_output).unwrap();
        assert_eq!(interactive_output.matches("first prompt").count(), 1);
        assert_eq!(interactive_output.matches("second prompt").count(), 1);

        let mut explicit_output = Vec::new();
        present_prompt(
            &mut explicit_output,
            &theme,
            "piped prompt",
            PromptPresentation::Explicit,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(explicit_output)
                .unwrap()
                .matches("piped prompt")
                .count(),
            1
        );
    }

    #[test]
    fn one_shot_plain_exit_status_matches_print_mode() {
        assert!(finish_one_shot(PromptExit::Finished(HostRunOutcome::Completed)).is_ok());
        assert!(finish_one_shot(PromptExit::Finished(HostRunOutcome::MaxTurns)).is_err());
        assert!(finish_one_shot(PromptExit::Finished(HostRunOutcome::Aborted)).is_err());
        assert!(
            finish_one_shot(PromptExit::Finished(HostRunOutcome::Failed("nope".into()))).is_err()
        );
        assert!(finish_one_shot(PromptExit::Finished(HostRunOutcome::StreamLost)).is_err());
        assert!(finish_one_shot(PromptExit::Finished(HostRunOutcome::Shutdown)).is_ok());
    }

    #[test]
    fn interactive_plain_retains_a_requested_run_failure_until_exit() {
        let mut status = InteractiveExitStatus::default();
        assert!(status.observe(PromptExit::Finished(HostRunOutcome::Failed(
            "first request failed".into(),
        ))));
        assert!(status.observe(PromptExit::Finished(HostRunOutcome::Completed)));

        let error = status.finish().unwrap_err().to_string();
        assert!(error.contains("first request failed"), "{error}");
    }

    #[test]
    fn operation_retries_do_not_discard_plain_candidate() {
        let mut output = ProviderAttemptOutput {
            pending: "candidate awaiting gate".into(),
        };
        assert!(output
            .observe(&AgentEvent::ProviderUsageUncertain)
            .is_none());
        assert!(
            HostRunOutcome::from_event(&AgentEvent::ProviderUsageUncertain, "test", "test")
                .is_none()
        );
        assert_eq!(output.pending, "candidate awaiting gate");
        for operation in [
            octet_agent::ProviderOperation::LocalCompaction,
            octet_agent::ProviderOperation::NativeCompaction,
            octet_agent::ProviderOperation::TerminalGate,
        ] {
            for max_attempts in [None, Some(5)] {
                let event = AgentEvent::ProviderOperationRetry {
                    operation,
                    attempt: 8,
                    max_attempts,
                    delay: Duration::ZERO,
                    error: "offline".into(),
                };
                assert!(output.observe(&event).is_none());
                assert!(HostRunOutcome::from_event(&event, "test", "test").is_none());
                assert_eq!(output.pending, "candidate awaiting gate");
            }
        }
        assert_eq!(
            output.observe(&completed_turn()).unwrap(),
            "candidate awaiting gate"
        );
    }

    #[test]
    fn repeated_network_waits_preserve_committed_plain_output() {
        let mut output = ProviderAttemptOutput::default();
        let mut published = String::new();
        output.observe(&AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "COMMITTED".into(),
        });
        published.push_str(&output.observe(&completed_turn()).unwrap());
        output.observe(&AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "STALE".into(),
        });
        output.observe(&AgentEvent::ProviderRetry {
            attempt: 1,
            max_attempts: 5,
            delay: Duration::ZERO,
            error: "disconnect".into(),
        });
        for attempt in 1..=32 {
            let wait = AgentEvent::ProviderWaitingForNetwork {
                attempt,
                delay: Duration::from_secs(30),
                error: "offline".into(),
            };
            assert!(output.observe(&wait).is_none());
            assert!(HostRunOutcome::from_event(&wait, "test", "test").is_none());
            assert!(output.pending.is_empty());
            assert_eq!(published, "COMMITTED");
        }
        output.observe(&AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "RECOVERED".into(),
        });
        published.push_str(&output.observe(&completed_turn()).unwrap());
        assert_eq!(published, "COMMITTEDRECOVERED");
    }

    #[test]
    fn provider_retry_discards_stale_plain_output_before_turn_commit() {
        let events = [
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "STALE".into(),
            },
            AgentEvent::ProviderRetry {
                attempt: 1,
                max_attempts: 5,
                delay: Duration::ZERO,
                error: "forced disconnect".into(),
            },
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "FRESH".into(),
            },
            completed_turn(),
        ];
        let mut output = ProviderAttemptOutput::default();
        let published = events
            .iter()
            .filter_map(|event| output.observe(event))
            .collect::<String>();

        assert_eq!(published, "FRESH");
        assert!(!published.contains("STALE"));
    }
}
