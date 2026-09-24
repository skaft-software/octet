#![allow(missing_docs)]

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{Stream, StreamExt};
use octet_agent::extension_api_v03::MAX_JSON_RPC_ID_BYTES;
#[cfg(unix)]
use octet_agent::extension_process::{wait_for_bash_process, BashProcessLaunch};
use octet_agent::extension_process::{
    ExtensionInputRequest, ExtensionSessionLifecycleError, ExtensionSessionLifecycleOperation,
    ExtensionSessionLifecycleRequest, MAX_EXTENSION_TERMINAL_INPUT_BYTES,
};
use octet_agent::{
    analyze_session_cache_stats, AgentCompactionMode, AgentError, AgentEvent, EffectBroker,
    EffectIntent, EntryId, GoalDecision, GoalStatus, GoalTurnSource, OutputChannel, Run,
    RunControl, Session, ToolEffect, ToolProgress, ToolProgressSink,
};
use octet_ai::{Model, ModelId, ReasoningConfig, ReasoningMode, ToolCallId};
use tokio::time::{Instant, Interval, MissedTickBehavior};

use crate::app::bootstrap::{
    build_app, effective_compaction_threshold_fraction, estimate_text_tokens, open_launch_session,
    rebuild_app, resolve_launch_interactive, terminal_goal_session_id, Bootstrap, SessionSelection,
};
use crate::app::{
    apply_reconfig, level_from_reasoning, reasoning_label, supported_levels_with_subagents,
    requested_thinking_to_reasoning, App, Reconfig,
};
use crate::commands::{self, Command};
use crate::compaction::{
    attempt_compaction, context_window, estimate_next_request_tokens, CompactionOutcome,
};
use crate::config::{CompactionMode, Config, ResumeSelector, SandboxPolicy, ThinkingLevel};
use crate::modes::{HostRunOutcome, RUN_STREAM_LOST_MESSAGE};
use crate::presentation::RunId;
use crate::prompts::{render_and_record, RenderedPrompt};
use crate::provider_setup::{
    CompletedSetup, ProviderSetupError, ProviderSetupService, ProviderSetupState,
    SetupAuthentication, SetupAuthority, SetupDraft,
};
use crate::resources::{compose_instructions, expand_skill_command};
use crate::tui::composer::ComposedInput;
use crate::tui::keymap::{self, InputAction};
use crate::tui::pickers::{
    self, confirmation_picker, extension_confirmation_picker, extension_input_picker,
    extension_picker, message_picker, optional_model_picker, pick_list_with_preview,
    provider_setup_picker, read_only_document, read_only_document_live_styled, session_picker,
    subagent_picker, thinking_picker, SubagentPickerSnapshot,
};
use crate::tui::terminal::TerminalInput as EventStream;
use crate::tui::theme::OctetTheme;
use crate::tui::theme::{
    background_from_terminal_rgb, load_theme, load_theme_for_background, TerminalBackground,
    TerminalThemeChoice,
};
use crate::tui::view::{
    InteractiveShell, OrdinarySurfaceMetadata, OverlayInputResult, Panel, PanelAction, PanelResult,
    SubagentPanel,
};

mod onboarding;

/// Ordered controls sent to the frozen Agent during an active run.
enum ControlIntent {
    /// Retractable live steering whose payload and recall receipt already live
    /// in the shell queue. Sending through `steer_retractable` keeps the
    /// receipt authoritative, so an Option/Alt+Up recall before delivery is a
    /// successful no-op instead of a duplicate append. Nonretractable `/answer`
    /// steering keeps using [`ControlIntent::FinishNow`].
    SteerPrepared(octet_agent::PreparedSteering),
    FinishNow(octet_agent::UserInput),
}

type ControlFuture = Pin<Box<dyn Future<Output = Result<(), AgentError>>>>;

/// The narrow broadcast seam that brackets one frontend-owned host dialog with
/// exactly one `dialog/started`/`dialog/settled` pair, whatever its outcome.
trait DialogLifecycleSink {
    fn open_dialog(&self, dialog: &str);
    fn close_dialog(&self, dialog: &str);
}

impl DialogLifecycleSink for crate::extensions::ExtensionLifecycleSnapshot {
    fn open_dialog(&self, dialog: &str) {
        crate::extensions::ExtensionLifecycleSnapshot::dialog_started(self, dialog);
    }

    fn close_dialog(&self, dialog: &str) {
        crate::extensions::ExtensionLifecycleSnapshot::dialog_settled(self, dialog);
    }
}

/// Present one host-owned dialog under a single dialog boundary.
///
/// The settled boundary is published on every outcome, including refusal,
/// cancellation, picker errors, and shutdown, and it is published exactly once
/// because the boundary is not part of the picker's own control flow.
async fn present_host_dialog<S, F, T>(dialogs: &S, dialog: &'static str, present: F) -> T
where
    S: DialogLifecycleSink + ?Sized,
    F: Future<Output = T>,
{
    dialogs.open_dialog(dialog);
    let outcome = present.await;
    dialogs.close_dialog(dialog);
    outcome
}

struct InteractiveExtensionConfirmations<'a> {
    shell: &'a mut InteractiveShell,
    input: &'a mut EventStream,
    /// Process handles captured before the extension runtime took its mutable
    /// borrow, so a presented dialog can still announce its boundary.
    dialogs: &'a crate::extensions::ExtensionLifecycleSnapshot,
}

impl crate::extensions::ExtensionConfirmationHandler for InteractiveExtensionConfirmations<'_> {
    fn wait_for_cancel<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
        Box::pin(async move {
            loop {
                let event = tokio::select! {
                    biased;
                    _ = crate::tui::terminal::wait_for_shutdown_signal() => return Ok(()),
                    event = self.input.next() => event,
                };
                match event {
                    Some(Ok(Event::Key(key))) if keymap::is_close_key(&key) => {
                        self.shell.request_close();
                        return Ok(());
                    }
                    Some(Ok(Event::Key(key)))
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                            && key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        return Ok(());
                    }
                    Some(Ok(Event::Resize(columns, rows))) => {
                        self.shell.set_size(columns, rows);
                        self.shell.render();
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.into()),
                    None => return Ok(()),
                }
            }
        })
    }

    fn progress(&mut self, extension: &str, progress: &ToolProgress) {
        // Raw command output and status are intentionally not transcript
        // content. The typed decoration is already bounded and terminal-safe,
        // so it is the one semantic progress surface suitable for Pi's live
        // status detail.
        let ToolProgress::Decoration(decoration) = progress else {
            return;
        };
        let detail = decoration
            .detail()
            .map(|detail| format!(" · {detail}"))
            .unwrap_or_default();
        self.shell
            .set_status_detail(format!("{extension}: {}{detail}", decoration.label()));
        self.shell.render();
    }

    fn finish_progress(&mut self, _extension: &str) {
        self.shell.set_status_detail(String::new());
        self.shell.render();
    }

    fn confirm<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a octet_agent::extension_process::ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        let dialogs = self.dialogs;
        Box::pin(present_host_dialog(dialogs, "confirm", async move {
            tokio::select! {
                biased;
                _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                    anyhow::bail!("shutdown requested while awaiting extension confirmation")
                }
                result = extension_confirmation_picker(
                    self.shell,
                    self.input,
                    extension,
                    request,
                ) => result,
            }
        }))
    }

    fn input<'a>(
        &'a mut self,
        _extension: &'a str,
        request: &'a octet_agent::extension_process::ExtensionInputRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + 'a>> {
        let dialogs = self.dialogs;
        Box::pin(present_host_dialog(dialogs, "input", async move {
            tokio::select! {
                biased;
                _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                    anyhow::bail!("shutdown requested while awaiting extension input")
                }
                result = extension_input_picker(self.shell, self.input, request) => result,
            }
        }))
    }
}

/// Reconfiguration work requested while the Agent is active. It is applied
/// only after `Run` is dropped at the next idle boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingIdleAction {
    Login(Option<String>),
    Logout(Option<String>),
    ChangeModel(ModelId),
    Fast(bool),
    ChangeThinking(ReasoningConfig),
    ChangeThinkingLevel(ThinkingLevel),
    CycleThinking,
    PickModel,
    PickThinking,
    NewSession,
    ResumeSession(Option<String>),
    Fork,
    Clone,
    Compact,
    CompactWithInstructions(String),
    AutoCompact(Option<commands::AutoCompactSetting>),
    ShowContext,
    ReloadResources,
    Skills(commands::SkillsSubcommand),
    // `/goal` deliberately has no queued form: it is applied the instant it is
    // submitted, mid-run or idle. Deferring it to the next idle boundary was
    // the reported defect, so the queue cannot even represent it.
    /// Extension management that owns the application (menu, reload, actions).
    Extensions(commands::ExtensionsSubcommand),
    /// `/settings` mutations that must own the application (theme, images,
    /// default model/reasoning). Reports are rendered immediately instead.
    Settings(commands::SettingsCommand),
    /// `/scoped-models` mutations that must own the application catalog and the
    /// shell's cycling list. Reports are rendered immediately instead.
    ScopedModels(commands::ScopedModelsCommand),
    /// Durable record of a shell escape that ran during an active run.
    RecordShellEscape(commands::ShellEscapeRecord),
}

/// Push an idle action while preserving ordering barriers. Adjacent model or
/// thinking changes collapse to the latest request; sessions and compaction do
/// not collapse or disappear.
pub fn push_pending_action(queue: &mut VecDeque<PendingIdleAction>, action: PendingIdleAction) {
    let same_kind = matches!(
        (&queue.back(), &action),
        (
            Some(PendingIdleAction::ChangeModel(_)),
            PendingIdleAction::ChangeModel(_)
        ) | (Some(PendingIdleAction::Fast(_)), PendingIdleAction::Fast(_))
            | (
                Some(PendingIdleAction::ChangeThinking(_)),
                PendingIdleAction::ChangeThinking(_)
            )
            | (
                Some(PendingIdleAction::ChangeThinking(_)),
                PendingIdleAction::ChangeThinkingLevel(_)
            )
            | (
                Some(PendingIdleAction::ChangeThinkingLevel(_)),
                PendingIdleAction::ChangeThinking(_)
            )
            | (
                Some(PendingIdleAction::ChangeThinkingLevel(_)),
                PendingIdleAction::ChangeThinkingLevel(_)
            )
            | (
                Some(
                    PendingIdleAction::ChangeThinking(_)
                        | PendingIdleAction::ChangeThinkingLevel(_)
                        | PendingIdleAction::CycleThinking
                ),
                PendingIdleAction::CycleThinking
            )
    );
    if same_kind {
        let _ = queue.pop_back();
    }
    queue.push_back(action);
}

enum Idle {
    Submit(ComposedInput),
    Command(String),
    SessionLifecycle(ExtensionSessionLifecycleRequest),
    /// The live-reload supervisor has stale layers past their debounce deadline.
    /// `run_interactive_once` applies them in the fixed order resources →
    /// extensions → host before the next prompt is drawn.
    ReloadDue,
    GoalContinuation,
    CycleThinking,
    Quit,
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_prompt<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    scroll_tick: &mut Interval,
    extension_tick: &mut Interval,
    executable_extensions: &mut crate::extensions::ExecutableExtensions,
    goal_deadline: Option<Instant>,
    reload_tick: &mut Interval,
    reload_watcher: &crate::reload::ReloadWatcher,
    reload: &mut crate::reload::ReloadSupervisor,
) -> anyhow::Result<Idle>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut scroll_dirty = false;
    loop {
        if shell.close_requested() {
            return Ok(Idle::Quit);
        }
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                return Ok(Idle::Quit);
            }
            maybe = input.next() => {
                let event = match maybe {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => return Err(error.into()),
                    None => return Ok(Idle::Quit),
                };
                // Search owns its query before any extension or clipboard
                // admission; pasted paths/text must not become composer input.
                if shell.intercept_transcript_input(&event) {
                    continue;
                }
                observe_extension_terminal_event(executable_extensions, &event);
                if matches!(&event, Event::Key(key) if keymap::is_close_key(key)) {
                    shell.request_close();
                    return Ok(Idle::Quit);
                }
                // Panels are driven by picker functions that own the event
                // stream. If a panel leaks here (shouldn't happen), Esc closes it.
                if shell.has_panel() {
                    match &event {
                        Event::Mouse(_) => continue,
                        Event::Resize(columns, rows) => {
                            shell.set_size(*columns, *rows);
                            shell.render();
                            continue;
                        }
                        Event::Key(key)
                            if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc =>
                        {
                            shell.close_panel();
                            shell.render();
                            continue;
                        }
                        _ => continue,
                    }
                }
                if shell.has_overlay() {
                    match event {
                        Event::Mouse(_) => continue,
                        Event::Resize(columns, rows) => {
                            shell.set_size(columns, rows);
                            shell.render();
                            continue;
                        }
                        _ => match shell.overlay_input(&event) {
                            OverlayInputResult::Consumed => {
                                shell.render();
                                continue;
                            }
                            OverlayInputResult::Closed => {
                                shell.clear_error();
                                shell.render();
                                continue;
                            }
                            OverlayInputResult::Legacy => {
                                shell.close_overlay();
                                shell.clear_error();
                                shell.render();
                                continue;
                            }
                        },
                    }
                }
                if let Some(shortcut) = executable_extensions.dispatch_shortcut_for_event(&event) {
                    shell.notice(format!(
                        "running extension shortcut {}: {}",
                        shortcut.extension, shortcut.description
                    ));
                    shell.render();
                    continue;
                }
                // The clipboard gesture is resolved before translation: the
                // native read is asynchronous and the translator has no action
                // that can await it. A failed read falls through untouched.
                if paste_clipboard_text(shell, &event).await {
                    continue;
                }
                match shell.translate_input(Some(event), false) {
                    InputAction::SlashMenu(action) => {
                        if shell.slash_menu(action) {
                            return Ok(Idle::Command(shell.drain_editor()));
                        }
                        shell.render();
                    }
                    InputAction::CompleteSlashCommand => {
                        shell.complete_slash_command();
                        shell.render();
                    }
                    InputAction::CompletePath => {
                        if shell.accept_extension_autocomplete() {
                            shell.render();
                        } else if !executable_extensions
                            .request_editor_autocomplete(shell.extension_editor_snapshot())
                        {
                            shell.complete_path();
                            shell.render();
                        }
                    }
                    InputAction::Edit(action) => {
                        shell.apply_edit(action);
                        shell.render();
                    }
                    InputAction::Resize(columns, rows) => {
                        shell.set_size(columns, rows);
                        shell.render();
                    }
                    InputAction::Scroll(direction) => {
                        shell.scroll(direction);
                        shell.render();
                    }
                    InputAction::ScrollLines(direction) => {
                        shell.scroll_lines(direction);
                        scroll_dirty = true;
                    }
                    InputAction::JumpToTail => {
                        shell.jump_to_tail();
                        shell.render();
                    }
                    InputAction::SelectAllTranscript => {
                        shell.select_all_transcript();
                        shell.render();
                    }
                    InputAction::CopyTranscriptSelection => {
                        if shell.copy_selected_plain_text().is_some() {
                            shell.notice("copied to clipboard");
                        }
                        shell.render();
                    }
                    InputAction::TranscriptPointer(gesture) => {
                        match gesture {
                            crate::tui::keymap::PointerGesture::Begin { row, col, extend } => {
                                shell.begin_transcript_selection(row, col, extend);
                            }
                            crate::tui::keymap::PointerGesture::Extend { row, col } => {
                                shell.extend_transcript_selection(row, col);
                            }
                            crate::tui::keymap::PointerGesture::End { row, col } => {
                                shell.end_transcript_selection(row, col);
                            }
                        }
                        shell.render();
                    }
                    InputAction::ShowCompactionSummary => {
                        shell.show_compaction_summary();
                        shell.render();
                    }
                    InputAction::ToggleDisclosure => {
                        shell.toggle_disclosure();
                        shell.render();
                    }
                    InputAction::CycleThinking => return Ok(Idle::CycleThinking),
                    InputAction::EditQueued => {
                        shell.edit_queued_message();
                        shell.render();
                    }
                    InputAction::ClearEditor => {
                        shell.clear_editor();
                        shell.render();
                    }
                    InputAction::Close => {
                        shell.clear_error();
                        shell.render();
                    }
                    InputAction::Submit(_) => return Ok(Idle::Submit(shell.drain_composed())),
                    InputAction::Command(text) => return Ok(Idle::Command(shell.consume_command_text(text))),
                    InputAction::Closed => return Ok(Idle::Quit),
                    InputAction::FocusGained => apply_focus_transition(shell, true),
                    InputAction::FocusLost => apply_focus_transition(shell, false),
                    InputAction::Ignore | InputAction::Abort | InputAction::DispatchQueued
                    | InputAction::Queue(_) | InputAction::Steer(_) => {}
                }
            }
            // Mouse/trackpad events arrive in bursts. Apply every delta to
            // state, but draw at most once per frame so a large transcript
            // cannot leave a backlog that appears as post-scroll inertia.
            _ = scroll_tick.tick(), if scroll_dirty => {
                shell.render();
                scroll_dirty = false;
            },
            _ = async {
                match goal_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => return Ok(Idle::GoalContinuation),
            _ = extension_tick.tick() => {
                // A modal is an in-progress interactive action, not a safe
                // active-session replacement boundary. Keep the bounded
                // request queued until the frontend returns to its prompt.
                if !shell.has_panel() && !shell.has_overlay() {
                    if let Some(request) = executable_extensions.next_session_lifecycle_request() {
                        if matches!(
                            request.operation(),
                            ExtensionSessionLifecycleOperation::Reload
                        ) {
                            // API 0.3 `session/reload` is a first-class reload
                            // trigger (Pi's `ctx.reload()`), not only a file
                            // change: mark resources and extensions stale and
                            // let the idle boundary apply them in order.
                            reload.request(
                                std::time::Instant::now(),
                                crate::reload::ReloadRequester::ExtensionSessionReload,
                            );
                        }
                        return Ok(Idle::SessionLifecycle(request));
                    }
                }
                if apply_extension_background(shell, executable_extensions) {
                    shell.render();
                }
            }
            // Live reload: sample on the poll interval, then hand a due pass
            // back to the caller. `begin` and the reloads themselves stay in
            // the outer loop, which owns `&mut App`.
            _ = reload_tick.tick() => {
                let now = std::time::Instant::now();
                if let Some(observed) =
                    reload_watcher.poll(reload, &crate::reload::SystemMetadata, now)
                {
                    if observed.first_observation {
                        if let Some(notice) = reload_watcher.watches().limit_notice() {
                            shell.notice(notice);
                            shell.render();
                        }
                    }
                    for notice in reload.checked_problems(
                        crate::reload::ReloadComponent::WatchCoverage,
                        observed.cap_notice().into_iter().collect(),
                        false,
                    ) {
                        shell.notice(notice);
                        shell.render();
                    }
                    if observed.anything_changed() && !observed.due {
                        // Queued evidence always carries a debounce deadline;
                        // without one it would wait forever, so say so loudly
                        // rather than silently never applying it.
                        debug_assert!(
                            reload.deadline().is_some(),
                            "queued live-reload evidence must carry a debounce deadline"
                        );
                    }
                }
                if reload.is_due(now) {
                    return Ok(Idle::ReloadDue);
                }
            }
        }
    }
}

/// Durable goal state a live run can still reach.
///
/// `Run` owns `&mut Agent` for the whole turn, so the frontend cannot lend it
/// `&App` and cannot call the idle goal dispatcher. These are exactly the
/// application fields the goal command touches: the shared durable store, the
/// session key that store is addressed by, and the driver, whose state is
/// already `Arc`-shared with `app.goal_driver`, so a command applied mid-run is
/// the same command the idle boundary would have applied.
#[derive(Clone)]
pub(crate) struct GoalAccess {
    store: Arc<octet_agent::DurableGoalStore>,
    driver: octet_agent::GoalDriver,
    session_id: String,
}

/// A live run that cannot address the durable goal at all.
///
/// `/goal` under an active run fails closed with this reason: it is rendered to
/// the user, never queued to the next idle boundary, and never a silent no-op.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ActiveGoalError {
    /// The session carries no durable goal identity, so neither a read nor a
    /// mutation can be addressed. Bootstrap requires one, so this means the run
    /// cannot reconstruct the key it would have to mutate.
    #[error("the session has no durable goal identity, so the goal store cannot be addressed")]
    UnaddressableSession,
}

impl GoalAccess {
    /// Address the durable goal of an application whose `Agent` is not borrowed.
    fn from_app(app: &App) -> Result<Self, ActiveGoalError> {
        Self::from_parts(
            app.goal_store.clone(),
            app.goal_driver.clone(),
            app.goal_session_id.clone(),
        )
    }

    fn from_parts(
        store: Arc<octet_agent::DurableGoalStore>,
        driver: octet_agent::GoalDriver,
        session_id: String,
    ) -> Result<Self, ActiveGoalError> {
        if session_id.is_empty() {
            return Err(ActiveGoalError::UnaddressableSession);
        }
        Ok(Self {
            store,
            driver,
            session_id,
        })
    }

    fn status_text(&self) -> anyhow::Result<String> {
        let goal = self.store.get(&self.session_id)?;
        let Some(goal) = goal else {
            return Ok("No goal is configured for this session.".to_owned());
        };
        let remaining = goal
            .turn_budget
            .map(|budget| {
                format!(
                    " · {} turn{} remaining",
                    budget.saturating_sub(goal.turns_used),
                    if budget.saturating_sub(goal.turns_used) == 1 {
                        ""
                    } else {
                        "s"
                    }
                )
            })
            .unwrap_or_default();
        Ok(format!(
            "{} goal: {}{}",
            goal_status_label(goal.status),
            goal.objective,
            remaining
        ))
    }

    fn arm_deadline(&self) -> anyhow::Result<Option<Instant>> {
        match self.driver.turn_settled(GoalTurnSource::User, "", false)? {
            GoalDecision::Wait { delay, .. } => Ok(Some(Instant::now() + delay)),
            _ => Ok(None),
        }
    }

    fn recovered_deadline(&self) -> anyhow::Result<Option<Instant>> {
        if self
            .store
            .get(&self.session_id)?
            .is_some_and(|goal| goal.status == GoalStatus::Active)
        {
            self.arm_deadline()
        } else {
            Ok(None)
        }
    }
}

fn goal_status_label(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "Active",
        GoalStatus::Paused => "Paused",
        GoalStatus::Complete => "Complete",
        GoalStatus::Blocked => "Blocked",
        GoalStatus::BudgetLimited => "Budget limited",
        _ => "Unknown",
    }
}

fn recovered_goal_deadline(app: &App) -> anyhow::Result<Option<Instant>> {
    GoalAccess::from_app(app)?.recovered_deadline()
}

/// The idle dispatcher's `/goal` arm: address the durable goal of the current
/// application and apply the command now. Idle and mid-run `/goal` both call
/// [`apply_goal_command`], so their durable effect and driver state cannot
/// diverge.
fn apply_idle_goal_command(
    app: &App,
    shell: &mut InteractiveShell,
    command: commands::GoalCommand,
    goal_deadline: &mut Option<Instant>,
) -> anyhow::Result<()> {
    apply_goal_command(&GoalAccess::from_app(app)?, shell, command, goal_deadline)
}

/// Apply `/goal` now, against durable state, wherever the command was typed.
///
/// The same producer serves the idle dispatcher and a live run: `/goal` is
/// never queued to the next idle boundary, so a mid-run objective, pause,
/// resume, or clear mutates the store at that instant and leaves the driver
/// armed exactly as an idle command would.
fn apply_goal_command(
    access: &GoalAccess,
    shell: &mut InteractiveShell,
    command: commands::GoalCommand,
    goal_deadline: &mut Option<Instant>,
) -> anyhow::Result<()> {
    use octet_agent::GoalAction as DurableGoalAction;

    match command {
        commands::GoalCommand::Help => shell.show_report_text(
            "Goal help",
            "Browse commands for the current session goal",
            "Goal commands\n\n/goal <objective>\n/goal status\n/goal pause\n/goal resume\n/goal clear"
                .to_owned(),
        ),
        commands::GoalCommand::Status => match access.status_text() {
            Ok(status) => shell.show_report_text(
                "Goal status",
                "Review the current session goal",
                status,
            ),
            Err(error) => shell.error(format!("unable to read goal: {error}")),
        },
        commands::GoalCommand::Set(objective) => {
            match access.store.set(&access.session_id, &objective, None) {
                Ok(goal) => {
                    access.driver.user_spoke();
                    *goal_deadline = access.arm_deadline()?;
                    shell.notice(format!(
                        "goal set · {} goal: {}",
                        goal_status_label(goal.status), goal.objective
                    ));
                }
                Err(error) => shell.error(format!("unable to set goal: {error}")),
            }
        }
        commands::GoalCommand::Pause => {
            match access
                .store
                .apply(&access.session_id, DurableGoalAction::Pause)
            {
                Ok(Some(goal)) => {
                    *goal_deadline = None;
                    access.driver.user_spoke();
                    shell.notice(format!("goal paused · {}", goal.objective));
                }
                Ok(None) => shell.error("no goal is configured for this session".to_owned()),
                Err(error) => shell.error(format!("unable to pause goal: {error}")),
            }
        }
        commands::GoalCommand::Resume => {
            match access
                .store
                .apply(&access.session_id, DurableGoalAction::Resume)
            {
                Ok(Some(goal)) => {
                    access.driver.user_spoke();
                    *goal_deadline = access.arm_deadline()?;
                    shell.notice(format!("goal resumed · {}", goal.objective));
                }
                Ok(None) => shell.error("no goal is configured for this session".to_owned()),
                Err(error) => shell.error(format!("unable to resume goal: {error}")),
            }
        }
        commands::GoalCommand::Clear => {
            match access
                .store
                .apply(&access.session_id, DurableGoalAction::Clear)
            {
                Ok(None) => {
                    *goal_deadline = None;
                    access.driver.user_spoke();
                    shell.notice("goal cleared");
                }
                Ok(Some(_)) => unreachable!("clearing a goal returns no state"),
                Err(error) => shell.error(format!("unable to clear goal: {error}")),
            }
        }
    }
    Ok(())
}
fn settle_goal(
    app: &App,
    shell: &mut InteractiveShell,
    source: GoalTurnSource,
    response: &str,
    made_tool_call: bool,
    completed: bool,
) -> Option<GoalDecision> {
    if !completed {
        let _ = app.goal_driver.session_error();
        return None;
    }
    match app
        .goal_driver
        .turn_settled(source, response, made_tool_call)
    {
        Ok(decision) => Some(decision),
        Err(error) => {
            let _ = app.goal_driver.session_error();
            shell.error(format!("unable to update goal state: {error}"));
            None
        }
    }
}

fn answer_now_prompt(instruction: Option<String>) -> String {
    const DIRECTIVE: &str =
        "Answer now using only the evidence already gathered. Do not call tools. State any remaining uncertainty.";
    instruction
        .map(|instruction| format!("{}\n\n{DIRECTIVE}", instruction.trim()))
        .unwrap_or_else(|| DIRECTIVE.to_owned())
}

fn answer_now_input(instruction: Option<String>) -> ComposedInput {
    let display = instruction
        .as_deref()
        .map(str::trim)
        .filter(|instruction| !instruction.is_empty())
        .map(|instruction| format!("/answer {instruction}"))
        .unwrap_or_else(|| "/answer".to_owned());
    ComposedInput::for_answer(display, answer_now_prompt(instruction))
}

fn queue_command(command: Command, queue: &mut VecDeque<PendingIdleAction>) -> anyhow::Result<()> {
    let action = match command {
        Command::Login(provider) => PendingIdleAction::Login(provider),
        Command::Logout(provider) => PendingIdleAction::Logout(provider),
        Command::Model(Some(id)) => PendingIdleAction::ChangeModel(ModelId(id)),
        Command::Model(None) => PendingIdleAction::PickModel,
        Command::Thinking(Some(level)) => match ThinkingLevel::parse(&level)? {
            ThinkingLevel::Off => PendingIdleAction::ChangeThinking(ReasoningConfig::Off),
            level => PendingIdleAction::ChangeThinkingLevel(level),
        },
        Command::Thinking(None) => PendingIdleAction::PickThinking,
        Command::New => PendingIdleAction::NewSession,
        Command::Resume(id) => PendingIdleAction::ResumeSession(id),
        Command::Fork => PendingIdleAction::Fork,
        Command::Clone => PendingIdleAction::Clone,
        Command::Compact => PendingIdleAction::Compact,
        Command::CompactWithInstructions(instructions) => {
            PendingIdleAction::CompactWithInstructions(instructions)
        }
        Command::AutoCompact(setting) => PendingIdleAction::AutoCompact(setting),
        Command::Context => PendingIdleAction::ShowContext,
        Command::Reload => PendingIdleAction::ReloadResources,
        Command::Skills(sub) => PendingIdleAction::Skills(sub),
        // `/goal` is applied the instant it is submitted, mid-run or idle. It
        // has no queued form: queueing it is exactly the reported defect, so an
        // attempt to queue one fails closed instead of deferring the command.
        Command::Goal(_) => {
            anyhow::bail!("`/goal` is applied immediately and is never queued as an idle action")
        }
        other => anyhow::bail!("{other:?} cannot be queued as an idle action"),
    };
    push_pending_action(queue, action);
    Ok(())
}

/// Drive only cancellation-safe, locally owned futures here (never a detached
/// lifecycle worker). Dropping the future stops local work, not remote billing
/// or effects that completed before cancellation. Animation remains owned by
/// the renderer thread; input does not need a periodic redraw loop.
async fn await_with_ctrl_c<F, S>(
    future: F,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> Option<F::Output>
where
    F: std::future::Future,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut future = Box::pin(future);
    let mut input_open = true;
    loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.request_close();
                return None;
            }
            event = input.next(), if input_open => match event {
                Some(Ok(event)) => {
                    if shell.intercept_transcript_input(&event) {
                        continue;
                    }
                    // A clipboard paste is still admitted while a cancellable
                    // operation runs; a failed read keeps the event unchanged.
                    if paste_clipboard_text(shell, &event).await {
                        continue;
                    }
                    if handle_cancellable_wait_input(shell, event) {
                        return None;
                    }
                }
                Some(Err(error)) => {
                    shell.error(format!("terminal input failed: {error}"));
                    shell.request_close();
                    return None;
                }
                None => input_open = false,
            },
            output = &mut future => return Some(output),
        }
    }
}

fn handle_cancellable_wait_input(shell: &mut InteractiveShell, event: Event) -> bool {
    if matches!(&event, Event::Key(key) if keymap::is_close_key(key)) {
        shell.request_close();
        return true;
    }
    if let Event::Resize(columns, rows) = event {
        shell.set_size(columns, rows);
        shell.render();
        return false;
    }
    if shell.has_overlay() {
        match shell.overlay_input(&event) {
            OverlayInputResult::Consumed => {}
            OverlayInputResult::Closed => shell.clear_error(),
            OverlayInputResult::Legacy => shell.close_overlay(),
        }
        shell.render();
        return false;
    }
    match shell.translate_input(Some(event), true) {
        InputAction::Abort | InputAction::DispatchQueued => return true,
        InputAction::Closed => {
            shell.request_close();
            return true;
        }
        InputAction::ClearEditor => shell.clear_editor(),
        InputAction::Edit(action) => shell.apply_edit(action),
        InputAction::ToggleDisclosure => shell.toggle_disclosure(),
        InputAction::ShowCompactionSummary => shell.show_compaction_summary(),
        InputAction::Scroll(direction) => shell.scroll(direction),
        InputAction::ScrollLines(direction) => shell.scroll_lines(direction),
        InputAction::JumpToTail => shell.jump_to_tail(),
        InputAction::SlashMenu(action) => {
            shell.slash_menu(action);
        }
        InputAction::CompleteSlashCommand => shell.complete_slash_command(),
        InputAction::CompletePath => shell.complete_path(),
        InputAction::Queue(_)
        | InputAction::Steer(_)
        | InputAction::Submit(_)
        | InputAction::Command(_) => {
            // This operation has no RunControl owner. Keep the complete draft
            // rather than pretending Enter delivered it or discarding it.
            shell.notice("operation in progress · draft kept for the next prompt");
        }
        InputAction::Close => shell.clear_error(),
        _ => return false,
    }
    shell.render();
    false
}

/// Manual and queued compaction share the same input owner and settlement path.
async fn compact_interactively<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
    force: bool,
    instructions: Option<&str>,
) where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    if let Some(message) = cost_limit_message(app) {
        shell.error(message);
        return;
    }
    shell.set_run_label("compacting…");
    shell.render();
    app.executable_extensions.notify_compaction_started_all();
    let original_keep = app.config.compaction.keep_recent_tokens;
    if force {
        app.config.compaction.keep_recent_tokens = 1;
    }
    let result = await_with_ctrl_c(
        crate::compaction::attempt_compaction_with_instructions(app, instructions),
        shell,
        input,
    )
    .await;
    app.config.compaction.keep_recent_tokens = original_keep;
    // Clear the transient activity on every result before publishing the one
    // settled frame, including errors and cancellation of a held-open response.
    shell.set_run_label("idle");
    match result {
        Some(Ok(outcome)) => {
            app.executable_extensions.notify_compaction_settled_all();
            report_compaction(shell, &outcome, app.agent.session());
        }
        Some(Err(error)) => {
            app.executable_extensions
                .notify_compaction_failed_all(&error.to_string());
            shell.error(format!("compaction failed: {error}"));
        }
        None => {
            // A cancelled boundary still settles; nothing failed.
            app.executable_extensions.notify_compaction_settled_all();
            shell.notice("compaction cancelled · completed work is retained");
        }
    }
    if let Some(message) = cost_limit_message(app) {
        shell.error(message);
    }
    update_status(shell, app);
    shell.render();
}

const LIFECYCLE_SHUTDOWN_GRACE: Duration = Duration::from_millis(1400);
const RAW_CTRL_C_SIGNAL: i32 = 2;

fn is_ctrl_c(key: &crossterm::event::KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Declared clipboard paste gesture. `app.clipboard.pasteImage` defaults to
/// ctrl+v, or alt+v on Windows (`tui/keymap/keybindings.rs`). The gesture is
/// resolved here rather than in the keymap because the native read is
/// asynchronous: the translator has no action that can await it. The event is
/// otherwise unbound, so this consumes nothing the translator would deliver.
///
/// Required keymap change (recorded, not made — another worker owns
/// `keymap.rs`): add `InputAction::PasteImage`, return it from
/// `translate_input` for this same key, and reserve the key in
/// `is_reserved_extension_shortcut`; then this predicate can be deleted.
fn is_clipboard_paste_key(key: &crossterm::event::KeyEvent) -> bool {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return false;
    }
    let expected = if cfg!(target_os = "windows") {
        KeyModifiers::ALT
    } else {
        KeyModifiers::CONTROL
    };
    key.code == KeyCode::Char('v') && key.modifiers == expected
}

/// Insert native clipboard text into the composer through the bracketed-paste
/// path, so consent, path attachment, and large-paste classification stay
/// identical to a terminal-originated paste. Returns `true` only when the
/// gesture was consumed: an unavailable, empty, or oversized clipboard falls
/// through to the existing behaviour (terminal bracketed paste plus the
/// retained copy buffer) instead of reporting a paste that did not happen.
async fn paste_clipboard_text(shell: &mut InteractiveShell, event: &Event) -> bool {
    if !matches!(event, Event::Key(key) if is_clipboard_paste_key(key)) {
        return false;
    }
    let Some(text) = clipboard_read::read_text().await else {
        return false;
    };
    shell.apply_edit(crate::tui::keymap::EditAction::Paste(text));
    shell.render();
    true
}

/// Settle only into the exact editor revision that admitted the native read.
/// Any intervening text/cursor edit (including extension replacement) invalidates
/// both its text and fallback gesture. The normal composer must still own focus;
/// search/panels/tool input must never receive its result. Snapshot text is cloned
/// only at admission, completion, and fallback replay—not on each loop poll.
fn settle_active_clipboard_read(
    shell: &mut InteractiveShell,
    revision: u64,
    text: Option<String>,
    gesture: Option<Event>,
) -> Option<Event> {
    let editor = shell.extension_editor_snapshot();
    if !editor.focused || editor.revision != revision {
        return None;
    }
    if let Some(text) = text {
        shell.apply_edit(crate::tui::keymap::EditAction::Paste(text));
        shell.render();
        None
    } else {
        gesture
    }
}

/// Native **text** clipboard read (parity row 2c.6). Clipboard image capture is
/// an explicit exclusion, so only text ever leaves the clipboard and nothing in
/// this module writes to it. The existing write transport (pbcopy plus OSC 52 in
/// `tui/view.rs`) is untouched and remains the fallback.
///
/// Every helper is bounded by a deadline and a byte cap, and every failure fails
/// closed. A helper that blocks — a disconnected display, a wedged Wayland
/// compositor, no `pbpaste` on PATH — must never hold the interactive loop.
mod clipboard_read {
    use std::process::Stdio;
    use std::time::Duration;

    /// Deadline for one helper process.
    const READ_TIMEOUT: Duration = Duration::from_millis(600);
    /// Accepted clipboard text. A larger payload fails closed rather than
    /// pasting a silently truncated document.
    const MAX_TEXT_BYTES: usize = 1024 * 1024;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Platform {
        MacOs,
        Linux,
        Windows,
    }

    fn host_platform() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Helper {
        program: String,
        args: Vec<String>,
    }

    fn helper(program: &str, args: &[&str]) -> Helper {
        Helper {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    /// Declared helper order. A platform helper is used when it exists, so an
    /// environment that declares no display yields no helper at all and the read
    /// fails closed instead of scraping an unrelated transport.
    fn helpers(platform: Platform, env: impl Fn(&str) -> Option<String>) -> Vec<Helper> {
        match platform {
            Platform::MacOs => vec![helper("pbpaste", &[])],
            // `clip` writes only; PowerShell is the declared text reader.
            Platform::Windows => vec![helper(
                "powershell",
                &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
            )],
            Platform::Linux => {
                let mut helpers = Vec::new();
                if env("TERMUX_VERSION").is_some() {
                    helpers.push(helper("termux-clipboard-get", &[]));
                }
                if env("WAYLAND_DISPLAY").is_some() {
                    helpers.push(helper("wl-paste", &["--no-newline", "--type", "text"]));
                }
                if env("DISPLAY").is_some() {
                    helpers.push(helper("xclip", &["-selection", "clipboard", "-out"]));
                    helpers.push(helper("xsel", &["--clipboard", "--output"]));
                }
                helpers
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        /// The helper exited successfully and produced bytes.
        Text(Vec<u8>),
        /// The helper exited successfully and produced nothing.
        Empty,
        /// The helper is missing, failed, timed out, or exceeded the byte cap.
        Failed,
    }

    /// Decide one helper's contribution. `Err(())` means "try the next helper";
    /// every other outcome settles the read, matching the reference loop, where
    /// a successful helper that printed nothing reports an empty clipboard
    /// rather than leaking into the next transport.
    fn settle(outcome: Outcome) -> Result<Option<String>, ()> {
        match outcome {
            Outcome::Failed => Err(()),
            Outcome::Empty => Ok(None),
            Outcome::Text(bytes) => Ok(decode(&bytes)),
        }
    }

    /// Invalid UTF-8 is replaced rather than dropped, matching the reference
    /// reader's `toString("utf8")`. At this point the payload is already
    /// bounded, so only a genuinely empty clipboard yields `None`.
    fn decode(bytes: &[u8]) -> Option<String> {
        if bytes.is_empty() || bytes.len() > MAX_TEXT_BYTES {
            return None;
        }
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    fn classify(bytes: Vec<u8>) -> Outcome {
        if bytes.len() > MAX_TEXT_BYTES {
            Outcome::Failed
        } else if bytes.is_empty() {
            Outcome::Empty
        } else {
            Outcome::Text(bytes)
        }
    }

    async fn run(helper: &Helper) -> Outcome {
        let command = tokio::process::Command::new(&helper.program)
            .args(&helper.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // A helper that ignores the deadline is killed when its future is
            // dropped, so cancellation cannot leak a blocked child.
            .kill_on_drop(true)
            .spawn();
        let Ok(mut child) = command else {
            return Outcome::Failed;
        };
        let Some(mut stdout) = child.stdout.take() else {
            return Outcome::Failed;
        };
        let operation = async move {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            let mut bounded = (&mut stdout).take(MAX_TEXT_BYTES as u64 + 1);
            bounded.read_to_end(&mut bytes).await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((bytes, status))
        };
        match tokio::time::timeout(READ_TIMEOUT, operation).await {
            // A helper that reports failure is transport failure, not an empty
            // clipboard: `settle` then tries the next declared helper.
            Ok(Ok((bytes, status))) if status.success() => classify(bytes),
            _ => Outcome::Failed,
        }
    }

    pub(super) async fn read_text() -> Option<String> {
        #[cfg(test)]
        if let Some(helper) = TEST_HELPER.with(|slot| slot.borrow_mut().take()) {
            return helper.await;
        }
        #[cfg(test)]
        if let Some(overridden) = test_override() {
            return overridden;
        }
        for helper in helpers(host_platform(), |name| std::env::var(name).ok()) {
            if let Ok(text) = settle(run(&helper).await) {
                return text;
            }
        }
        None
    }

    #[cfg(test)]
    type TestHelper = std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>;

    #[cfg(test)]
    thread_local! {
        /// One-shot controllable helper; no developer clipboard is accessed.
        static TEST_HELPER: std::cell::RefCell<Option<TestHelper>> =
            const { std::cell::RefCell::new(None) };
        /// The outer `None` means no override; per-thread state isolates tests.
        static OVERRIDE: std::cell::RefCell<Option<Option<String>>> =
            const { std::cell::RefCell::new(None) };
    }

    #[cfg(test)]
    pub(super) fn set_test_helper(
        helper: impl std::future::Future<Output = Option<String>> + Send + 'static,
    ) {
        TEST_HELPER.with(|slot| *slot.borrow_mut() = Some(Box::pin(helper)));
    }

    #[cfg(test)]
    fn test_override() -> Option<Option<String>> {
        OVERRIDE.with(|slot| slot.borrow().clone())
    }

    #[cfg(test)]
    pub(super) fn set_test_text(text: Option<String>) {
        OVERRIDE.with(|slot| *slot.borrow_mut() = Some(text));
    }

    #[cfg(test)]
    pub(super) fn clear_test_text() {
        OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
            move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        }

        #[test]
        fn linux_helper_order_follows_the_declared_environment_gates() {
            let all = helpers(
                Platform::Linux,
                env(&[
                    ("TERMUX_VERSION", "1"),
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("DISPLAY", ":0"),
                ]),
            );
            let programs: Vec<&str> = all.iter().map(|helper| helper.program.as_str()).collect();
            assert_eq!(
                programs,
                ["termux-clipboard-get", "wl-paste", "xclip", "xsel"]
            );
            assert_eq!(
                all[1].args,
                [
                    "--no-newline".to_owned(),
                    "--type".to_owned(),
                    "text".to_owned()
                ]
            );
            assert_eq!(
                all[2].args,
                ["-selection", "clipboard", "-out"].map(str::to_owned)
            );
        }

        #[test]
        fn a_session_with_no_declared_display_yields_no_helper() {
            assert!(helpers(Platform::Linux, env(&[])).is_empty());
            assert_eq!(helpers(Platform::Linux, env(&[("DISPLAY", ":0")])).len(), 2);
            assert_eq!(
                helpers(Platform::Linux, env(&[("WAYLAND_DISPLAY", "wayland-1")])).len(),
                1
            );
        }

        #[test]
        fn macos_and_windows_read_through_one_declared_helper() {
            assert_eq!(helpers(Platform::MacOs, env(&[]))[0].program, "pbpaste");
            let windows = helpers(Platform::Windows, env(&[]));
            assert_eq!(windows[0].program, "powershell");
            assert!(windows[0]
                .args
                .iter()
                .any(|arg| arg.contains("Get-Clipboard")));
            assert!(!windows[0].args.iter().any(|arg| arg.contains("clip\"")));
        }

        #[test]
        fn helper_failure_tries_the_next_transport_and_empty_success_settles() {
            assert_eq!(settle(Outcome::Failed), Err(()));
            assert_eq!(settle(Outcome::Empty), Ok(None));
            assert_eq!(
                settle(Outcome::Text(b"from clipboard".to_vec())),
                Ok(Some("from clipboard".to_owned()))
            );
        }

        #[test]
        fn oversized_payloads_fail_closed_instead_of_pasting_a_prefix() {
            assert_eq!(classify(vec![b'x'; MAX_TEXT_BYTES + 1]), Outcome::Failed);
            assert_eq!(classify(Vec::new()), Outcome::Empty);
            assert_eq!(decode(&[]), None);
            assert_eq!(decode(&vec![b'x'; MAX_TEXT_BYTES + 1]), None);
            // The reference reader replaces invalid UTF-8 instead of dropping
            // the whole clipboard.
            assert_eq!(decode(&[0xff, 0xfe]), Some("\u{fffd}\u{fffd}".to_owned()));
        }

        #[tokio::test]
        async fn a_missing_helper_fails_closed_without_panicking() {
            let missing = Helper {
                program: "octet-no-such-clipboard-helper".to_owned(),
                args: Vec::new(),
            };
            assert_eq!(run(&missing).await, Outcome::Failed);
        }

        /// Exercise the real spawn/decode/reap path against throwaway helper
        /// programs. The developer's own clipboard is never read or written.
        #[cfg(unix)]
        #[tokio::test]
        async fn a_real_helper_is_read_bounded_and_its_exit_status_is_honoured() {
            let text = Helper {
                program: "/bin/echo".to_owned(),
                args: vec!["clipboard text".to_owned()],
            };
            assert_eq!(
                run(&text).await,
                Outcome::Text(b"clipboard text\n".to_vec())
            );
            assert_eq!(
                settle(Outcome::Text(b"clipboard text\n".to_vec())),
                Ok(Some("clipboard text\n".to_owned()))
            );

            let empty = Helper {
                // /bin/true is absent on macOS; use the portable shell builtin.
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "exit 0".to_owned()],
            };
            assert_eq!(run(&empty).await, Outcome::Empty);

            let failing = Helper {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "exit 3".to_owned()],
            };
            assert_eq!(run(&failing).await, Outcome::Failed);

            // A helper that never returns is killed at the deadline instead of
            // holding the interactive loop.
            let wedged = Helper {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "exec sleep 30".to_owned()],
            };
            let started = std::time::Instant::now();
            assert_eq!(run(&wedged).await, Outcome::Failed);
            assert!(
                started.elapsed() < READ_TIMEOUT * 6,
                "bounded helper read took {:?}",
                started.elapsed()
            );
        }

        #[tokio::test]
        async fn the_test_override_replaces_the_platform_read() {
            set_test_text(Some("overridden".to_owned()));
            assert_eq!(read_text().await, Some("overridden".to_owned()));
            set_test_text(None);
            assert_eq!(read_text().await, None);
            clear_test_text();
        }
    }
}

/// Forward only normalized, bounded observations. Extensions never receive the
/// terminal event itself and cannot influence the host keymap or resize path.
fn observe_extension_terminal_event(
    executable_extensions: &mut crate::extensions::ExecutableExtensions,
    event: &Event,
) {
    if executable_extensions.terminal_grant_is_active() {
        // A ceded holder owns raw input. A second observer must never see the
        // same keystroke, and the host must not answer a resize the child owns.
        return;
    }
    match event {
        Event::Resize(columns, rows) => {
            executable_extensions.observe_terminal_resize(*columns, *rows);
        }
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            // Debug formatting escapes control characters in `KeyCode::Char`,
            // so this never turns a terminal escape sequence into extension data.
            executable_extensions
                .observe_terminal_input(format!("key:{:?}:{:?}", key.modifiers, key.code));
        }
        Event::Paste(text) => {
            let mut normalized = String::from("paste:");
            for character in text.chars() {
                let encoded = match character {
                    '\n' => "\\n".to_owned(),
                    '\r' => "\\r".to_owned(),
                    '\t' => "\\t".to_owned(),
                    character if character.is_control() => "�".to_owned(),
                    character => character.to_string(),
                };
                if normalized.len().saturating_add(encoded.len())
                    > MAX_EXTENSION_TERMINAL_INPUT_BYTES
                {
                    break;
                }
                normalized.push_str(&encoded);
            }
            executable_extensions.observe_terminal_input(normalized);
        }
        _ => {}
    }
}

/// Keep raw-terminal input, resize handling, rendering, and termination
/// signals live while a bounded lifecycle operation runs elsewhere. Ordinary
/// typing/paste stays in the draft, including input retained by the startup
/// appearance probe. Ctrl-C becomes the same coordinated SIGINT shutdown used
/// by the signal thread; Ctrl-D records a close request and lets the owned
/// operation settle before its caller exits.
///
/// An empty `label` is the silent startup form: nothing is rendered for the
/// phase, while cancellation and shutdown diagnostics still name the operation
/// as `startup` (see [`run_blocking_startup_lifecycle`]).
async fn await_lifecycle<F, T, S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    label: &str,
    operation: F,
) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut operation = Box::pin(operation);
    let mut input_open = true;
    // Display text is empty for silent startup phases; diagnostics still need a
    // name so a cancelled or signalled wait remains attributable.
    let operation_name = if label.is_empty() { "startup" } else { label };
    shell.set_run_label(label);
    shell.render();

    loop {
        tokio::select! {
            biased;
            signal = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.set_run_label("shutting down…");
                shell.render();
                let _ = tokio::time::timeout(LIFECYCLE_SHUTDOWN_GRACE, &mut operation).await;
                anyhow::bail!("shutdown signal {signal} received during {operation_name}");
            }
            result = &mut operation => {
                shell.set_run_label("idle");
                return result;
            }
            event = input.next(), if input_open => match event {
                Some(Ok(Event::Key(key))) if is_ctrl_c(&key) => {
                    crate::tui::terminal::request_coordinated_shutdown(RAW_CTRL_C_SIGNAL)?;
                    shell.set_run_label("shutting down…");
                    shell.render();
                    let _ = tokio::time::timeout(LIFECYCLE_SHUTDOWN_GRACE, &mut operation).await;
                    anyhow::bail!("Ctrl-C cancelled {operation_name}");
                }
                Some(Ok(Event::Key(key))) if keymap::is_close_key(&key) => {
                    shell.request_close();
                    shell.set_run_label("closing…");
                    shell.render();
                }
                Some(Ok(Event::Resize(columns, rows))) => {
                    shell.set_size(columns, rows);
                    shell.render();
                }
                Some(Ok(event)) => {
                    if shell.intercept_transcript_input(&event) {
                        continue;
                    }
                    // Submission is not admitted during lifecycle work, but
                    // ordinary editing must not lose the probe's saved input.
                    // The same holds for a native clipboard paste.
                    if paste_clipboard_text(shell, &event).await {
                        continue;
                    }
                    let _ = handle_cancellable_wait_input(shell, event);
                }
                Some(Err(error)) => {
                    // A blocking lifecycle worker cannot be aborted safely: it
                    // may own the only App and dropping its JoinHandle merely
                    // detaches it. Treat terminal input failure like loss of the
                    // controlling TTY, announce coordinated shutdown, and give
                    // the owned operation the same bounded settlement window as
                    // an explicit signal before returning the original error.
                    let shutdown =
                        crate::tui::terminal::request_coordinated_shutdown(RAW_CTRL_C_SIGNAL);
                    shell.set_run_label("shutting down…");
                    shell.render();
                    let _ = tokio::time::timeout(LIFECYCLE_SHUTDOWN_GRACE, &mut operation).await;
                    return match shutdown {
                        Ok(()) => Err(error.into()),
                        Err(shutdown_error) => Err(anyhow::anyhow!(
                            "terminal input failed: {error}; coordinated shutdown also failed: {shutdown_error}"
                        )),
                    };
                }
                None => input_open = false,
            },
        }
    }
}

pub(crate) async fn run_blocking_lifecycle<T, W, S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    label: &str,
    work: W,
) -> anyhow::Result<T>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
    T: Send + 'static,
    W: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let task = tokio::task::spawn_blocking(work);
    await_lifecycle(shell, input, label, async move {
        task.await
            .map_err(|error| anyhow::anyhow!("{label} worker failed: {error}"))?
    })
    .await
}

/// Diagnostic names for silent startup work. They never render: the startup
/// composer stays typeable without branding until readiness; a name appears
/// only in a worker-failure, cancellation, or shutdown diagnostic.
const STARTUP_MODELS_OPERATION: &str = "model discovery";
const STARTUP_APP_OPERATION: &str = "startup build";
const STARTUP_SESSION_OPERATION: &str = "session open";

/// Run one bounded startup phase with no status text and a typeable composer.
///
/// Startup shows nothing: no phase label, no progress notice. Keystrokes are
/// still accepted and buffered into the draft from the first frame, and the
/// phase durations stay attributable off-screen through
/// `OCTET_STARTUP_TRACE=1` (`crate::app::bootstrap::startup_phase`).
pub(crate) async fn run_blocking_startup_lifecycle<T, W, S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    operation: &'static str,
    work: W,
) -> anyhow::Result<T>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
    T: Send + 'static,
    W: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let task = tokio::task::spawn_blocking(work);
    await_lifecycle(shell, input, "", async move {
        task.await
            .map_err(|error| anyhow::anyhow!("{operation} worker failed: {error}"))?
            // Nothing rendered, so the operation name is the only attribution a
            // failed startup phase carries.
            .map_err(|error| anyhow::anyhow!("{operation} failed: {error}"))
    })
    .await
}

fn validate_provider(provider: Option<&str>) -> anyhow::Result<&str> {
    match provider.unwrap_or("codex") {
        "codex" | "openai-codex" | "openai" => Ok("codex"),
        "custom" | "openai-custom" => Ok("custom"),
        other => anyhow::bail!("unknown provider {other:?}; supported: codex, custom"),
    }
}

/// Run device-code login outside raw primary-screen rendering and return the
/// refreshed catalog. The caller decides whether it can install the catalog
/// into a live Agent or must ask the user to restart from a model-less shell.
async fn login_codex_catalog(
    shell: &mut InteractiveShell,
) -> anyhow::Result<Option<octet_ai::ModelCatalog>> {
    shell.set_run_label("signing in to ChatGPT…");
    shell.render();
    shell.suspend();
    let store = crate::auth::codex::CredentialStore::new(crate::auth::codex::default_path());
    let login_result = crate::auth::codex::login(&store, false).await;
    // Restoring the terminal is mandatory even when OAuth fails.
    shell.resume()?;
    shell.set_run_label("idle");

    if let Err(error) = login_result {
        shell.error(format!("ChatGPT login failed: {error:#}"));
        shell.render();
        return Ok(None);
    }

    let catalog = match crate::app::bootstrap::model_catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            shell.error(format!(
                "ChatGPT login succeeded, but reloading models failed: {error:#}"
            ));
            shell.render();
            return Ok(None);
        }
    };
    if !catalog
        .models()
        .any(|model| model.endpoint.0 == crate::auth::codex::ENDPOINT_ID)
    {
        shell.error("ChatGPT login completed, but no Codex models could be registered".into());
        shell.render();
        return Ok(None);
    }
    Ok(Some(catalog))
}

/// Run device-code login outside raw primary-screen rendering, then make the new
/// models available immediately without restarting the current Agent.
async fn login_codex(app: &mut App, shell: &mut InteractiveShell) -> anyhow::Result<()> {
    if let Some(catalog) = login_codex_catalog(shell).await? {
        app.catalog = catalog;
        shell.clear_error();
        shell.notice("signed in to ChatGPT; use /model to select a Codex model");
        shell.render();
    }
    Ok(())
}

/// Remove the octet-owned credential and catalog entries together. If the active
/// model is a Codex model, choose its replacement before deleting anything so
/// cancellation leaves both the session and credentials untouched.
async fn logout_codex(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<App> {
    let catalog = crate::app::bootstrap::model_catalog_without_codex()?;
    let replacement = if app.model.endpoint.id.0 == crate::auth::codex::ENDPOINT_ID {
        shell.notice("select a replacement model before signing out");
        let Some(model) = optional_model_picker(shell, input, &catalog).await? else {
            shell.notice("logout cancelled");
            return Ok(app);
        };
        Some(model)
    } else {
        None
    };

    // Transition while authentication and the old catalog are still intact.
    // If rebuilding the Agent fails, the user remains signed in rather than
    // being stranded on a model whose credential was already deleted.
    if let Some(model) = replacement {
        app = transition(app, shell, input, Reconfig::Model(model)).await?;
    }

    let store = crate::auth::codex::CredentialStore::new(crate::auth::codex::default_path());
    if let Err(error) = store.delete_async().await {
        shell.error(format!("ChatGPT logout failed: {error:#}"));
        return Ok(app);
    }
    app.catalog = catalog;
    shell.clear_error();
    shell.notice("signed out of ChatGPT");
    shell.render();
    Ok(app)
}

/// Save a default custom provider registry and reload the catalog.
fn login_custom(shell: &mut InteractiveShell) -> anyhow::Result<()> {
    use crate::auth::custom::{
        self, CustomAuthConfig, CustomCredential, CustomProvider, CustomRegistry,
    };
    let store = custom::CredentialStore::new(custom::default_path());
    let path = custom::default_path();

    if store.load_registry()?.is_some() {
        shell.notice(format!(
            "custom provider registry already configured at {}; use /logout custom first to replace it",
            path.display()
        ));
        return Ok(());
    }

    let provider = CustomProvider {
        label: "Local endpoint".into(),
        credential: CustomCredential {
            base_url: "http://localhost:1234/v1/".into(),
            api_key: String::new(),
            api_name: "local-model".into(),
            headers: Vec::new(),
            models: Vec::new(),
            auto_discover: true,
        },
        auth: Some(CustomAuthConfig::None),
        api_key_env: None,
        cache: None,
        startup_timeout_secs: None,
        lifecycle_feedback: false,
    };
    store.save_registry(&CustomRegistry::single("local", provider))?;
    shell.notice(format!(
        "custom provider registry template saved to {}\n\
         edit it with your provider details, then /reload to register the models",
        path.display()
    ));
    Ok(())
}

/// Remove custom endpoint credentials and rebuild the catalog.
async fn logout_custom(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<App> {
    use crate::auth::custom;

    let store = custom::CredentialStore::new(custom::default_path());
    if store.load_registry()?.is_none() {
        shell.notice("no custom provider registry configured");
        return Ok(app);
    }

    // Pick a replacement model if the active model belongs to any custom
    // provider in the unified registry.
    let needs_replacement = custom::is_endpoint_id(&app.model.endpoint.id.0);
    if needs_replacement {
        let catalog = crate::app::bootstrap::model_catalog()?;
        // Temporarily remove custom from consideration.
        shell.notice("select a replacement model before signing out");
        let Some(model) = optional_model_picker(shell, input, &catalog).await? else {
            shell.notice("logout cancelled");
            return Ok(app);
        };
        app = transition(app, shell, input, Reconfig::Model(model)).await?;
    }

    store.delete()?;
    // Rebuild catalog without the custom endpoint.
    let catalog = crate::app::bootstrap::model_catalog()?;
    // model_catalog() will no longer find the credential, so the custom model
    // won't be registered. But if we just deleted, it might still show. Force a
    // fresh rebuild by calling base_model_catalog + codex registration directly
    // is complex; for now just reload.
    app.catalog = catalog;
    shell.clear_error();
    shell.notice("custom endpoint removed");
    shell.render();
    Ok(app)
}

fn show_hotkeys(shell: &mut InteractiveShell) {
    let text = shell.hotkeys_text();
    shell.show_report_text("Hotkeys", "Resolved user keybindings", text);
}

fn copy_last_assistant(shell: &mut InteractiveShell) {
    if shell.copy_last_assistant().is_some() {
        shell.notice("Copied the last assistant message");
    } else {
        shell.error("There is no assistant message to copy yet".into());
    }
}

/// Refuse unsupported routes before either applying or queueing a tier change.
fn fast_route_available(shell: &mut InteractiveShell, model: &Model) -> bool {
    if commands::codex_fast_tier_endpoint(model) {
        true
    } else {
        shell.error(format!(
            "`/fast` is only available on Codex Responses routes; {} is not one, so nothing changed",
            commands::model_route_label(model),
        ));
        false
    }
}

/// Apply only while the App owns the idle Agent; never mutate an active Run.
fn apply_fast_command(app: &mut App, shell: &mut InteractiveShell, requested: Option<bool>) {
    if !fast_route_available(shell, &app.model) {
        return;
    }
    if let Some(enabled) = requested {
        if let Err(error) = app.set_fast_mode(enabled) {
            shell.error(format!("`/fast` not applied: {error}"));
            return;
        }
        update_status(shell, app);
    }
    shell.notice(format!(
        "{}; current live session only (restart or a different session resets it). Priority billing is not fully qualified across requests and reservations; enabling it marks session cost uncertain.",
        commands::fast_status_text(&app.model, app.agent.service_tier()),
    ));
}

fn active_fast_status(
    inspection: &ActiveRunInspection,
    queue: &VecDeque<PendingIdleAction>,
) -> String {
    let mut status =
        commands::fast_status_text(&inspection.model, inspection.service_tier).to_owned();
    // Queue order matters: a model/session change between toggles is a barrier,
    // so report all queued switches without pretending any has been applied.
    for action in queue {
        if let PendingIdleAction::Fast(enabled) = action {
            status.push_str(if *enabled {
                "\nFast mode on queued for the next idle boundary (route rechecked then)"
            } else {
                "\nFast mode off queued for the next idle boundary (route rechecked then)"
            });
        }
    }
    status
}

/// Apply a terminal keyboard-focus transition (`?1004` reporting).
///
/// A stale press, drag, or hover must not resume when focus returns, and a
/// returning window must repaint immediately. The shell owns pointer state;
/// settling a gesture whose pointer is outside the transcript clears the
/// pending press anchor and the drag flag and creates no selection. An
/// already-copied selection is a copy buffer, not transient interaction state,
/// so it survives.
fn apply_focus_transition(shell: &mut InteractiveShell, gained: bool) {
    if !gained {
        shell.begin_transcript_selection(u16::MAX, u16::MAX, false);
    }
    shell.render();
}

/// Read-only application facts a live run may inspect while its own
/// agent/session borrow is held by `Run`.
///
/// An active run owns `&mut Agent`, so the frontend cannot reach
/// `app.agent.session()`. Session-scoped reports therefore re-open the same
/// session file read-only by path — the same handle the live `/subagents`
/// drill-in already uses through the run's delegation binding. Nothing here
/// mutates the running session or the frozen agent.
#[derive(Clone)]
pub struct ActiveRunInspection {
    workspace: PathBuf,
    invocation_cwd: PathBuf,
    session_path: PathBuf,
    model: Model,
    catalog: octet_ai::ModelCatalog,
    sessions: crate::session_store::SessionStore,
    /// The launch sandbox policy, so a local shell escape applies the same
    /// process/shell gates and limits as the model `bash` tool.
    sandbox: SandboxPolicy,
    /// The launch effect policy, so a local shell escape takes the ordinary
    /// process approval decision instead of inventing a second one.
    effect_policy: octet_agent::EffectPolicy,
    subagents_available: bool,
    service_tier: Option<octet_ai::ServiceTier>,
    /// Durable goal state, or the typed reason the run cannot address it.
    ///
    /// Captured like every other fact here; the goal store and the driver hold
    /// no borrow of the application, so `/goal` mutates durable state mid-run
    /// instead of being deferred to the next idle boundary.
    goal: Result<GoalAccess, ActiveGoalError>,
    /// Effective `/settings` facts (defaults, theme, transport, images).
    settings: commands::SettingsSurface,
    /// The launch ordered cycling scope, rendered by `/scoped-models` mid-run.
    model_scope: Option<Vec<crate::cli::parity::ScopedModel>>,
    /// Whether the catalog holds only the routes this launch proved it needs.
    ///
    /// A narrowed picker waits for idle ownership of the app, then opens from
    /// the current catalog while deferred fleet discovery runs independently.
    catalog_is_narrowed: bool,
}

impl ActiveRunInspection {
    /// Whether this run's catalog is missing providers the launch deferred.
    fn is_narrowed(&self) -> bool {
        self.catalog_is_narrowed
    }

    /// Snapshot the application facts a run cannot borrow without cancelling it.
    pub fn capture(app: &App) -> Self {
        Self {
            workspace: app.config.workspace.clone(),
            invocation_cwd: app.config.invocation_cwd.clone(),
            session_path: app.agent.session().path().to_path_buf(),
            model: app.model.clone(),
            catalog: app.catalog.clone(),
            sandbox: app.config.sandbox.clone(),
            effect_policy: app.config.effect_policy,
            catalog_is_narrowed: !app.readiness.is_fleet(),
            sessions: app.sessions.clone(),
            subagents_available: app.subagents_available(),
            service_tier: app.agent.service_tier(),
            goal: GoalAccess::from_app(app),
            settings: commands::SettingsSurface::capture(app),
            model_scope: app.model_scope.clone(),
        }
    }

    fn session_id(&self) -> Option<&str> {
        self.session_path.file_stem().and_then(|stem| stem.to_str())
    }

    /// Re-read the live session without repairing, truncating, or appending.
    fn read_only_session(&self) -> anyhow::Result<Session> {
        Ok(Session::open_read_only(&self.session_path)?)
    }

    /// The durable goal of this run, or the typed reason it is unaddressable.
    fn goal_access(&self) -> Result<&GoalAccess, &ActiveGoalError> {
        self.goal.as_ref()
    }
}

/// Immediate `/context` body for an active run.
///
/// The styled idle report (`tui::context::ContextReport`) can only be built
/// from `&App`, which the run holds. The run's own live context snapshot is the
/// authoritative alternative for the same quantities.
fn active_context_text(snapshot: &octet_agent::ContextSnapshot, model: &Model) -> String {
    let breakdown = &snapshot.context;
    let mut text = format!(
        "Model: {}\nContext window: {} tokens\nEstimated next request: {} tokens\n\n",
        model
            .spec
            .display_name
            .as_deref()
            .unwrap_or(&model.spec.api_name),
        breakdown.context_limit,
        breakdown.total_tokens,
    );
    for (label, tokens) in [
        ("Runtime framing and tools", breakdown.system_tokens),
        ("System instructions", breakdown.instruction_tokens),
        ("Conversation", breakdown.conversation_tokens),
        ("Tool results", breakdown.tool_result_tokens),
        ("Attachments", breakdown.attachment_tokens),
        ("Compaction summaries", breakdown.compaction_summary_tokens),
        ("Other", breakdown.other_tokens),
    ] {
        text.push_str(&format!("{label}: {tokens} tokens\n"));
    }
    if let Some(measured) = breakdown.provider_tokens {
        text.push_str(&format!("Provider measurement: {measured} tokens\n"));
    }
    text
}

/// Install a panel, but leave input and the caller-driven Run with the main loop.
fn open_active_thinking(shell: &mut InteractiveShell, inspection: &ActiveRunInspection) {
    let levels = supported_levels_with_subagents(&inspection.model, inspection.subagents_available);
    let mut items: Vec<String> = levels.iter().map(|level| level.label().into()).collect();
    if let Some(surface) = codex_context_surface(&inspection.model) {
        items.push(pickers::codex_context_menu_row(&surface));
    }
    let (_, current) = shell.selected_identity();
    let selected = levels
        .iter()
        .position(|level| level.label() == current.trim_end_matches(" (queued)"))
        .unwrap_or(0);
    if let Some(item) = items.get_mut(selected) {
        item.push_str(" (current)");
    }
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::with_purpose(
            "Select thinking level",
            "Choose effort for the next supported response boundary and startup default",
        ),
        descriptions: vec![None; items.len()],
        items,
        selected,
        filter: String::new(),
        action: PanelAction::SelectThinking(levels),
    });
}

fn open_active_codex_context(
    shell: &mut InteractiveShell,
    surface: &commands::CodexContextSurface,
) {
    let mut items = vec![format!(
        "Keep the deliberate {} window",
        surface.effective_window()
    )];
    let mut descriptions = vec![Some("no change".into())];
    if let Some(target) = surface.raise_target() {
        items.push(format!(
            "Acknowledge and raise to {target} tokens (double-priced; websocket risk)"
        ));
        descriptions.push(Some(
            crate::codex_context::CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING.into(),
        ));
    }
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::with_purpose(
            "Codex context window",
            surface.summary_lines().join(" · "),
        ),
        items,
        descriptions,
        selected: 0,
        filter: String::new(),
        action: PanelAction::ProviderSetup(vec!["codex-context".into()]),
    });
}

/// Handle a slash command while the model is running.
///
/// Anything observable through [`ActiveRunInspection`] or the run's own context
/// snapshot renders immediately through the same producers the idle dispatcher
/// uses. Commands that must *transition* the application (session changes,
/// model selection, extension reload) still queue a [`PendingIdleAction`], but
/// they never leave the user with only a "next idle boundary" notice.
#[allow(clippy::too_many_arguments)]
async fn handle_active_command<S, F>(
    shell: &mut InteractiveShell,
    command: Command,
    inspection: &ActiveRunInspection,
    extensions: &mut crate::extensions::ExecutableExtensions,
    context: &octet_agent::ContextSnapshot,
    goal_deadline: &mut Option<Instant>,
    open_delegated: F,
    input: &mut S,
    queue: &mut VecDeque<PendingIdleAction>,
    quit_requested: &mut bool,
) -> anyhow::Result<()>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
    F: Fn(&str, &str) -> Result<Option<Session>, AgentError>,
{
    match command {
        Command::Status => {
            let mut status = shell.status_detail();
            if inspection
                .model
                .responses_features()
                .reasoning_effort_updates
            {
                let selected = shell.selected_identity().1;
                status = status
                    .lines()
                    .map(|line| {
                        if line.starts_with("Reasoning      ") {
                            format!("Reasoning      {selected}")
                        } else {
                            line.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                status.push_str("\nThinking is host-selected, not provider acknowledgement.");
            }
            if !queue.is_empty() {
                status.push_str(&format!("\nQueued idle actions: {}", queue.len()));
                if queue
                    .iter()
                    .any(|action| matches!(action, PendingIdleAction::Fast(_)))
                {
                    status.push_str(&format!("\n{}", active_fast_status(inspection, queue)));
                }
            }
            shell.show_status_text_with_telemetry(status);
        }
        Command::Changelog => shell.show_changelog(),
        Command::Hotkeys => show_hotkeys(shell),
        Command::Copy => copy_last_assistant(shell),
        Command::Session => match inspection.read_only_session() {
            Ok(session) => shell.show_report_text(
                "Session",
                "Durable session facts",
                commands::session_text(&session),
            ),
            Err(error) => shell.error(format!("session report unavailable: {error}")),
        },
        Command::Help(topic) => shell.show_report_text(
            "Help",
            "Browse commands and keyboard shortcuts",
            commands::help_text(&inspection.workspace, topic.as_deref()),
        ),
        Command::Cost => match inspection.read_only_session() {
            Ok(session) => shell.show_report_text(
                "Cost",
                "Review session token usage and estimated cost",
                commands::cost_text(&session, &inspection.model),
            ),
            Err(error) => shell.error(format!("cost report unavailable: {error}")),
        },
        Command::Cache => match inspection.read_only_session() {
            Ok(session) => shell.show_report_text(
                "Cache",
                "Review session cache accounting",
                commands::cache_text(&session),
            ),
            Err(error) => shell.error(format!("cache report unavailable: {error}")),
        },
        Command::Context => shell.show_report_text(
            "Context",
            "Review the estimated request context before the next turn",
            active_context_text(context, &inspection.model),
        ),
        Command::Update => {
            // The check is a bounded local request. Run events buffer until it
            // settles, exactly like the live `/subagents` panel, and the next
            // select iteration owns any shutdown the signal arm requests.
            shell.show_overlay_text("Checking for updates…".into());
            shell.render();
            tokio::select! {
                biased;
                _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                    shell.request_close();
                }
                result = crate::update::check() => match result {
                    Ok(status) => shell.show_overlay_text(match status {
                        crate::update::UpdateStatus::Available { .. } => {
                            format!("{status}\n\nRun `octet update` to install.")
                        }
                        status => status.to_string(),
                    }),
                    Err(error) => shell.error(format!("update check failed: {error}")),
                },
            }
        }
        Command::Verbose(value) => {
            let enabled = value.unwrap_or(!shell.verbose_tools());
            shell.set_verbose_tools(enabled);
            shell.notice(format!(
                "verbose transcript {}",
                if enabled { "enabled" } else { "disabled" }
            ));
        }
        Command::Name(name) => match inspection.session_id() {
            Some(id) => match name {
                Some(name) => match inspection.sessions.rename(id, &name) {
                    Ok(metadata) => shell.notice(format!(
                        "session named {}",
                        metadata.name.as_deref().unwrap_or("(unnamed)")
                    )),
                    Err(error) => shell.error(error.to_string()),
                },
                None => match inspection.sessions.load_metadata(id) {
                    Ok(metadata) => shell.notice(format!(
                        "session name: {}",
                        metadata
                            .name
                            .as_deref()
                            .unwrap_or("(derived from first prompt)")
                    )),
                    Err(error) => shell.error(error.to_string()),
                },
            },
            None => shell.error("current session has no valid id".into()),
        },
        Command::Export(output) => match inspection.session_id() {
            Some(id) => match crate::session_commands::export_portable(
                &inspection.sessions,
                id,
                output.map(PathBuf::from),
                &inspection.invocation_cwd,
                false,
                false,
            ) {
                Ok(report) => shell.show_overlay_text(format!(
                    "Exported {}\nRedacted {} potentially sensitive values{}",
                    report.destination.display(),
                    report.redaction_count,
                    if report.ignored_torn_tail {
                        "\nIgnored an interrupted final append; use `octet sessions repair`."
                    } else {
                        ""
                    }
                )),
                Err(error) => shell.error(error.to_string()),
            },
            None => shell.error("current session has no valid id".into()),
        },
        Command::Extensions(commands::ExtensionsSubcommand::Status) => {
            shell.show_overlay_text(extensions.inspect_text());
        }
        Command::Extensions(commands::ExtensionsSubcommand::Inspect { reference }) => {
            let principal = extensions.presentation_session_reference_principal(&reference);
            match principal {
                Some(principal) => {
                    match open_delegated(&principal, &reference) {
                        Ok(Some(session)) => {
                            let theme = shell.theme();
                            let width = shell.read_only_document_width();
                            let verbose_tools = shell.verbose_tools();
                            match delegated_session_text(&session, &theme, width, verbose_tools) {
                                Ok(text) => shell.show_styled_overlay_text(
                                    delegated_session_overlay_text(&text, &theme),
                                ),
                                Err(error) => shell
                                    .error(format!("failed to inspect delegated session: {error}")),
                            }
                        }
                        Ok(None) => shell.error(
                            "delegated session reference is unavailable for this parent".into(),
                        ),
                        Err(error) => {
                            shell.error(format!("failed to inspect delegated session: {error}"))
                        }
                    }
                }
                None => shell.error(
                    "delegated session reference is unavailable, stale, or owned by another extension"
                        .into(),
                ),
            }
        }
        Command::Extensions(sub) => {
            // The management menu, reload, and actions own the application and
            // its executable extensions, so they still run through the idle
            // dispatcher — but their current state renders now.
            shell.show_overlay_text(extensions.inspect_text());
            push_pending_action(queue, PendingIdleAction::Extensions(sub));
        }
        Command::Fast(requested) => {
            if fast_route_available(shell, &inspection.model) {
                if let Some(enabled) = requested {
                    push_pending_action(queue, PendingIdleAction::Fast(enabled));
                }
                shell.notice(active_fast_status(inspection, queue));
            }
        }
        Command::Goal(goal) => match inspection.goal_access() {
            // Applied now, against the same durable store and driver the idle
            // dispatcher uses. The run in progress is untouched: it is not
            // restarted, and the armed deadline is recomputed from the settled
            // turn exactly as it is for an idle `/goal`.
            Ok(access) => {
                if let Err(error) = apply_goal_command(access, shell, goal, goal_deadline) {
                    shell.error(format!("/goal failed: {error:#}"));
                }
            }
            // Fail closed. The typed reason is rendered; the command is never
            // queued and never a silent no-op.
            Err(error) => shell.error(format!("/goal failed: {error}")),
        },
        Command::Debug => {
            let rendered = shell.dump_rendered_frame().await;
            match inspection.read_only_session() {
                Ok(session) => {
                    write_debug_report(shell, &session, None, rendered).await;
                }
                Err(error) => shell.error(format!("/debug unavailable: {error}")),
            }
        }
        Command::Model(None) => {
            if inspection.is_narrowed() {
                push_pending_action(queue, PendingIdleAction::PickModel);
                shell.notice("model picker opens at the next idle boundary");
                return Ok(());
            }
            let mut presentation = pickers::model_picker_presentation(&inspection.catalog);
            let (current, _) = shell.selected_identity();
            if let Some(index) = presentation.ids.iter().position(|id| id.0 == current) {
                presentation.labels[index].push_str(" (current)");
            }
            shell.open_panel(Panel::SelectList {
                surface: OrdinarySurfaceMetadata::with_purpose(
                    "Select model",
                    "Choose the model for subsequent prompts and the startup default",
                ),
                items: presentation.labels,
                descriptions: presentation.descriptions,
                selected: 0,
                filter: String::new(),
                action: PanelAction::SelectGroupedModel {
                    models: presentation.ids,
                    providers: presentation.providers,
                },
            });
        }
        Command::Thinking(None) => open_active_thinking(shell, inspection),
        Command::Settings(sub) => {
            match sub {
                commands::SettingsCommand::Show => shell.show_report_text(
                    "Settings",
                    "Effective display and default preferences",
                    commands::settings_text(&inspection.settings),
                ),
                commands::SettingsCommand::Transport => shell.notice(format!(
                    "transport {} (declared by the {} route; not a user preference)",
                    inspection.settings.transport, inspection.settings.endpoint,
                )),
                commands::SettingsCommand::Padding => shell.notice(
                    "editor padding is compiled into the active theme layout; octet persists no padding override"
                        .to_owned(),
                ),
                mutation => {
                    // Theme install, config writes and image rendering own the
                    // application, so the mutation runs through the idle
                    // dispatcher verbatim at the next boundary; the report
                    // above never leaves the user with only a promise.
                    push_pending_action(queue, PendingIdleAction::Settings(mutation));
                    shell.notice("settings change queued for the next idle boundary");
                }
            }
        }
        Command::ScopedModels(sub) => {
            let mut available = inspection
                .catalog
                .models()
                .map(|spec| (spec.id.0.clone(), spec.endpoint.0.clone()))
                .collect::<Vec<_>>();
            available.sort();
            match sub {
                commands::ScopedModelsCommand::Show => shell.show_report_text(
                    "Scoped models",
                    "Ordered model cycling scope",
                    commands::scoped_models_text(inspection.model_scope.as_deref(), &available),
                ),
                // Scope mutation owns the App's catalog-backed scope and the
                // shell's cycling list, so it queues for the idle boundary but
                // reports the decision immediately.
                mutation => {
                    push_pending_action(queue, PendingIdleAction::ScopedModels(mutation));
                    shell.notice("model scope change queued for the next idle boundary");
                }
            }
        }
        Command::Bash(escape) => {
            let prefix = if escape.excluded { "!!" } else { "!" };
            if !inspection.sandbox.process_execution_allowed() {
                shell.error("shell commands are disabled by --no-process/--no-shell".to_owned());
            } else if escape.command.trim().is_empty() {
                shell.notice(format!("usage: {prefix}<shell command>"));
            } else if !matches!(
                approve_shell_escape(shell, input, inspection.effect_policy, &escape.command)
                    .await?,
                ShellEscapeApproval::Approved
            ) {
                shell.error(format!("{prefix} command was not approved"));
            } else {
                extensions.notify_user_bash_all(&escape.command);
                shell.on_local_command_submitted(&format!("{prefix}{}", escape.command));
                let outcome = run_local_shell(
                    shell,
                    input,
                    &inspection.workspace,
                    &inspection.sandbox,
                    &escape.command,
                )
                .await?;
                if outcome.shutting_down {
                    // The drive loop owns the ordered shutdown (abort, drain,
                    // process-group cleanup); the escape only requests it.
                    shell.request_close();
                } else if let Some(refusal) = outcome.refusal {
                    shell.error(refusal);
                } else {
                    // The run owns the session writer, so the durable record is
                    // appended by the idle owner without breaking tool-result
                    // ordering; the context decision is fixed now.
                    push_pending_action(
                        queue,
                        PendingIdleAction::RecordShellEscape(commands::ShellEscapeRecord::new(
                            escape.command,
                            outcome.output,
                            outcome.exit_code,
                            escape.excluded,
                        )),
                    );
                    shell.notice(if escape.excluded {
                        "shell result recorded at the next idle boundary; excluded from model context"
                            .to_owned()
                    } else {
                        "shell result recorded at the next idle boundary; added to model context"
                            .to_owned()
                    });
                }
            }
        }
        Command::Exit => *quit_requested = true,
        Command::Unknown(text) => shell.error(format!("unknown command: {text}")),
        command => match queue_command(command, queue) {
            Ok(()) => shell.notice("command queued for the next idle boundary"),
            Err(error) => shell.error(error.to_string()),
        },
    }
    shell.render();
    Ok(())
}

/// Route one active-run submission through the shell-escape dispatcher when it
/// is a `!`/`!!` command.
///
/// Returns `false` for an ordinary prompt, which then takes the existing queue
/// or steer path unchanged. An escape is consumed here: it keeps the ordinary
/// process gate, approval, bounded capture, and context decision instead of
/// silently becoming model input.
#[allow(clippy::too_many_arguments)]
async fn dispatch_active_shell_escape<'r, S>(
    composed: &ComposedInput,
    run: &mut Run<'r>,
    shell: &mut InteractiveShell,
    input: &mut S,
    executable_extensions: &mut crate::extensions::ExecutableExtensions,
    inspection: &ActiveRunInspection,
    goal_deadline: &mut Option<Instant>,
    queue: &mut VecDeque<PendingIdleAction>,
    quit_requested: &mut bool,
) -> anyhow::Result<bool>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let Some(escape) = commands::BashEscape::parse(&composed.display_text) else {
        return Ok(false);
    };
    let context = run.context_snapshot();
    handle_active_command(
        shell,
        Command::Bash(escape),
        inspection,
        executable_extensions,
        &context,
        goal_deadline,
        |principal: &str, reference: &str| {
            run.open_delegated_session_reference(principal, reference)
        },
        input,
        queue,
        quit_requested,
    )
    .await?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn request_active_close(
    control: &RunControl,
    shell: &mut InteractiveShell,
    run_id: RunId,
    input_open: &mut bool,
    aborting: &mut bool,
    intents: &mut VecDeque<ControlIntent>,
    in_flight: &mut Option<ControlFuture>,
    quit_requested: &mut bool,
) {
    shell.request_close();
    *input_open = false;
    control.abort();
    *aborting = true;
    intents.clear();
    *in_flight = None;
    *quit_requested = true;
    shell.set_run_preparing(run_id, "cancelling");
    shell.render();
}

fn confirmation_action(tool_name: Option<&str>) -> &str {
    match tool_name {
        Some(name) if matches!(name, "bash" | "edit" | "write") => name,
        Some(_) => "extension",
        None => "tool",
    }
}

fn confirmation_notice(tool_name: Option<&str>, confirmed: bool) -> String {
    format!(
        "{} action {}",
        confirmation_action(tool_name),
        if confirmed { "approved" } else { "denied" }
    )
}

/// The fan-out seam the run driver uses to publish assistant message
/// boundaries without owning any batching decision.
trait MessageLifecycleSink {
    fn message_started(&mut self, message_id: &str);
    fn message_delta(&mut self, delta: &str);
    fn message_settled(&mut self, message_id: &str);
}

impl MessageLifecycleSink for crate::extensions::ExecutableExtensions {
    fn message_started(&mut self, message_id: &str) {
        self.notify_message_started_all(message_id);
    }

    fn message_delta(&mut self, delta: &str) {
        self.push_message_delta_all(delta);
    }

    fn message_settled(&mut self, message_id: &str) {
        self.notify_message_settled_all(message_id);
    }
}

/// Producer-side state for one assistant message boundary.
///
/// One boundary covers one logical model turn: the first streamed text
/// increment opens it, a provider retry keeps it open (the retry is the same
/// logical turn), and the turn's terminal event settles it. Deltas are handed
/// to the host verbatim; the host coalescer owns every batching and flush
/// decision, so this producer never opens a second batching layer and never
/// causes one host notification per streamed increment.
#[derive(Default)]
struct AssistantMessageLifecycle {
    sequence: u64,
    message_id: Option<String>,
}

impl AssistantMessageLifecycle {
    /// Fold one run event into the assistant message boundary.
    fn observe(&mut self, sink: &mut impl MessageLifecycleSink, event: &AgentEvent) {
        match event {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text,
            } => self.delta(sink, text),
            // The turn commit and any terminal run outcome close an open
            // boundary exactly once; settling an already-settled boundary is a
            // no-op, and a turn that never streamed text never opens one.
            AgentEvent::TurnFinished { .. } | AgentEvent::RunFinished { .. } => {
                self.settle(sink);
            }
            _ => {}
        }
    }

    /// Open the boundary on the first non-empty text increment, then forward
    /// the increment unchanged.
    fn delta(&mut self, sink: &mut impl MessageLifecycleSink, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.message_id.is_none() {
            self.sequence += 1;
            let message_id = format!("assistant-{}", self.sequence);
            sink.message_started(&message_id);
            self.message_id = Some(message_id);
        }
        sink.message_delta(text);
    }

    /// Close the open boundary, if any.
    fn settle(&mut self, sink: &mut impl MessageLifecycleSink) {
        if let Some(message_id) = self.message_id.take() {
            sink.message_settled(&message_id);
        }
    }
}

/// Pending consent contains only bounded requests, never buffered Run events.
/// Dropping an unanswered interaction is fail-closed, including error paths.
struct ActiveToolInteraction {
    id: ToolCallId,
    tool: Option<String>,
    request: ActiveToolRequest,
}

enum ActiveToolRequest {
    Confirmation(octet_agent::tool::ToolConfirmation),
    Input(
        octet_agent::tool::ToolInputRequest,
        pickers::SecretInputBuffer,
    ),
}

impl Drop for ActiveToolInteraction {
    fn drop(&mut self) {
        match &self.request {
            ActiveToolRequest::Confirmation(request) => request.respond(false),
            ActiveToolRequest::Input(request, _) => request.cancel(),
        }
    }
}

impl ActiveToolInteraction {
    fn request_bytes(&self) -> usize {
        match &self.request {
            ActiveToolRequest::Confirmation(request) => request
                .prompt
                .len()
                .saturating_add(request.detail.as_ref().map_or(0, String::len)),
            ActiveToolRequest::Input(request, _) => request.prompt.len(),
        }
    }

    fn from_event(
        event: &AgentEvent,
        tools: &std::collections::HashMap<ToolCallId, (String, serde_json::Value)>,
    ) -> Option<Self> {
        let AgentEvent::ToolProgress { id, progress, .. } = event else {
            return None;
        };
        let request = match progress {
            ToolProgress::Confirmation(request) => ActiveToolRequest::Confirmation(request.clone()),
            ToolProgress::Input(request) => {
                ActiveToolRequest::Input(request.clone(), Default::default())
            }
            _ => return None,
        };
        Some(Self {
            id: id.clone(),
            tool: tools.get(id).map(|(name, _)| name.clone()),
            request,
        })
    }

    fn open(&self, shell: &mut InteractiveShell) {
        match &self.request {
            ActiveToolRequest::Confirmation(request) => {
                let items = if request.default {
                    vec!["Approve".into(), "Deny".into()]
                } else {
                    vec!["Deny".into(), "Approve".into()]
                };
                let title = if request.destructive {
                    format!("Action requires approval · {}", request.prompt)
                } else {
                    request.prompt.clone()
                };
                shell.open_panel(Panel::SelectList {
                    surface: OrdinarySurfaceMetadata::new(title),
                    items,
                    descriptions: vec![request.detail.clone(), request.detail.clone()],
                    selected: 0,
                    filter: String::new(),
                    action: PanelAction::Confirmation,
                });
            }
            ActiveToolRequest::Input(request, _) => {
                shell.set_tool_input_prompt(Some(request.prompt.clone()))
            }
        }
    }

    fn input(&mut self, shell: &mut InteractiveShell, event: &Event) -> bool {
        match &mut self.request {
            ActiveToolRequest::Confirmation(request) => {
                let cancelled = matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL));
                let result = if cancelled {
                    shell.close_panel();
                    Some(PanelResult::Cancel)
                } else {
                    shell.panel_input(event).map(|(result, _)| result)
                };
                let Some(result) = result else {
                    return false;
                };
                let confirmed = matches!(result, PanelResult::Confirm(index) if (index == 0) == request.default);
                request.respond(confirmed);
                let notice = confirmation_notice(self.tool.as_deref(), confirmed);
                if confirmed {
                    shell.notice_success(notice);
                } else {
                    shell.notice_error(notice);
                }
                true
            }
            ActiveToolRequest::Input(request, secret) => {
                match event {
                    Event::Key(key)
                        if key.kind == KeyEventKind::Press && key.code == KeyCode::Enter =>
                    {
                        request.respond(secret.take());
                        shell.set_tool_input_prompt(None);
                        return true;
                    }
                    Event::Key(key)
                        if key.kind == KeyEventKind::Press
                            && (key.code == KeyCode::Esc
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL))) =>
                    {
                        request.cancel();
                        shell.set_tool_input_prompt(None);
                        shell.notice("interactive command input cancelled");
                        return true;
                    }
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        match key.code {
                            KeyCode::Backspace => secret.backspace(),
                            KeyCode::Char(character)
                                if !key.modifiers.intersects(
                                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                                ) =>
                            {
                                secret.push(character)
                            }
                            _ => {}
                        }
                    }
                    Event::Paste(paste) => secret.extend_paste(paste),
                    Event::Resize(columns, rows) => shell.set_size(*columns, *rows),
                    _ => {}
                }
                false
            }
        }
    }
}

fn active_subagent_snapshot(
    extensions: &crate::extensions::ExecutableExtensions,
) -> Option<SubagentPickerSnapshot> {
    subagent_view_entries(extensions)
        .map(|(title, entries)| subagent_picker_snapshot(&title, &entries, Vec::new()))
}

fn active_subagent_panel(snapshot: &SubagentPickerSnapshot) -> SubagentPanel {
    SubagentPanel {
        node_ids: snapshot.node_ids.clone(),
        groups: snapshot.groups.clone(),
        collapsed: true,
        revealed_node: None,
        state_filter: None,
    }
}

fn open_active_subagent_list(
    shell: &mut InteractiveShell,
    extensions: &crate::extensions::ExecutableExtensions,
) -> bool {
    let Some(snapshot) =
        active_subagent_snapshot(extensions).filter(|snapshot| !snapshot.items.is_empty())
    else {
        shell.notice("No subagents for this session.");
        return false;
    };
    let action = PanelAction::SelectSubagent(active_subagent_panel(&snapshot));
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new(snapshot.title),
        items: snapshot.items,
        descriptions: snapshot.descriptions,
        selected: 0,
        filter: String::new(),
        action,
    });
    true
}

fn refresh_active_subagent_list(
    shell: &mut InteractiveShell,
    extensions: &crate::extensions::ExecutableExtensions,
) {
    if let Some(snapshot) = active_subagent_snapshot(extensions) {
        let panel = active_subagent_panel(&snapshot);
        shell.refresh_subagent_panel(snapshot.title, snapshot.items, snapshot.descriptions, panel);
    }
}

fn open_active_subagent_document(
    shell: &mut InteractiveShell,
    extensions: &crate::extensions::ExecutableExtensions,
    run: &Run<'_>,
    id: &str,
    opening: bool,
) {
    // Re-resolve both the node and opaque principal/reference for every read;
    // a stale panel must never confer another generation's session authority.
    let entry = subagent_view_entries(extensions)
        .and_then(|(_, entries)| entries.into_iter().find(|entry| entry.node_id == id));
    let Some(entry) = entry else {
        if opening {
            shell.notice("subagent state changed; select it again to view");
            open_active_subagent_list(shell, extensions);
        } else {
            shell.update_read_only_document("Subagent is no longer available to this owner; return to the list.".into());
        }
        return;
    };
    let text = match entry.session_reference.as_deref().and_then(|reference| {
        extensions
            .presentation_session_reference_principal(reference)
            .map(|principal| (principal, reference))
    }) {
        Some((principal, reference)) => {
            match run.open_delegated_session_reference(&principal, reference) {
                Ok(Some(session)) => delegated_session_text(
                    &session,
                    &shell.theme(),
                    shell.read_only_document_width(),
                    shell.verbose_tools(),
                )
                .unwrap_or_else(|error| crate::tui::view::sanitize_for_terminal(&format!("Failed to render delegated transcript: {error}"))),
                Ok(None) => crate::tui::view::sanitize_for_terminal(&entry.fallback_detail),
                Err(error) => crate::tui::view::sanitize_for_terminal(&format!(
                    "Failed to open delegated transcript: {error}"
                )),
            }
        }
        None => crate::tui::view::sanitize_for_terminal(&entry.fallback_detail),
    };
    if opening {
        shell.open_panel(Panel::ReadOnlyDocument {
            title: format!("{} · read-only transcript", entry.label),
            text: text.into(),
            styled: true,
            scroll_from_bottom: 0,
        });
    } else {
        shell.update_read_only_document_styled(text);
    }
}

/// Drive one active frozen-Agent run. Control sends are queued locally, and
/// their bounded sends are polled alongside input and the run stream. Channel
/// admission is not a delivery acknowledgement; only SteeringDelivered is.
#[allow(clippy::too_many_arguments)]
pub async fn drive_active_run<S>(
    run: &mut Run<'_>,
    control: &RunControl,
    shell: &mut InteractiveShell,
    input: &mut S,
    scroll_tick: &mut Interval,
    pending_actions: &mut VecDeque<PendingIdleAction>,
    quit_requested: &mut bool,
    max_cost_microdollars: Option<u64>,
    cost_warning_microdollars: Option<u64>,
    executable_extensions: &mut crate::extensions::ExecutableExtensions,
    made_tool_call: &mut bool,
    inspection: &ActiveRunInspection,
    goal_deadline: &mut Option<Instant>,
) -> anyhow::Result<HostRunOutcome>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let run_id = shell
        .current_run_id()
        .ok_or_else(|| anyhow::anyhow!("cannot drive a run without presentation state"))?;
    let mut intents = VecDeque::<ControlIntent>::new();
    let mut in_flight: Option<ControlFuture> = None;
    // Loop-owned, not spawned: dropping this future also drops the helper's
    // kill-on-drop child. No clipboard task can outlive the run.
    let mut clipboard: Option<Pin<Box<dyn Future<Output = Option<String>>>>> = None;
    let mut clipboard_gesture = None;
    let mut clipboard_revision = 0;
    let mut clipboard_fallback = None;
    let mut pending_reasoning: Option<ReasoningConfig> = None;
    // One pending latest choice and one bounded channel admission; never spawn
    // a control sender that could outlive the caller-driven run.
    type ReasoningSend = Pin<Box<dyn Future<Output = (ReasoningConfig, Result<(), AgentError>)>>>;
    let mut reasoning_send: Option<ReasoningSend> = None;
    let mut aborting = false;
    let mut dispatch_queued = false;
    shell.settle_queued_follow_ups(false);
    let mut input_open = true;
    let mut scroll_dirty = false;
    let mut last_run_cost = 0u64;
    let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
    extension_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut tool_calls =
        std::collections::HashMap::<ToolCallId, (String, serde_json::Value)>::new();
    let mut assistant_message = AssistantMessageLifecycle::default();
    let mut interactions = VecDeque::<ActiveToolInteraction>::new();
    let mut subagents_open = false;
    let mut subagent_document = None::<String>;
    let mut subagent_refresh: Option<Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>> =
        None;
    let mut subagent_refresh_error = None::<String>;
    let mut update_check: Option<Pin<Box<dyn Future<Output = anyhow::Result<crate::update::UpdateStatus>>>>> = None;
    let mut update_report_open = false;
    let mut modal_refresh = tokio::time::interval(Duration::from_secs(1));
    modal_refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        if aborting || shell.close_requested() {
            clipboard = None;
            clipboard_gesture = None;
            clipboard_fallback = None;
            if !interactions.is_empty() {
                interactions.clear();
                shell.close_panel();
                shell.set_tool_input_prompt(None);
            }
        }
        if shell.close_requested() && !*quit_requested {
            request_active_close(
                control,
                shell,
                run_id,
                &mut input_open,
                &mut aborting,
                &mut intents,
                &mut in_flight,
                quit_requested,
            );
        }
        if !aborting && in_flight.is_none() {
            if let Some(intent) = intents.pop_front() {
                let control = control.clone();
                in_flight = Some(Box::pin(async move {
                    match intent {
                        ControlIntent::SteerPrepared(prepared) => {
                            control.steer_retractable(prepared).await
                        }
                        ControlIntent::FinishNow(text) => control.finish_now(text).await,
                    }
                }));
            }
        }

        if !aborting && reasoning_send.is_none() {
            if let Some(reasoning) = pending_reasoning.take() {
                let control = control.clone();
                reasoning_send = Some(Box::pin(async move {
                    let result = control.set_reasoning(reasoning.clone()).await;
                    (reasoning, result)
                }));
            }
        }
        if aborting {
            pending_reasoning = None;
            reasoning_send = None;
        }

        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                drop(clipboard.take());
                control.abort();
                *quit_requested = true;
                shell.restore_queued_steering();
                shell.set_run_preparing(run_id, "shutting down");
                shell.render();
                octet_agent::extension_process::terminate_bash_process_groups(
                    Duration::from_millis(400),
                )
                .await;
                return Ok(HostRunOutcome::shutdown());
            }
            result = futures_util::future::OptionFuture::from(reasoning_send.as_mut().map(|f| f.as_mut())), if reasoning_send.is_some() => {
                reasoning_send = None;
                if let Some((reasoning, result)) = result {
                    match result {
                        Ok(()) => {
                            let label = reasoning_label(&reasoning);
                            shell.set_identity(&inspection.model.endpoint.id.0, &inspection.model.spec.id.0, &format!("{label} (queued)"));
                            shell.notice(format!("thinking {label} queued for the next response boundary; not provider acknowledgement"));
                            if let Err(error) = persist_configuration(Some(executable_extensions), || crate::cli::persist_reasoning(&label)).await {
                                shell.error(format!("failed to save thinking preference: {error}"));
                            }
                        }
                        Err(error) => shell.error(format!("thinking unchanged: {error}")),
                    }
                }
                shell.render();
            }
            result = futures_util::future::OptionFuture::from(clipboard.as_mut().map(|f| f.as_mut())), if clipboard.is_some() => {
                clipboard = None;
                // A background extension can replace the composer without an
                // InputAction. Fence completion as well as explicit handoffs.
                clipboard_fallback = settle_active_clipboard_read(
                    shell, clipboard_revision, result.flatten(), clipboard_gesture.take(),
                );
            }
            result = futures_util::future::OptionFuture::from(in_flight.as_mut().map(|f| f.as_mut())), if in_flight.is_some() => {
                // A run may have ended before a pending control was delivered.
                // That error is harmless; no detached send survives this loop.
                let _ = result;
                in_flight = None;
            }
            _ = scroll_tick.tick(), if scroll_dirty => {
                shell.render();
                scroll_dirty = false;
            }
            result = futures_util::future::OptionFuture::from(update_check.as_mut().map(|future| future.as_mut())), if update_check.is_some() => {
                update_check = None;
                if let Some(result) = result {
                    match result {
                        Ok(status) => {
                            let text = match status {
                                crate::update::UpdateStatus::Available { .. } => format!("{status}\n\nRun `octet update` to install."),
                                status => status.to_string(),
                            };
                            if update_report_open && interactions.is_empty() && !shell.has_panel() {
                                shell.show_overlay_text(text);
                            } else { shell.notice(text); }
                        }
                        Err(error) => shell.error(format!("update check failed: {error}")),
                    }
                }
                shell.render();
            }
            result = futures_util::future::OptionFuture::from(subagent_refresh.as_mut().map(|future| future.as_mut())), if subagent_refresh.is_some() => {
                subagent_refresh = None;
                let error = result.and_then(Result::err).map(|error| format!("subagent view live refresh failed; showing the last accepted state: {error}"));
                if error != subagent_refresh_error {
                    if let Some(error) = error.as_ref() { shell.notice(error.clone()); }
                    subagent_refresh_error = error;
                }
                apply_extension_background(shell, executable_extensions);
                if subagents_open && subagent_document.is_none() && interactions.is_empty() {
                    refresh_active_subagent_list(shell, executable_extensions);
                }
                shell.render();
            }
            _ = modal_refresh.tick(), if subagents_open && interactions.is_empty() => {
                if subagent_refresh.is_none() { subagent_refresh = executable_extensions.subagent_status_check(); }
                if let Some(id) = subagent_document.as_deref() {
                    open_active_subagent_document(shell, executable_extensions, run, id, false);
                } else { refresh_active_subagent_list(shell, executable_extensions); }
                shell.render();
            }
            _ = extension_tick.tick() => {
                if apply_extension_background(shell, executable_extensions) {
                    shell.render();
                }
            }
            incoming = async {
                if let Some(event) = clipboard_fallback.take() {
                    (Some(Ok(event)), true)
                } else {
                    (input.next().await, false)
                }
            }, if input_open => {
                let (maybe, clipboard_replay) = incoming;
                let event = match maybe {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        control.abort();
                        shell.fail_run(run_id, format!("terminal input failed: {error}"));
                        return Err(error.into());
                    }
                    None => {
                        // A fused/closed stream is immediately ready forever.
                        // Disable this select branch after the first EOF so it
                        // cannot starve the Agent's terminal RunFinished event.
                        input_open = false;
                        control.abort();
                        aborting = true;
                        intents.clear();
                        in_flight = None;
                        shell.set_run_preparing(run_id, "cancelling");
                        shell.render();
                        *quit_requested = true;
                        continue;
                    }
                };
                if clipboard_replay {
                    // A higher-priority branch may have changed ownership since
                    // the failed read settled on the preceding select iteration.
                    let editor = shell.extension_editor_snapshot();
                    if !editor.focused || editor.revision != clipboard_revision {
                        continue;
                    }
                }
                // Search owns its query before any extension or clipboard
                // admission; pasted paths/text must not become composer input.
                if !clipboard_replay && interactions.is_empty() && !shell.has_panel() && shell.intercept_transcript_input(&event) {
                    continue;
                }
                if !clipboard_replay {
                    observe_extension_terminal_event(executable_extensions, &event);
                }
                if !clipboard_replay && matches!(&event, Event::Key(key) if keymap::is_close_key(key)) {
                    request_active_close(
                        control,
                        shell,
                        run_id,
                        &mut input_open,
                        &mut aborting,
                        &mut intents,
                        &mut in_flight,
                        quit_requested,
                    );
                    continue;
                }
                // Tool consent preempts ordinary inspection. No provider events are
                // buffered by a modal and no request can hide behind a report.
                if let Some(interaction) = interactions.front_mut().filter(|_| !clipboard_replay) {
                    if interaction.input(shell, &event) {
                        interactions.pop_front();
                        if let Some(next) = interactions.front() { next.open(shell); }
                    }
                    shell.render();
                    continue;
                }
                if !clipboard_replay && shell.has_panel() {
                    // Ctrl+C remains draft-sensitive even while an ordinary picker
                    // owns navigation. Escape only dismisses that picker.
                    if matches!(&event, Event::Key(key) if key.kind == KeyEventKind::Press
                        && key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)) {
                        if !shell.pending().is_empty() {
                            clipboard = None;
                            clipboard_gesture = None;
                            shell.clear_editor();
                        } else {
                            control.abort(); aborting = true;
                            intents.clear(); in_flight = None;
                            shell.close_panel(); subagents_open = false; subagent_document = None;
                            shell.set_run_preparing(run_id, "cancelling");
                        }
                        shell.render(); continue;
                    }
                    if let Some((result, action)) = shell.panel_input(&event) {
                        match (result, action) {
                            (PanelResult::Confirm(index), PanelAction::SelectGroupedModel { models, .. }) => {
                                if let Some(id) = models.get(index) {
                                    if let Err(error) = crate::cli::persist_model(&id.0) {
                                        shell.error(format!("failed to save model preference: {error}"));
                                    }
                                    push_pending_action(pending_actions, PendingIdleAction::ChangeModel(id.clone()));
                                    shell.notice("model change queued for the next idle boundary");
                                }
                            }
                            (PanelResult::Confirm(index), PanelAction::SelectThinking(levels)) => {
                                if let Some(level) = levels.get(index) {
                                    let reasoning = match requested_thinking_to_reasoning(*level,
                                        &inspection.model, inspection.subagents_available) {
                                        Ok(reasoning) => reasoning,
                                        Err(error) => {
                                            shell.error(format!("thinking unchanged: {error}"));
                                            shell.render();
                                            continue;
                                        }
                                    };
                                    if inspection.model.responses_features().reasoning_effort_updates {
                                        pending_reasoning = Some(reasoning);
                                    } else {
                                        push_pending_action(pending_actions, PendingIdleAction::ChangeThinking(reasoning));
                                        shell.notice("thinking change queued for the next idle boundary");
                                    }
                                } else if let Some(surface) = codex_context_surface(&inspection.model) {
                                    open_active_codex_context(shell, &surface);
                                }
                            }
                            (result, PanelAction::ProviderSetup(kind)) if kind.first().is_some_and(|kind| kind == "codex-context") => {
                                if matches!(result, PanelResult::Confirm(1)) {
                                    if let Some(surface) = codex_context_surface(&inspection.model) {
                                        if let Some(target) = surface.raise_target() {
                                            shell.notice(format!("{} Set acknowledged for the launch boundary.",
                                                crate::codex_context::CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING));
                                            match surface.raise(target, true) {
                                                Ok(window) => shell.notice(format!("Codex context window raise to {window} tokens accepted: {}", surface.raise_instruction(target))),
                                                Err(reason) => shell.error(format!("Codex context window unchanged at {} tokens: {reason}", surface.effective_window())),
                                            }
                                        }
                                    }
                                }
                                open_active_thinking(shell, inspection);
                            }
                            (PanelResult::Confirm(index), PanelAction::SelectSubagent(panel)) => {
                                subagent_document = panel.node_ids.get(index).cloned();
                                if let Some(id) = subagent_document.as_deref() {
                                    open_active_subagent_document(shell, executable_extensions, run, id, true);
                                }
                            }
                            (_, PanelAction::ReadOnlyDocument) if subagents_open => {
                                subagent_document = None;
                                open_active_subagent_list(shell, executable_extensions);
                            }
                            _ => { subagents_open = false; subagent_document = None; }
                        }
                    }
                    shell.render();
                    continue;
                }
                if shell.has_overlay() {
                    match event {
                        Event::Mouse(_) => continue,
                        Event::Resize(columns, rows) => {
                            shell.set_size(columns, rows);
                            shell.render();
                            continue;
                        }
                        _ => match shell.overlay_input(&event) {
                            OverlayInputResult::Consumed => {
                                shell.render();
                                continue;
                            }
                            OverlayInputResult::Closed => {
                                update_report_open = false;
                                shell.clear_error();
                                shell.render();
                                continue;
                            }
                            OverlayInputResult::Legacy => {
                                update_report_open = false;
                                shell.close_overlay();
                                shell.clear_error();
                                shell.render();
                                continue;
                            }
                        },
                    }
                }
                if let Some(shortcut) = (!clipboard_replay).then(|| executable_extensions.dispatch_shortcut_for_event(&event)).flatten() {
                    shell.notice(format!(
                        "running extension shortcut {}: {}",
                        shortcut.extension, shortcut.description
                    ));
                    shell.render();
                    continue;
                }
                // Keep polling input and the run while a native helper waits.
                // Coalesce held/repeated gestures to one bounded read.
                if !clipboard_replay && matches!(&event, Event::Key(key) if is_clipboard_paste_key(key)) {
                    if !aborting && clipboard.is_none() {
                        clipboard_revision = shell.extension_editor_snapshot().revision;
                        clipboard = Some(Box::pin(clipboard_read::read_text()));
                        clipboard_gesture = Some(event);
                    }
                    continue;
                }
                let action = match shell.translate_input(Some(event), true) {
                    InputAction::SlashMenu(action) => {
                        if shell.slash_menu(action) {
                            InputAction::Command(shell.pending())
                        } else {
                            shell.render();
                            continue;
                        }
                    }
                    action => action,
                };
                // These actions consume or replace the draft that admitted the
                // read. Its eventual text must not enter the next composer.
                if matches!(&action, InputAction::Queue(_) | InputAction::Steer(_)
                    | InputAction::Command(_) | InputAction::EditQueued | InputAction::ClearEditor)
                {
                    clipboard = None;
                    clipboard_gesture = None;
                }
                match action {
                    InputAction::CompletePath => {
                        if shell.accept_extension_autocomplete() {
                            shell.render();
                        } else if !executable_extensions
                            .request_editor_autocomplete(shell.extension_editor_snapshot())
                        {
                            shell.complete_path();
                            shell.render();
                        }
                    }
                    InputAction::Abort | InputAction::DispatchQueued => {
                        // A second Escape during settlement must not turn a
                        // plain Ctrl+C cancellation into an implicit send.
                        if !aborting {
                            dispatch_queued = matches!(action, InputAction::DispatchQueued);
                        }
                        control.abort();
                        // A steer send can be waiting for acknowledgement or
                        // still be only a local intent. Stop dispatching both,
                        // then let SteeringDelivered/RunFinished settle which
                        // entries became durable before restoring the rest.
                        aborting = true;
                        intents.clear();
                        in_flight = None;
                        shell.set_run_preparing(run_id, "cancelling");
                        shell.render();
                    }
                    InputAction::EditQueued => {
                        shell.edit_queued_message();
                        shell.render();
                    }
                    InputAction::ClearEditor => {
                        shell.clear_editor();
                        shell.render();
                    }
                    InputAction::Queue(_) => {
                        if !aborting {
                            let composed = shell.drain_composed();
                            if !dispatch_active_shell_escape(
                                &composed,
                                run,
                                shell,
                                input,
                                executable_extensions,
                                inspection,
                                goal_deadline,
                                pending_actions,
                                quit_requested,
                            )
                            .await?
                            {
                                shell.queue_follow_up(composed);
                            }
                        }
                        shell.render();
                    }
                    InputAction::Steer(_) => {
                        if !aborting {
                            let composed = shell.drain_composed();
                            if !composed.is_empty()
                                && !dispatch_active_shell_escape(
                                    &composed,
                                    run,
                                    shell,
                                    input,
                                    executable_extensions,
                                    inspection,
                                    goal_deadline,
                                    pending_actions,
                                    quit_requested,
                                )
                                .await?
                            {
                                // Reserve synchronously before publishing a shell receipt.
                                // A full/ended control queue has accepted no payload, so its
                                // editable projection returns directly to the composer instead
                                // of becoming a stale FIFO entry ahead of later steering.
                                let ComposedInput {
                                    display_text,
                                    transcript_text,
                                    parts,
                                    attachments,
                                    ..
                                } = composed;
                                match control.prepare_steer(parts) {
                                    Ok((prepared, receipt)) => {
                                        shell.queue_retractable_steering(
                                            receipt,
                                            transcript_text,
                                            display_text,
                                            attachments,
                                        );
                                        intents.push_back(ControlIntent::SteerPrepared(prepared));
                                    }
                                    Err(error) => {
                                        shell.restore_unqueued_steering(display_text, attachments);
                                        shell.error(format!("could not queue steering: {error}"));
                                    }
                                }
                            }
                        }
                        shell.render();
                    }

                    InputAction::Command(text) if text.starts_with("/skill:") => {
                        // Selecting a skill from the slash popup emits Command
                        // even though its invocation is an ordinary prompt.
                        // Keep the same follow-up path as typed Enter while a
                        // run owns the agent; expansion happens at admission.
                        if !aborting {
                            let composed = shell.drain_composed();
                            shell.queue_follow_up(composed);
                        }
                        shell.render();
                    }
                    InputAction::Command(text) => {
                        if aborting && matches!(commands::parse(&text), Command::Answer(_)) {
                            shell.notice("run is settling · answer request kept in the draft");
                            shell.render();
                            continue;
                        }
                        let command = commands::parse(&shell.consume_command_text(text));
                        if let Command::Thinking(Some(level)) = &command {
                            let requested = ThinkingLevel::parse(level).and_then(|level| requested_thinking_to_reasoning(
                                level, &inspection.model, inspection.subagents_available,
                            ));
                            match requested {
                                Err(error) => { shell.error(error.to_string()); shell.render(); continue; }
                                Ok(reasoning) if inspection.model.responses_features().reasoning_effort_updates => {
                                    if aborting {
                                        shell.error("thinking unchanged: run is settling".into());
                                    } else {
                                        pending_reasoning = Some(reasoning);
                                    }
                                    shell.render();
                                    continue;
                                }
                                Ok(_) => {}
                            }
                        }
                        let was_quit = matches!(command, Command::Exit);
                        if matches!(command, Command::Update) {
                            if update_check.is_none() { update_check = Some(Box::pin(crate::update::check())); }
                            update_report_open = true;
                            shell.show_overlay_text("Checking for updates…".into());
                            shell.render();
                            continue;
                        }
                        if let Command::Answer(instruction) = &command {
                            if !aborting {
                                let composed = answer_now_input(instruction.clone());
                                shell.queue_steering(&composed);
                                intents.push_back(ControlIntent::FinishNow(
                                    composed.into_user_input(),
                                ));
                                shell.notice(
                                    "answer requested · tools disabled at the next safe boundary",
                                );
                            }
                            shell.render();
                            continue;
                        }
                        if matches!(&command, Command::Unknown(text)
                            if is_live_subagents_command(text, executable_extensions))
                        {
                            subagents_open = open_active_subagent_list(shell, executable_extensions);
                            subagent_document = None;
                            shell.render();
                            continue;
                        }
                        let context = run.context_snapshot();
                        if let Err(error) = handle_active_command(
                            shell,
                            command,
                            inspection,
                            executable_extensions,
                            &context,
                            goal_deadline,
                            |principal: &str, reference: &str| {
                                run.open_delegated_session_reference(principal, reference)
                            },
                            input,
                            pending_actions,
                            quit_requested,
                        )
                        .await
                        {
                            shell.error(format!("command failed: {error}"));
                        }
                        if was_quit {
                            control.abort();
                            aborting = true;
                            intents.clear();
                            in_flight = None;
                            shell.set_run_preparing(run_id, "cancelling");
                            shell.render();
                        }
                    }
                    InputAction::CompleteSlashCommand => {
                        shell.complete_slash_command();
                        shell.render();
                    }
                    InputAction::Edit(action) => {
                        shell.apply_edit(action);
                        shell.render();
                    }
                    InputAction::Resize(columns, rows) => {
                        shell.set_size(columns, rows);
                        shell.render();
                    }
                    InputAction::Scroll(direction) => {
                        shell.scroll(direction);
                        shell.render();
                    }
                    InputAction::ScrollLines(direction) => {
                        shell.scroll_lines(direction);
                        scroll_dirty = true;
                    }
                    InputAction::JumpToTail => {
                        shell.jump_to_tail();
                        shell.render();
                    }
                    InputAction::SelectAllTranscript => {
                        shell.select_all_transcript();
                        shell.render();
                    }
                    InputAction::CopyTranscriptSelection => {
                        if shell.copy_selected_plain_text().is_some() {
                            shell.notice("copied to clipboard");
                        }
                        shell.render();
                    }
                    InputAction::TranscriptPointer(gesture) => {
                        match gesture {
                            crate::tui::keymap::PointerGesture::Begin { row, col, extend } => {
                                shell.begin_transcript_selection(row, col, extend);
                            }
                            crate::tui::keymap::PointerGesture::Extend { row, col } => {
                                shell.extend_transcript_selection(row, col);
                            }
                            crate::tui::keymap::PointerGesture::End { row, col } => {
                                shell.end_transcript_selection(row, col);
                            }
                        }
                        shell.render();
                    }
                    InputAction::ShowCompactionSummary => {
                        shell.show_compaction_summary();
                        shell.render();
                    }
                    InputAction::ToggleDisclosure => {
                        shell.toggle_disclosure();
                        shell.render();
                    }
                    InputAction::CycleThinking => {
                        if inspection.model.responses_features().reasoning_effort_updates {
                            let levels = supported_levels_with_subagents(&inspection.model, inspection.subagents_available);
                            let current = pending_reasoning.as_ref().map(reasoning_label)
                                .unwrap_or_else(|| shell.selected_identity().1.trim_end_matches(" (queued)").to_owned());
                            if let Some(index) = levels.iter().position(|level| level.label() == current) {
                                match requested_thinking_to_reasoning(
                                    levels[(index + 1) % levels.len()], &inspection.model, inspection.subagents_available,
                                ) {
                                    Ok(reasoning) => pending_reasoning = Some(reasoning),
                                    Err(error) => shell.error(format!("thinking unchanged: {error}")),
                                }
                            }
                        } else {
                            push_pending_action(pending_actions, PendingIdleAction::CycleThinking);
                            shell.notice("thinking change queued for the next idle boundary");
                        }
                        shell.render();
                    }
                    InputAction::Close => {
                        shell.clear_error();
                        shell.render();
                    }
                    InputAction::FocusGained => apply_focus_transition(shell, true),
                    InputAction::FocusLost => apply_focus_transition(shell, false),
                    InputAction::Closed => {
                        request_active_close(
                            control,
                            shell,
                            run_id,
                            &mut input_open,
                            &mut aborting,
                            &mut intents,
                            &mut in_flight,
                            quit_requested,
                        );
                    }
                    InputAction::Ignore | InputAction::Submit(_) => {}
                    InputAction::SlashMenu(_) => unreachable!(
                        "slash-menu actions are handled before active command dispatch"
                    ),
                }
            }
            event = run.next() => match event {
                Some(event) => {
                    if matches!(&event, AgentEvent::TurnStarted) && inspection.model.responses_features().reasoning_effort_updates {
                        if let Ok(session) = inspection.read_only_session() {
                            if let Ok(Some((_, reasoning))) = session.responses_reasoning(&inspection.model.endpoint.id, &inspection.model.spec.id) {
                                shell.set_identity(&inspection.model.endpoint.id.0, &inspection.model.spec.id.0, &reasoning_label(&reasoning));
                            }
                        }
                    }
                    if let AgentEvent::ToolStarted { id, name, args } = &event {
                        *made_tool_call = true;
                        tool_calls.insert(id.clone(), (name.clone(), args.clone()));
                    }
                    if let Some(interaction) = ActiveToolInteraction::from_event(&event, &tool_calls) {
                        if aborting || *quit_requested || interactions.len() >= 32
                            || interactions.iter().map(ActiveToolInteraction::request_bytes).sum::<usize>()
                                .saturating_add(interaction.request_bytes()) > 256 * 1024
                        {
                            // Bound both request count and bytes; private input buffers
                            // additionally retain at most 4 KiB per request.
                            // Saturation is an explicit denial, never implicit consent.
                            shell.notice_error("interactive tool request denied while cancelling or at the pending-request limit");
                        } else {
                            if interactions.is_empty() {
                                shell.close_panel(); shell.close_overlay();
                                update_report_open = false;
                                subagents_open = false; subagent_document = None;
                                interaction.open(shell);
                            }
                            interactions.push_back(interaction);
                        }
                    }
                    if let AgentEvent::ToolFinished { id, .. } = &event {
                        let was_front = interactions.front().is_some_and(|request| &request.id == id);
                        interactions.retain(|request| &request.id != id);
                        if was_front {
                            shell.close_panel(); shell.set_tool_input_prompt(None);
                            if let Some(next) = interactions.front() { next.open(shell); }
                        }
                    }
                    shell.on_run_event(run_id, &event);
                    assistant_message.observe(executable_extensions, &event);
                    if let AgentEvent::ToolFinished { id, result, .. } = &event {
                        if let Some((name, arguments)) = tool_calls.remove(id) {
                            let (output, is_error) = match result {
                                Ok(output) => (Some(output.text.clone()), output.is_error()),
                                Err(error) => (Some(error.message.clone()), true),
                            };
                            executable_extensions.request_tool_render(
                                id.clone(),
                                &name,
                                arguments,
                                output,
                                is_error,
                            );
                            for message in executable_extensions.drain_events() {
                                shell.notice(message);
                            }
                        }
                    }
                    if let AgentEvent::TurnFinished {
                        session_cost_microdollars,
                        run_cost_microdollars,
                        ..
                    } = &event
                    {
                        let turn_cost = run_cost_microdollars.saturating_sub(last_run_cost);
                        if cost_warning_microdollars.is_some_and(|threshold| turn_cost >= threshold)
                        {
                            shell.notice(format!(
                                "turn cost warning: {} reached the {} threshold",
                                crate::commands::format_microdollars(turn_cost),
                                crate::commands::format_microdollars_cents(
                                    cost_warning_microdollars.unwrap_or_default()
                                )
                            ));
                        }
                        last_run_cost = *run_cost_microdollars;
                        if let (Some(limit), Some(total)) =
                            (max_cost_microdollars, *session_cost_microdollars)
                        {
                            if total >= limit {
                                shell.error(format!(
                                    "Session cost limit of {} reached.",
                                    crate::commands::format_microdollars_cents(limit)
                                ));
                                control.abort();
                                aborting = true;
                                intents.clear();
                                in_flight = None;
                            }
                        }
                    }
                    let run_finished = matches!(&event, AgentEvent::RunFinished { .. });
                    if run_finished {
                        interactions.clear();
                        shell.close_panel();
                        shell.set_tool_input_prompt(None);
                        // The renderer is asynchronous and coalesces requests.
                        // Restore any steer that lost the final delivery race
                        // before requesting the terminal frame, so idle chrome,
                        // the terminal outcome, and the editor are one atomic
                        // presentation state.
                        shell.restore_queued_steering();
                        let dispatch = !*quit_requested && !shell.close_requested()
                            && matches!(&event, AgentEvent::RunFinished { reason, .. }
                                if matches!(reason, octet_agent::FinishReason::Completed)
                                    || (dispatch_queued && matches!(reason, octet_agent::FinishReason::Aborted)));
                        shell.settle_queued_follow_ups(dispatch);
                    }
                    shell.render();
                    if let AgentEvent::RunFinished { reason, .. } = event {
                        let (endpoint, model) = shell
                            .current_run_route()
                            .unwrap_or_else(|| ("unknown".to_owned(), "unknown".to_owned()));
                        return Ok(HostRunOutcome::from_finish_reason(
                            &reason,
                            &endpoint,
                            &model,
                        ));
                    }
                }
                None => {
                    interactions.clear();
                    shell.close_panel();
                    shell.set_tool_input_prompt(None);
                    assistant_message.settle(executable_extensions);
                    shell.restore_queued_steering();
                    shell.fail_run(run_id, RUN_STREAM_LOST_MESSAGE);
                    shell.render();
                    return Ok(HostRunOutcome::stream_lost());
                }
            },
        }
    }
}

fn cost_limit_message(app: &App) -> Option<String> {
    let limit = app.config.max_cost_microdollars?;
    (app.agent.session().total_cost_microdollars() >= limit).then(|| {
        format!(
            "Session cost limit of {} reached.",
            crate::commands::format_microdollars_cents(limit)
        )
    })
}

fn prepare_prompt(shell: &mut InteractiveShell) {
    // Errors describe the previous interaction. Once a new prompt is accepted
    // they are stale and must not remain pinned below the active run.
    shell.clear_error();
}

fn status_context_estimate(app: &App) -> u64 {
    // Context is a property of the next request, not cumulative session spend.
    // This borrows Session's cached model-visible messages, so compaction and
    // checkout are reflected immediately without cloning the transcript.
    estimate_next_request_tokens(app, &[])
}

fn update_status(shell: &mut InteractiveShell, app: &App) {
    let context_estimate = status_context_estimate(app);
    let cache_stats = analyze_session_cache_stats(app.agent.session());
    let endpoint_label = app
        .catalog
        .endpoint_label(&app.model.endpoint.id)
        .unwrap_or(&app.model.endpoint.id.0);
    shell.set_identity(
        endpoint_label,
        &app.model.spec.id.0,
        &crate::app::reasoning_label(&app.reasoning),
    );
    // Registry/configured metadata overrides the conservative canonical-ID
    // fallback installed by `set_identity` and is cached until model switch.
    shell.set_model_theme(&app.model);
    shell.set_model_cycle(app.model_cycle());
    shell.set_status_detail(commands::status_text_with_metrics(
        app,
        None,
        context_estimate,
        &cache_stats,
    ));
    shell.set_input_modalities(app.model.spec.capabilities.input_modalities);
    shell.set_workspace(app.config.workspace.clone());
    shell.set_prompt_templates(app.prompts.descriptors());
    shell.set_skill_commands(Arc::from(
        app.skills
            .descriptors()
            .iter()
            .map(|skill| (format!("skill:{}", skill.id), skill.description.clone()))
            .collect::<Vec<_>>(),
    ));
    shell.set_extension_commands(Arc::from(app.executable_extensions.command_suggestions()));
    shell.set_context_estimate(context_estimate, context_window(&app.model));
    shell.set_session_telemetry(
        app.agent.session(),
        cache_stats.latest_raw_hit_rate_basis_points(),
    );
}

fn request_extension_ui(shell: &mut InteractiveShell, app: &mut App) {
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    for message in app.executable_extensions.drain_events_for_shell(shell) {
        shell.notice(message);
    }
    if app
        .executable_extensions
        .apply_session_host_requests(&mut app.agent, &app.sessions)
    {
        shell.notice("extension updated the session metadata");
    }
    let _ = app.executable_extensions.sync_semantic_ui(shell);
    app.executable_extensions
        .sync_editor_state(shell.extension_editor_snapshot());
}

fn apply_extension_background(
    shell: &mut InteractiveShell,
    executable_extensions: &mut crate::extensions::ExecutableExtensions,
) -> bool {
    let updates = executable_extensions.drain_background_updates();
    let mut changed = false;
    for message in executable_extensions.drain_post_mutation_rescans() {
        shell.notice(message);
        changed = true;
    }
    for update in updates.rendered_tools {
        shell.apply_extension_tool_renderer(&update.id, &update.segments);
        changed = true;
    }
    for update in updates.autocomplete {
        if shell.set_extension_autocomplete(&update.snapshot, update.prefix, update.items) {
            changed = true;
        }
    }
    for message in updates.shortcut_messages {
        shell.notice(message);
        changed = true;
    }
    for message in executable_extensions.drain_events_for_shell(shell) {
        shell.notice(message);
        changed = true;
    }
    if executable_extensions.sync_semantic_ui(shell) {
        changed = true;
    }
    executable_extensions.sync_editor_state(shell.extension_editor_snapshot());
    // Extension contributions can arrive (or change) after the initial
    // handshake; keep the composer's slash-command list in step so commands
    // like /subagents are enterable as soon as their owning process is ready.
    shell.set_extension_commands(Arc::from(executable_extensions.command_suggestions()));
    changed
}

fn report_compaction(shell: &mut InteractiveShell, outcome: &CompactionOutcome, session: &Session) {
    match outcome {
        CompactionOutcome::Compacted { elided } => {
            let usage = session
                .usage_records()
                .iter()
                .rev()
                .find(|record| matches!(record.kind, octet_agent::UsageRecordKind::Compaction));
            let detail = usage.map_or_else(
                || format!("{elided} earlier messages summarized"),
                |record| {
                    let cost = record
                        .cost_microdollars
                        .map(commands::format_microdollars)
                        .unwrap_or_else(|| "cost unavailable".to_owned());
                    let prompt_tokens = record
                        .usage
                        .input_tokens
                        .saturating_add(record.usage.cache_read_tokens)
                        .saturating_add(record.usage.cache_write_tokens);
                    format!(
                        "{prompt_tokens} input tokens summarized · {cost} compaction cost{}",
                        if session.has_uncertain_usage() || session.has_unpriced_usage() {
                            " (session usage or pricing uncertain)"
                        } else {
                            ""
                        }
                    )
                },
            );
            let summary = session
                .head()
                .and_then(|head| session.entry(&head))
                .and_then(|entry| match &entry.value {
                    octet_agent::EntryValue::Compaction { summary, .. } => Some(summary.clone()),
                    _ => None,
                });
            if let Some(summary) = summary {
                shell.compaction_marker(format!("Context compacted · {detail}"), summary);
            } else {
                shell.error("compaction completed without a durable summary marker".to_owned());
            }
        }
        CompactionOutcome::NativeCompacted => {
            let usage = session
                .usage_records()
                .iter()
                .rev()
                .find(|record| matches!(record.kind, octet_agent::UsageRecordKind::Compaction));
            let detail = usage.map_or_else(
                || "opaque Responses state retained".to_owned(),
                |record| {
                    let cost = record
                        .cost_microdollars
                        .map(commands::format_microdollars)
                        .unwrap_or_else(|| "cost unavailable".to_owned());
                    let prompt_tokens = record
                        .usage
                        .input_tokens
                        .saturating_add(record.usage.cache_read_tokens)
                        .saturating_add(record.usage.cache_write_tokens);
                    format!(
                        "{prompt_tokens} input tokens compacted · {cost} compaction cost{}",
                        if session.has_uncertain_usage() || session.has_unpriced_usage() {
                            " (session usage or pricing uncertain)"
                        } else {
                            ""
                        }
                    )
                },
            );
            shell.native_compaction_marker(format!("Context compacted natively · {detail}"));
        }
        CompactionOutcome::Skipped { reason } => {
            shell.notice(format!("compaction skipped: {reason}"))
        }
    }
}

fn configure_auto_compaction(
    app: &mut App,
    shell: &mut InteractiveShell,
    setting: Option<commands::AutoCompactSetting>,
) -> anyhow::Result<()> {
    let mut candidate_mode = app.config.compaction.mode;
    let mut candidate_threshold = app.config.compaction.threshold_fraction;
    match setting {
        Some(commands::AutoCompactSetting::Mode(mode)) => {
            if mode == CompactionMode::NativeResponses
                && app.model.spec.protocol != octet_ai::Protocol::OpenAiResponses
            {
                shell.error(format!(
                    "native Responses compaction is unavailable for {:?}; select an OpenAI Responses model or use local compaction",
                    app.model.spec.protocol
                ));
                return Ok(());
            }
            if mode == CompactionMode::NativeResponses
                && app
                    .config
                    .compaction
                    .compact_model
                    .as_ref()
                    .is_some_and(|model| model != &app.model.spec.id)
            {
                shell.error(
                    "native Responses compaction requires compaction.compact_model to match the active model"
                        .to_owned(),
                );
                return Ok(());
            }
            candidate_mode = mode;
        }
        Some(commands::AutoCompactSetting::ThresholdPercent(percent)) => {
            candidate_threshold = f64::from(percent) / 100.0;
        }
        None => {}
    }
    let agent_mode = match candidate_mode {
        CompactionMode::Disabled => AgentCompactionMode::Disabled,
        CompactionMode::Local => AgentCompactionMode::Local,
        CompactionMode::NativeResponses => AgentCompactionMode::NativeResponses,
    };
    let mut candidate_config = app.config.clone();
    candidate_config.compaction.mode = candidate_mode;
    candidate_config.compaction.threshold_fraction = candidate_threshold;
    let effective_threshold =
        effective_compaction_threshold_fraction(&candidate_config, &app.model);
    if let Err(error) = app.agent.set_compaction_token_mode(
        agent_mode,
        effective_threshold,
        app.config.compaction.keep_recent_tokens,
    ) {
        shell.error(format!("auto-compaction was not changed: {error}"));
        return Ok(());
    }
    // Publish the candidate only after the Agent accepts it. In particular, a
    // legacy Responses session cannot leave configuration claiming `native`
    // while the Agent safely remains in its previous mode.
    app.config.compaction.mode = candidate_mode;
    app.config.compaction.threshold_fraction = candidate_threshold;
    let threshold_detail = if (effective_threshold - candidate_threshold).abs() < f64::EPSILON {
        format!("{:.0}%", effective_threshold * 100.0)
    } else {
        format!(
            "{:.0}% effective (configured {:.0}%)",
            effective_threshold * 100.0,
            candidate_threshold * 100.0
        )
    };
    shell.notice(format!(
        "auto-compaction {} at {threshold_detail} · keep ~{} recent tokens · this process",
        candidate_mode.label(),
        app.config.compaction.keep_recent_tokens,
    ));
    Ok(())
}

/// Bounded, descriptor-bound observation; unknown files never produce a claim
/// that a config mutation committed. Raw bytes remain inside the host.
fn configuration_snapshot(path: &Path) -> Option<Option<Vec<u8>>> {
    match octet_agent::secure_fs::read_regular_file_bounded(path, 1024 * 1024) {
        Ok(bytes) => Some(Some(bytes)),
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Some(None)
        }
        Err(_) => None,
    }
}

async fn observe_configuration_commit(
    extensions: &mut crate::extensions::ExecutableExtensions,
    before: Option<Option<Vec<u8>>>,
    path: Option<&Path>,
) -> bool {
    let (Some(before), Some(path)) = (before, path) else {
        return false;
    };
    let Some(after) = configuration_snapshot(path) else {
        return false;
    };
    if before == after {
        return false;
    }
    // An observation owner lives inside this process. No contents, paths,
    // credentials, or user identities appear in its stable notification ID.
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    extensions
        .notify_configuration_changed(
            format!("configuration:{generation}"),
            generation,
            octet_agent::PostMutationState::Committed,
        )
        .await;
    true
}

async fn persist_configuration<T>(
    extensions: Option<&mut crate::extensions::ExecutableExtensions>,
    persist: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let path = extensions
        .as_ref()
        .and_then(|_| crate::cli::global_config_path());
    let before = path.as_deref().and_then(configuration_snapshot);
    let result = persist()?;
    if let Some(extensions) = extensions {
        observe_configuration_commit(extensions, before, path.as_deref()).await;
    }
    Ok(result)
}

async fn reload_resources(
    app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<App> {
    let _diagnostics = crate::output::defer_tui_diagnostics();
    let background = shell.theme().background();
    let (app, theme) = run_blocking_lifecycle(shell, input, "reloading resources…", move || {
        let mut app = app;
        app.system = compose_instructions(&app.config)?;
        app.system_tokens = estimate_text_tokens(&app.system);
        let app = rebuild_app(app, None, None, None, None)?;
        let theme = load_theme_for_background(&app.config, background);
        Ok((app, theme))
    })
    .await?;
    shell.set_theme(theme);
    shell.set_runtime_config(app.config.clone());
    shell.reload_keybindings();
    shell.hydrate(app.agent.session())?;
    app.executable_extensions
        .activate_session_lifecycle_driver();
    update_status(shell, &app);
    Ok(app)
}

/// How one `/reload`-family caller wants the host layer handled.
///
/// The host layer is the only layer that executes a replacement image, so which
/// caller selected it decides what consent exists: plain `/reload` and the
/// queued/extension request path are resources-only, a typed `/reload --force`
/// is its own explicit confirmation, and the automatic `reload_host = true`
/// watcher must defer to an explicit command before any consent is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostPass {
    /// Resources only: never probe, never re-exec.
    ResourcesOnly,
    /// The host layer was selected. `redirect_confirmed` is true when the
    /// caller's own explicit user action already confirmed a retargeted image.
    Allowed { redirect_confirmed: bool },
}

impl HostPass {
    fn may_prompt(self) -> bool {
        matches!(
            self,
            Self::Allowed {
                redirect_confirmed: true
            }
        )
    }
}

#[derive(Debug)]
enum HostReloadResult {
    Unchanged,
    Problem(String),
    Ready(crate::reexec::ReexecPlan),
}

/// `/reload` keeps its existing transactional resource reload, then re-execs
/// only when the on-disk executable changed *and* the caller selected the host
/// layer.
///
/// Returns `Some(plan)` when the caller must leave the TUI and call
/// `plan.exec()` after the loop settles. `None` covers a resources-only reload
/// and every refusal or validation failure, all of which leave this process
/// fully live.
async fn reload_resources_with_reexec(
    app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    reexec: Option<&mut crate::reexec::ReexecController>,
    host: HostPass,
    reload: &mut crate::reload::ReloadSupervisor,
) -> anyhow::Result<(App, Option<crate::reexec::ReexecPlan>)> {
    let before = app.executable_extensions.running_generations();
    let mut app = reload_resources(app, shell, input).await?;
    remember_rebuilt_extensions(
        reload,
        &before,
        &app.executable_extensions.running_generations(),
    );
    request_extension_ui(shell, &mut app);
    if host == HostPass::ResourcesOnly {
        return Ok((app, None));
    }
    let (next, result) = reload_host_with_reexec(app, shell, input, reexec, host).await?;
    app = next;
    let plan = match result {
        HostReloadResult::Unchanged => {
            shell.notice(crate::reexec::notice::RESOURCES_ONLY);
            None
        }
        HostReloadResult::Problem(notice) => {
            shell.notice(notice);
            None
        }
        HostReloadResult::Ready(plan) => Some(plan),
    };
    Ok((app, plan))
}

fn remember_rebuilt_extensions(
    reload: &mut crate::reload::ReloadSupervisor,
    before: &std::collections::BTreeMap<String, (String, u64)>,
    after: &std::collections::BTreeMap<String, (String, u64)>,
) {
    for (name, generation) in after {
        if before.get(name) != Some(generation) {
            // A new live instance/generation completed initialization. Retained,
            // stopped, and absent bindings were not successful replacement checks.
            reload.checked_problems(
                crate::reload::ReloadComponent::Extension(name.clone()),
                Vec::new(),
                true,
            );
        }
    }
}

async fn reload_host_with_reexec(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    reexec: Option<&mut crate::reexec::ReexecController>,
    host: HostPass,
) -> anyhow::Result<(App, HostReloadResult)> {
    let Some(reexec) = reexec else {
        return Ok((app, HostReloadResult::Problem(
            "reload: the current executable could not be captured; host replacement is unavailable".into(),
        )));
    };
    let session_id = match crate::app::bootstrap::terminal_goal_session_id(app.agent.session()) {
        Ok(id) => id,
        Err(error) => {
            return Ok((
                app,
                HostReloadResult::Problem(format!(
                    "reload: the active session cannot be resumed: {error}"
                )),
            ));
        }
    };
    reexec.set_session_id(session_id);
    // Resolve the executable *now*. An update commonly retargets a symlink or
    // swaps a version-pinned directory (Homebrew, npm, a pinned installer), so
    // the path this process started from is not necessarily the path `/reload`
    // must enter. The observation carries the resolved path into the probe, the
    // exec, and the notice, and reports an update that is still in flight as its
    // own blocked notice instead of an opaque error.
    let observed = reexec.observe_generation();
    let workers = active_subagent_workers(&app);
    let safety = live_reexec_inputs(&workers);
    // The two consents a host pass can need. A typed `/reload --force` already
    // carries the retarget consent; an automatic host pass must defer before a
    // retargeted image is probed.
    let mut options = crate::reexec::ReexecOptions::refusing_workers();
    if matches!(
        host,
        HostPass::Allowed {
            redirect_confirmed: true
        }
    ) {
        options = options.confirming_redirect();
    }
    // Live workers refuse the reload by default. The one explicit opt-in is the
    // user's decision to detach them, and it is offered only when every worker
    // that would be detached already resolves in this session's durable roster:
    // extension shutdown detaches them, the replacement image reattaches them
    // from that roster alone, and a worker without a durable record would be
    // lost rather than detached. A retargeted image is confirmed the same way
    // *before* it is probed, because the probe is its first execution.
    let decision = loop {
        let (decision, events) =
            reexec_decision(&mut app, reexec, &observed, &safety, options, &workers).await;
        for event in events {
            shell.notice(event);
        }
        match decision {
            crate::reexec::ReexecDecision::ConfirmationRequired { redirect, .. }
                if !options.redirect_confirmed =>
            {
                if !host.may_prompt() {
                    return Ok((
                        app,
                        HostReloadResult::Problem(
                            "reload: binary replacement needs consent; run /reload --force".into(),
                        ),
                    ));
                }
                if confirm_binary_retarget(shell, input, &redirect).await? {
                    options = options.confirming_redirect();
                    continue;
                }
                return Ok((app, HostReloadResult::Problem(
                    "reload cancelled · the replaced binary was not probed or entered; this process is unchanged".into(),
                )));
            }
            crate::reexec::ReexecDecision::Refused {
                reason: crate::reexec::RefusalReason::BackgroundWorkers(count),
                notice,
            } if !options.detach_background_workers => {
                if !host.may_prompt() {
                    return Ok((app, HostReloadResult::Problem(format!(
                        "reload: {count} active background workers need detach consent; run /reload --force"
                    ))));
                }
                if workers.references.len() != count {
                    return Ok((app, HostReloadResult::Problem(format!(
                        "reload: {count} background workers are active and do not all disclose a durable session reference; detaching them could lose work, so the reload stays refused"
                    ))));
                }
                if confirm_worker_detach(shell, input, count).await? {
                    options = options.with_detaching_workers();
                    continue;
                }
                return Ok((app, HostReloadResult::Problem(notice)));
            }
            other => break other,
        }
    };
    match decision {
        crate::reexec::ReexecDecision::ResourcesOnly => Ok((app, HostReloadResult::Unchanged)),
        crate::reexec::ReexecDecision::Refused { notice, .. }
        | crate::reexec::ReexecDecision::Blocked { notice, .. }
        | crate::reexec::ReexecDecision::ExecFailed { notice, .. }
        | crate::reexec::ReexecDecision::ConfirmationRequired { notice, .. } => {
            Ok((app, HostReloadResult::Problem(notice)))
        }
        crate::reexec::ReexecDecision::Ready(plan) => {
            // Imminent replacement and actual detach are occurrences, never deduplicated.
            if let Some(detach) = plan.detach_notice() {
                shell.notice(detach);
            }
            shell.notice(plan.notice());
            shell.render();
            Ok((app, HostReloadResult::Ready(plan)))
        }
    }
}

/// One `/reload` decision taken against the live session and extension manager.
///
/// The hooks borrow the session, the extension manager, and the worker
/// references for the duration of the decision only; nothing below the validated
/// `Ready` moves the application.
async fn reexec_decision(
    app: &mut App,
    reexec: &crate::reexec::ReexecController,
    observed: &crate::reexec::ExecutableObservation,
    safety: &crate::reexec::LiveSafetyInputs,
    options: crate::reexec::ReexecOptions,
    workers: &ActiveWorkers,
) -> (crate::reexec::ReexecDecision, Vec<String>) {
    let mut hooks = InteractiveReexecHooks {
        session: app.agent.session(),
        extensions: &mut app.executable_extensions,
        worker_references: &workers.references,
        discarded_requests: Vec::new(),
    };
    let decision = reexec
        .reexec_if_changed(observed, safety, options, &mut hooks)
        .await;
    (decision, hooks.discarded_requests)
}

/// Ask the user to confirm entering a replaced or moved binary.
///
/// The probe executes the candidate image, so a retargeted image is never
/// probed before this answer. The suggested choice is to keep the current
/// process, and a denied confirmation leaves the binary unexecuted.
async fn confirm_binary_retarget(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    redirect: &crate::reexec::ExecutableRedirect,
) -> anyhow::Result<bool> {
    let detail = "The new image has not been probed or executed yet. Approving runs it once as a \
                  probe and then replaces this process image.";
    let prompt = match redirect {
        crate::reexec::ExecutableRedirect::Moved { from, to } => format!(
            "The octet binary moved from {} to {}. Reload into it?",
            from.display(),
            to.display()
        ),
        crate::reexec::ExecutableRedirect::Replaced { path } => format!(
            "The octet binary at {} was replaced (new file identity). Reload into it?",
            path.display()
        ),
    };
    let request = octet_agent::extension_process::ConfirmationRequest {
        parent_request_id: None,
        prompt,
        detail: Some(detail.to_owned()),
        destructive: false,
        default: false,
    };
    extension_confirmation_picker(shell, input, "octet", &request).await
}

/// The explicit opt-in that lets one `/reload` detach live background workers.
///
/// Refusing is the default policy, and this prompt is the only path that turns
/// the worker refusal into the detach warning. It names the count and states the
/// consequence plainly: the workers keep their durable records and are
/// reattachable in the new image. The suggested choice is to keep the current
/// process.
async fn confirm_worker_detach(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    count: usize,
) -> anyhow::Result<bool> {
    let request = octet_agent::extension_process::ConfirmationRequest {
        parent_request_id: None,
        prompt: format!(
            "Reload into the newer binary and detach {count} background worker{}?",
            if count == 1 { "" } else { "s" }
        ),
        detail: Some(crate::reexec::notice::detaching_workers(count)),
        destructive: false,
        default: false,
    };
    extension_confirmation_picker(shell, input, "octet", &request).await
}

/// Private delegation directory that owns this session's durable worker roster.
///
/// It mirrors the host's layout rather than inventing one: `app::bootstrap`
/// enables V2 delegation with
/// `DelegationConfig::new(session_parent.join(".delegation"))`, `session_store`
/// owns the same private directory, and the roster itself is
/// `<session dir>/.delegation/fleet.json`.
const DELEGATION_DIRECTORY: &str = ".delegation";

/// The approval boundary a worker detach must pass before it is offered.
///
/// The detach itself belongs to extension shutdown, which is why the ordering
/// `crate::reexec` enforces matters: the durable-head hook runs first, and it is
/// this function that makes every worker record durable *before* the workers are
/// handed over. A worker whose durable roster record or transcript is missing
/// fails the reload closed instead of being detached into nothing; the
/// replacement image reattaches workers from that roster alone.
fn verify_worker_records_are_durable(
    session: &Session,
    references: &[String],
) -> anyhow::Result<()> {
    if references.is_empty() {
        return Ok(());
    }
    let directory = session.path().parent().map_or_else(
        || PathBuf::from(DELEGATION_DIRECTORY),
        |parent| parent.join(DELEGATION_DIRECTORY),
    );
    for reference in references {
        octet_agent::delegation::resolve_launchable_child_session(&directory, reference).map_err(
            |error| {
                anyhow::anyhow!(
                    "background worker {reference} has no durable record to reattach: {error}"
                )
            },
        )?;
    }
    Ok(())
}

/// Apply one admitted live-reload pass at the interactive idle boundary.
///
/// This is the only place a pass is applied, and it is reachable only from the
/// idle prompt: `Idle::ReloadDue` is produced by `wait_for_prompt`, which runs
/// before any `Run` exists, and the explicit `/reload --dry-run` /
/// `/reload --force` text is handled in the same idle match arm. A run owns the
/// session inside the `Idle::Submit` arm, which never reaches this function, so
/// "never reload while a run owns the session" is enforced by where this is
/// called from — and `ReloadBoundary::Idle`, which `ReloadSupervisor::begin`
/// refuses to bend, is the checked half of the same rule.
///
/// Host consent is checked before any resource rebuild can tear down worker
/// bindings. An admitted host re-exec supersedes in-process rebuilds; otherwise
/// resources precede extension restarts.
async fn apply_live_reload_plan(
    app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    reload: &mut crate::reload::ReloadSupervisor,
    mut plan: crate::reload::ReloadPlan,
    reexec: Option<&mut crate::reexec::ReexecController>,
    pending_reexec: &mut Option<crate::reexec::ReexecPlan>,
) -> anyhow::Result<App> {
    use crate::reload::{LayerOutcome, ReloadLayer, SkipReason};

    let mut app = app;
    let wants_resources = plan.contains(ReloadLayer::Resources);
    let wants_extensions = plan.contains(ReloadLayer::Extensions);
    let wants_host = plan.contains(ReloadLayer::Host);
    let workers = active_subagent_workers(&app);
    if !plan.is_forced() && workers.count > 0 {
        for notice in reload.checked_problems(
            crate::reload::ReloadComponent::Workers,
            vec![format!("reload deferred: {} active background worker(s); run /reload --force to review interruption and detach consent", workers.count)],
            false,
        ) {
            shell.notice(notice);
        }
        for layer in plan.layers() {
            plan.record(layer, LayerOutcome::Skipped(SkipReason::NotSelected));
        }
        reload.finish(plan);
        shell.render();
        return Ok(app);
    }
    if workers.count == 0 {
        reload.checked_problems(
            crate::reload::ReloadComponent::Workers,
            Vec::new(),
            plan.is_forced(),
        );
    }
    // A forced pass is the user's explicit confirmation of whatever the
    // executable now is; an automatic host pass defers rather than asking for
    // consent to enter a retargeted image.
    let host_pass = if wants_host {
        HostPass::Allowed {
            redirect_confirmed: plan.is_forced(),
        }
    } else {
        HostPass::ResourcesOnly
    };

    if wants_host {
        let (next, result) = reload_host_with_reexec(app, shell, input, reexec, host_pass).await?;
        app = next;
        match result {
            HostReloadResult::Ready(host_plan) => {
                reload.checked_problems(
                    crate::reload::ReloadComponent::Host,
                    Vec::new(),
                    plan.is_forced(),
                );
                plan.record_reload(ReloadLayer::Host);
                plan.record(
                    ReloadLayer::Extensions,
                    LayerOutcome::Skipped(SkipReason::NotSelected),
                );
                *pending_reexec = Some(host_plan);
            }
            HostReloadResult::Unchanged => {
                reload.checked_problems(
                    crate::reload::ReloadComponent::Host,
                    Vec::new(),
                    plan.is_forced(),
                );
                plan.record(
                    ReloadLayer::Host,
                    LayerOutcome::Skipped(SkipReason::NoChange),
                );
                if workers.count > 0 {
                    shell.notice("reload deferred: active background workers remain; no host replacement was selected and no worker teardown was authorized");
                    for layer in [ReloadLayer::Resources, ReloadLayer::Extensions] {
                        plan.record(layer, LayerOutcome::Skipped(SkipReason::NotSelected));
                    }
                    reload.finish(plan);
                    shell.render();
                    return Ok(app);
                }
            }
            HostReloadResult::Problem(problem) => {
                for notice in reload.checked_problems(
                    crate::reload::ReloadComponent::Host,
                    vec![problem],
                    plan.is_forced(),
                ) {
                    shell.notice(notice);
                }
                plan.record(ReloadLayer::Host, LayerOutcome::Failed);
                if workers.count > 0 {
                    for layer in [ReloadLayer::Resources, ReloadLayer::Extensions] {
                        plan.record(layer, LayerOutcome::Skipped(SkipReason::NotSelected));
                    }
                    if let Some(report) = reload.finish(plan) {
                        if report.forced {
                            shell.notice(report.summary());
                        }
                    }
                    shell.render();
                    return Ok(app);
                }
            }
        }
    }

    if wants_resources && pending_reexec.is_none() {
        let _automatic = (!plan.is_forced()).then(crate::output::automatic_diagnostics);
        app = reload_resources(app, shell, input).await?;
        request_extension_ui(shell, &mut app);
        plan.record_reload(ReloadLayer::Resources);
    }

    if wants_extensions && pending_reexec.is_none() {
        let result = await_lifecycle(shell, input, "reloading extensions…", async {
            Ok(app.executable_extensions.reload_report().await)
        })
        .await?;
        let failed = result.processes.iter().any(|(_, result)| result.is_err());
        for notice in extension_reload_notices(reload, result, plan.is_forced()) {
            shell.notice(notice);
        }
        // reload_report already drained typed rescans. Do not merge their
        // discard events or successful generation details into catalog problems.
        let catalog = app
            .executable_extensions
            .synchronize_provider_catalog_report(&mut app.catalog, &app.client);
        for notice in provider_reload_notices(reload, catalog, plan.is_forced()) {
            shell.notice(notice);
        }
        plan.record(
            ReloadLayer::Extensions,
            if failed {
                LayerOutcome::Failed
            } else {
                LayerOutcome::Reloaded
            },
        );
        request_extension_ui(shell, &mut app);
    }

    if let Some(report) = reload.finish(plan) {
        debug_assert!(
            report.boundary_idle || report.forced,
            "a live-reload pass is applied at the idle boundary"
        );
        debug_assert!(
            !reload.is_in_flight(),
            "finishing a pass always clears the in-flight plan"
        );
        // Typed component feedback above owns automatic diagnostics. Rendering
        // the aggregate here would repeat failures and inspection warnings.
        if report.forced {
            for notice in report.notices() {
                shell.notice(notice);
            }
        }
        // Background maintenance should not add success chatter to the chat.
        // Explicit /reload --force still gets a completion report; plain
        // /reload and --dry-run own their feedback at their command sites.
        if report.forced {
            shell.notice(report.summary());
        }
        update_status(shell, &app);
        shell.render();
    }
    Ok(app)
}

fn extension_reload_notices(
    reload: &mut crate::reload::ReloadSupervisor,
    result: crate::extensions::ExtensionReloadReport,
    explicit: bool,
) -> Vec<String> {
    use crate::reload::ReloadComponent;
    let mut notices = Vec::new();
    for (name, outcome) in result.processes {
        let problems = match outcome {
            Ok(detail) => {
                if explicit {
                    notices.push(detail);
                }
                Vec::new()
            }
            Err(problem) => vec![problem],
        };
        notices.extend(reload.checked_problems(
            ReloadComponent::Extension(name),
            problems,
            explicit,
        ));
    }
    notices.extend(reload.checked_problems(
        ReloadComponent::ExtensionShortcuts,
        result.shortcuts,
        explicit,
    ));
    for (component, problems) in result.rescans.checked {
        notices.extend(reload.checked_problems(
            ReloadComponent::ExtensionRescan(component),
            problems,
            explicit,
        ));
    }
    if explicit {
        notices.extend(result.details);
        notices.extend(result.rescans.details);
    }
    notices.extend(result.rescans.events);
    // Every event occurrence survives, including identical request-loss notices.
    notices.extend(result.events);
    notices
}

fn provider_reload_notices(
    reload: &mut crate::reload::ReloadSupervisor,
    catalog: crate::extensions::ProviderCatalogReport,
    explicit: bool,
) -> Vec<String> {
    let mut notices = if catalog.checked {
        reload.checked_problems(
            crate::reload::ReloadComponent::ProviderCatalog,
            catalog.problems,
            explicit,
        )
    } else if explicit {
        catalog.problems
    } else {
        Vec::new()
    };
    if explicit {
        notices.extend(catalog.details);
    }
    notices
}

fn reload_interruption_preview(app: &App) -> Vec<String> {
    let mut notices = Vec::new();
    let requests = app.executable_extensions.pending_host_request_count();
    if requests > 0 {
        notices.push(format!("reload: possible interruption of {requests} pending extension host request(s) if their process is replaced"));
    }
    let workers = active_subagent_workers(app).count;
    if workers > 0 {
        notices.push(format!("reload: possible interruption of {workers} active background worker(s); host replacement requires explicit detach consent"));
    }
    notices
}

/// The two caller-owned operations `crate::reexec` must not implement itself.
struct InteractiveReexecHooks<'a> {
    session: &'a Session,
    extensions: &'a mut crate::extensions::ExecutableExtensions,
    /// Durable session references of the workers this reload would detach. Empty
    /// on every path except the detach opt-in, where they are the records the
    /// replacement image reattaches from.
    worker_references: &'a [String],
    discarded_requests: Vec<String>,
}

#[async_trait::async_trait(?Send)]
impl crate::reexec::ReexecHooks for InteractiveReexecHooks<'_> {
    async fn make_session_durable(&mut self) -> anyhow::Result<()> {
        // Reuse the existing session store: every semantic record is
        // `sync_data`d before its append reports success
        // (`octet-agent/src/session_writer.rs`), so the durable head is made
        // recoverable by observing it rather than by writing a second store.
        // Re-opening replays the file with the same parser `--resume` will.
        let head = self.session.head_ref().cloned();
        let reopened = Session::open(self.session.path())?;
        anyhow::ensure!(
            reopened.head_ref() == head.as_ref(),
            "the durable session head does not match the live head"
        );
        // Worker records before the detach, in this hook and nowhere later: the
        // extension shutdown below is what detaches the running workers, so
        // every one of them must already resolve durably by now.
        verify_worker_records_are_durable(self.session, self.worker_references)
    }

    async fn shutdown_extensions(&mut self) -> anyhow::Result<()> {
        // `ExecutableExtensions::shutdown` releases the current session
        // binding and bounds each protocol plus the whole manager with its own
        // hard deadline; no extension child is inherited by the replacement
        // image. The caller can rebuild children after a failed exec.
        self.extensions.shutdown().await;
        self.discarded_requests
            .extend(self.extensions.discard_stale_host_requests());
        Ok(())
    }
}

/// The live delegated workers one `/reload` decision must account for.
struct ActiveWorkers {
    /// Every worker that still counts as live, whether or not its durable
    /// session reference is visible.
    count: usize,
    /// Durable session references (`agent-session:<sha256>`) of those workers.
    references: Vec<String>,
}

/// Live-state inputs for a re-exec decision taken by `/reload`.
///
/// `/reload` is dispatched only at the interactive idle boundary: the main
/// loop drops the active `Run` before queued actions or idle commands run,
/// idle shell escapes are awaited to completion before their outcome returns,
/// every session append is synchronous, and effect approvals are admitted only
/// inside the run pipeline. The five run-owned fields are therefore
/// observations of this boundary rather than guesses. Delegated workers
/// outlive the root run, so their count is read from live state.
fn live_reexec_inputs(workers: &ActiveWorkers) -> crate::reexec::LiveSafetyInputs {
    crate::reexec::LiveSafetyInputs {
        model_turn_active: false,
        tool_call_active: false,
        shell_child_active: false,
        pending_effect: false,
        session_persistence_in_flight: false,
        background_workers: workers.count,
    }
}

/// Live delegated workers reported by the octet-subagents presentation view.
///
/// Only read-only presentation state is consulted: no extension command is
/// dispatched and no worker is touched. A worker counts while it is not
/// terminal (`pending`, `active`, `running`, `degraded`), and its durable session
/// reference is collected when the extension disclosed one. A missing view means
/// no worker state is visible at all, which counts as zero.
fn active_subagent_workers(app: &App) -> ActiveWorkers {
    let mut workers = ActiveWorkers {
        count: 0,
        references: Vec::new(),
    };
    let Some(view) = app
        .executable_extensions
        .presentation_views()
        .into_iter()
        .find(|view| view.extension == "octet-subagents")
    else {
        return workers;
    };
    let Some(collection) = view.snapshot.collection else {
        return workers;
    };
    for node in &collection.nodes {
        if !matches!(
            node.state,
            octet_agent::ExtensionPresentationState::Pending
                | octet_agent::ExtensionPresentationState::Active
                | octet_agent::ExtensionPresentationState::Running
                | octet_agent::ExtensionPresentationState::Degraded
        ) {
            continue;
        }
        workers.count += 1;
        if let Some(reference) = node.references.iter().find(|reference| {
            reference.kind == octet_agent::ExtensionPresentationReferenceKind::Session
        }) {
            workers.references.push(reference.id.clone());
        }
    }
    workers
}

#[derive(Clone, Debug)]
struct InstalledExtensionChoice {
    name: String,
    label: String,
    description: String,
    enabled: bool,
    toggleable: bool,
}

fn installed_extension_choices(app: &App) -> anyhow::Result<Vec<InstalledExtensionChoice>> {
    let root = crate::extension_package::extensions_root()?;
    let installed = crate::extension_bundle::list_installed(&root)?;
    let summaries = app.executable_extensions.summaries();
    let explicitly_required = app
        .config
        .tools
        .explicit_names()
        .into_iter()
        .flatten()
        .collect::<std::collections::BTreeSet<_>>();
    let current_tools = app
        .agent
        .registered_tool_names()
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    Ok(installed
        .into_iter()
        .map(|bundle| {
            let summary = summaries.iter().find(|summary| summary.name == bundle.id);
            let enabled = app
                .config
                .enabled_extensions
                .iter()
                .any(|name| name == &bundle.id);
            let global_source = summary.is_some_and(|summary| {
                matches!(
                    summary.source,
                    octet_agent::extension_process::ExtensionSource::Global
                )
            });
            let unavailable_disable = summary.is_none() && enabled;
            let one_shot_trust_enable = !enabled
                && app
                    .config
                    .invocation_trusted_extensions
                    .iter()
                    .any(|name| name == &bundle.id);
            let alternate_source_trust_enable = !enabled
                && app.config.trusted_extensions.iter().any(|grant| {
                    grant
                        .split_once('@')
                        .is_some_and(|(name, _)| name == bundle.id.as_str())
                });
            let required_tools = summary
                .map(|summary| {
                    summary
                        .tools
                        .iter()
                        .filter(|tool| explicitly_required.contains(tool.as_str()))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let required_by_explicit_tools = enabled && !required_tools.is_empty();
            let colliding_tools = if enabled {
                Vec::new()
            } else {
                summary
                    .map(|summary| {
                        summary
                            .tools
                            .iter()
                            .filter(|tool| current_tools.contains(tool.as_str()))
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            let activation_authoritative = !app.config.extension_activation_overridden;
            let toggleable = (global_source || unavailable_disable)
                && activation_authoritative
                && !one_shot_trust_enable
                && !alternate_source_trust_enable
                && !required_by_explicit_tools
                && colliding_tools.is_empty();
            let marker = if enabled { "[x]" } else { "[ ]" };
            let description = match summary {
                Some(_) if !activation_authoritative => format!(
                    "installed {} · activation controlled by project, environment, or CLI; menu is read-only",
                    bundle.version
                ),
                Some(_) if one_shot_trust_enable => format!(
                    "installed {} · one-shot name trust can change source during rebuild; enable on a clean next launch",
                    bundle.version
                ),
                Some(_) if alternate_source_trust_enable => format!(
                    "installed {} · another manifest source has an exact trust grant; enable on a clean next launch",
                    bundle.version
                ),
                Some(_) if required_by_explicit_tools => format!(
                    "installed {} · required by explicit tool(s) {}; disable after removing that allowlist",
                    bundle.version,
                    required_tools.join(", ")
                ),
                Some(_) if !colliding_tools.is_empty() => format!(
                    "installed {} · tool name collision with {}; enable after resolving the active provider",
                    bundle.version,
                    colliding_tools.join(", ")
                ),
                Some(summary) if toggleable => format!(
                    "{} · {} · {} · API {}",
                    if summary.running {
                        "running"
                    } else {
                        "stopped"
                    },
                    if summary.trusted {
                        "trusted"
                    } else {
                        "untrusted"
                    },
                    bundle.version,
                    summary.api_version,
                ),
                Some(summary) => format!(
                    "installed {} · shadowed by {:?} source; toggle unavailable",
                    bundle.version, summary.source
                ),
                None if unavailable_disable && activation_authoritative => format!(
                    "installed {} · enabled but unavailable in discovery; Enter disables safely",
                    bundle.version
                ),
                None if unavailable_disable => format!(
                    "installed {} · enabled but unavailable; activation override makes the menu read-only",
                    bundle.version
                ),
                None => format!(
                    "installed {} · unavailable in current discovery; cannot enable (see /extensions status)",
                    bundle.version
                ),
            };
            InstalledExtensionChoice {
                name: bundle.id.clone(),
                label: format!("{marker} {}", bundle.id),
                description,
                enabled,
                toggleable,
            }
        })
        .collect())
}

const WEB_SEARCH_EXTENSION_NAME: &str = "octet-web-search";
const WEB_SEARCH_COMMAND_NAME: &str = "web-search";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebSearchMenuAction {
    Configured,
    Disable,
    Back,
}

fn web_search_menu_entries(allow_disable: bool) -> (Vec<String>, Vec<Option<String>>) {
    let mut items = vec![
        "Brave Search (recommended)".to_owned(),
        "SearXNG".to_owned(),
    ];
    let mut descriptions = vec![
        Some(
            "Hosted Brave Search API · setup asks for an API key and provides the signup link"
                .to_owned(),
        ),
        Some("Use an existing self-hosted or public SearXNG JSON endpoint".to_owned()),
    ];
    if allow_disable {
        items.push("Disable octet-web-search".to_owned());
        descriptions.push(Some("Stop the extension and remove its tools".to_owned()));
    }
    (items, descriptions)
}

async fn web_search_management_menu(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    allow_disable: bool,
) -> anyhow::Result<WebSearchMenuAction> {
    let (items, descriptions) = web_search_menu_entries(allow_disable);
    let Some(index) = extension_picker(
        shell,
        input,
        OrdinarySurfaceMetadata::with_purpose(
            "Select web search provider",
            "Choose a provider for web search or disable the extension",
        ),
        items,
        descriptions,
        0,
    )
    .await?
    else {
        return Ok(WebSearchMenuAction::Back);
    };
    if allow_disable && index == 2 {
        return Ok(WebSearchMenuAction::Disable);
    }
    let provider = if index == 0 { "brave" } else { "searxng" };
    let output = {
        let dialogs = app.executable_extensions.lifecycle_snapshot();
        let mut interaction = InteractiveExtensionConfirmations {
            shell,
            input,
            dialogs: &dialogs,
        };
        app.executable_extensions
            .execute_command_with_confirmation(
                WEB_SEARCH_COMMAND_NAME,
                vec!["setup".to_owned(), provider.to_owned()],
                &mut interaction,
            )
            .await
    };
    match output {
        Ok(Some(output)) => {
            if !output.trim().is_empty() {
                shell.notice(output);
            }
            shell.clear_error();
            request_extension_ui(shell, app);
        }
        Ok(None) => shell.error(
            "octet-web-search is running but its setup command is unavailable; see /extensions status"
                .to_owned(),
        ),
        Err(error) => shell.error(format!("web search provider was not changed: {error}")),
    }
    Ok(WebSearchMenuAction::Configured)
}

async fn extension_management_menu(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<App> {
    let mut selected = 0usize;
    loop {
        let choices = match installed_extension_choices(&app) {
            Ok(choices) => choices,
            Err(error) => {
                shell.error(format!(
                    "extension menu could not inspect installed bundles: {error}"
                ));
                return Ok(app);
            }
        };
        if choices.is_empty() {
            read_only_document(
                shell,
                input,
                "Extensions",
                "No executable extension bundles are installed.\n\nInstall one with `octet extension install <name>`.".into(),
            )
            .await?;
            return Ok(app);
        }
        let items = choices.iter().map(|choice| choice.label.clone()).collect();
        let descriptions = choices
            .iter()
            .map(|choice| Some(choice.description.clone()))
            .collect();
        let Some(index) = extension_picker(
            shell,
            input,
            OrdinarySurfaceMetadata::with_purpose(
                "Manage extensions",
                "Enter enables/disables; enabled web search opens provider setup",
            ),
            items,
            descriptions,
            selected,
        )
        .await?
        else {
            return Ok(app);
        };
        selected = index;
        let choice = &choices[index];
        if choice.enabled
            && choice.name == WEB_SEARCH_EXTENSION_NAME
            && app
                .executable_extensions
                .command_owner(WEB_SEARCH_COMMAND_NAME)
                .as_deref()
                == Some(WEB_SEARCH_EXTENSION_NAME)
        {
            match web_search_management_menu(&mut app, shell, input, choice.toggleable).await? {
                WebSearchMenuAction::Disable => {}
                WebSearchMenuAction::Configured | WebSearchMenuAction::Back => continue,
            }
        }
        if !choice.toggleable {
            shell.error(format!(
                "{} cannot be toggled from this menu: {}",
                choice.name, choice.description
            ));
            continue;
        }

        let authoritative = match crate::cli::extension_activation_menu_authoritative(&app.config) {
            Ok(authoritative) => authoritative,
            Err(error) => {
                shell.error(format!(
                    "{} was not changed: could not revalidate activation precedence: {error}",
                    choice.name
                ));
                continue;
            }
        };
        if !authoritative {
            shell.error(format!(
                "{} was not changed: project, environment, or CLI activation now makes the user config read-only",
                choice.name
            ));
            continue;
        }

        let enabled = !choice.enabled;
        let config_path = crate::cli::global_config_path();
        let before_config = config_path.as_deref().and_then(configuration_snapshot);
        let persisted = match crate::cli::persist_extension_enabled(&choice.name, enabled) {
            Ok(persisted) => persisted,
            Err(error) => {
                shell.error(format!(
                    "{} was not changed: could not update user configuration: {error}",
                    choice.name
                ));
                continue;
            }
        };
        app.config.enabled_extensions = persisted;
        app = match reload_resources(app, shell, input).await {
            Ok(app) => app,
            Err(error) => {
                let rollback = crate::cli::persist_extension_enabled(&choice.name, choice.enabled);
                return match rollback {
                    Ok(_) => Err(error.context(format!(
                        "{} runtime rebuild failed; the user-config activation change was rolled back",
                        choice.name
                    ))),
                    Err(rollback_error) => Err(error.context(format!(
                        "{} runtime rebuild failed and user-config rollback also failed: {rollback_error}",
                        choice.name
                    ))),
                };
            }
        };
        observe_configuration_commit(
            &mut app.executable_extensions,
            before_config,
            config_path.as_deref(),
        )
        .await;
        request_extension_ui(shell, &mut app);
        let summary = app
            .executable_extensions
            .summaries()
            .into_iter()
            .find(|summary| summary.name == choice.name);
        let detail = if enabled && summary.as_ref().is_some_and(|summary| !summary.trusted) {
            "; executable extensions require full access; safe mode keeps them stopped"
        } else {
            ""
        };
        shell.notice(format!(
            "{} {}{detail}",
            choice.name,
            if enabled { "enabled" } else { "disabled" }
        ));
        shell.clear_error();
        if enabled
            && choice.name == WEB_SEARCH_EXTENSION_NAME
            && summary
                .as_ref()
                .is_some_and(|summary| summary.running && summary.trusted)
            && app
                .executable_extensions
                .command_owner(WEB_SEARCH_COMMAND_NAME)
                .as_deref()
                == Some(WEB_SEARCH_EXTENSION_NAME)
        {
            let _ = web_search_management_menu(&mut app, shell, input, false).await?;
        }
    }
}

fn next_thinking_level(app: &App) -> anyhow::Result<ThinkingLevel> {
    let levels = supported_levels_with_subagents(&app.model, app.subagents_available());
    let current = level_from_reasoning(&app.reasoning, &app.model)?;
    let index = levels
        .iter()
        .position(|level| *level == current)
        .unwrap_or(0);
    levels
        .get((index + 1) % levels.len())
        .copied()
        .ok_or_else(|| anyhow::anyhow!("no thinking levels are available"))
}

async fn thinking_configuration_picker(
    app: &App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<Option<(ReasoningMode, ThinkingLevel)>> {
    let levels = supported_levels_with_subagents(&app.model, app.subagents_available());
    let codex_context = codex_context_surface(&app.model);
    let Some(level) = thinking_picker(shell, input, &levels, codex_context.as_ref()).await? else {
        return Ok(None);
    };
    Ok(Some((ReasoningMode::Standard, level)))
}

/// Codex context-window facts for the effort menu, or `None` on every other
/// route.
///
/// Entitlement is read from the same subscription credential the launch used,
/// and an unreadable or unparsable credential reports `false` so an unknown plan
/// never grants a raise above the deliberate cap.
fn codex_context_surface(model: &octet_ai::Model) -> Option<commands::CodexContextSurface> {
    if !commands::codex_responses_endpoint(model) {
        return None;
    }
    let store = crate::auth::codex::CredentialStore::new(crate::auth::codex::default_path());
    let entitled = crate::auth::codex::usable_subscription_claims(&store)
        .ok()
        .flatten()
        .and_then(|claims| claims.plan)
        .is_some_and(|plan| plan.uses_max_context_window());
    commands::CodexContextSurface::capture(model, entitled)
}

fn delegated_session_text(
    session: &Session,
    theme: &OctetTheme,
    width: u16,
    verbose_tools: bool,
) -> anyhow::Result<String> {
    crate::tui::view::delegated_session_document(session, theme, width, verbose_tools)
}

fn delegated_session_overlay_text(transcript: &str, theme: &OctetTheme) -> String {
    let dot = if theme.unicode() { "•" } else { "*" };
    format!(
        "{} {}\n{}\n\n{transcript}",
        theme.settled_event_dot("neutral", dot),
        theme.bold(&theme.fg("foreground", "Delegated worker transcript")),
        theme.dim("read-only · mutation remains owner-bound to agent_sessions"),
    )
}

#[derive(Clone, Debug)]
struct SubagentViewEntry {
    node_id: String,
    label: String,
    description: String,
    /// Declared generic presentation state, lowercased. The panel derives its
    /// group headings from this and never from the row text.
    state: String,
    session_reference: Option<String>,
    fallback_detail: String,
}

/// Display group for one declared generic presentation state.
///
/// Live work is expanded by default; terminal states collapse behind their
/// heading so a long-lived parent session cannot bury its running workers under
/// finished ones. `openall` is adding per-worker model/reasoning to the typed
/// presentation, so the group table stays separate from the row text.
fn subagent_group_for(state: &str) -> (&'static str, u8, bool) {
    match state {
        "running" => ("Running", 0, false),
        "pending" => ("Queued", 1, false),
        "degraded" => ("Blocked", 2, false),
        "active" => ("Active", 3, false),
        "succeeded" => ("Done", 4, true),
        "failed" => ("Failed", 5, true),
        "stopped" | "cancelled" => ("Stopped", 6, true),
        "unavailable" => ("Unavailable", 7, true),
        "empty" | "loading" => ("Pending state", 8, false),
        _ => ("Other", 9, true),
    }
}

/// Order entries into their display groups and derive the group index ranges.
///
/// Only ordering changes here: every worker keeps its stable node id and its
/// opaque session reference, and no session identifier enters the display text.
fn order_subagent_entries(
    entries: Vec<SubagentViewEntry>,
) -> (Vec<SubagentViewEntry>, Vec<crate::tui::view::SubagentGroup>) {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|index| {
        let (_, priority, _) = subagent_group_for(&entries[*index].state);
        (priority, *index)
    });
    let mut groups: Vec<crate::tui::view::SubagentGroup> = Vec::new();
    let mut ordered: Vec<SubagentViewEntry> = Vec::with_capacity(entries.len());
    for (position, source) in order.into_iter().enumerate() {
        let entry = &entries[source];
        let (label, _, collapsible) = subagent_group_for(&entry.state);
        match groups.last_mut() {
            Some(group) if group.label == label => group.indices.push(position),
            _ => groups.push(crate::tui::view::SubagentGroup {
                label: label.to_owned(),
                indices: vec![position],
                collapsible,
            }),
        }
        ordered.push(entry.clone());
    }
    (ordered, groups)
}

fn subagent_view_entries_from_presentation(
    view: crate::extensions::ExtensionPresentationView,
) -> Option<(String, Vec<SubagentViewEntry>)> {
    // Panel titles are surface names only. Per-worker states live on the group
    // rows and key affordances live in the picker's action footer, so the
    // status label's counts stay in status/telemetry/notices instead of the
    // header. A blank collection title falls back to the surface name rather
    // than inventing chrome.
    let collection = view.snapshot.collection?;
    let title = {
        let declared = collection.title.trim();
        if declared.is_empty() {
            "Subagents".to_owned()
        } else {
            declared.to_owned()
        }
    };
    let detail = collection.detail;
    let entries = collection
        .nodes
        .into_iter()
        .map(|node| {
            let state = format!("{:?}", node.state).to_lowercase();
            let description = node.secondary.clone().unwrap_or_else(|| state.clone());
            let session_reference = node
                .references
                .iter()
                .find(|reference| {
                    reference.kind == octet_agent::ExtensionPresentationReferenceKind::Session
                })
                .map(|reference| reference.id.clone());
            let fallback_detail = detail
                .as_ref()
                .filter(|detail| detail.node_id.as_deref() == Some(node.id.as_str()))
                .map(|detail| detail.body.clone())
                .unwrap_or_else(|| {
                    format!(
                        "{}\n\nState: {state}\nTranscript is not available yet.",
                        node.label
                    )
                });
            SubagentViewEntry {
                node_id: node.id,
                label: node.label,
                description,
                state,
                session_reference,
                fallback_detail,
            }
        })
        .collect();
    Some((title, entries))
}

fn subagent_view_entries(
    extensions: &crate::extensions::ExecutableExtensions,
) -> Option<(String, Vec<SubagentViewEntry>)> {
    let view = extensions
        .presentation_views()
        .into_iter()
        .find(|view| view.extension == "octet-subagents")?;
    subagent_view_entries_from_presentation(view)
}

fn subagent_picker_snapshot(
    title: &str,
    entries: &[SubagentViewEntry],
    notices: Vec<String>,
) -> SubagentPickerSnapshot {
    let (ordered, groups) = order_subagent_entries(entries.to_vec());
    SubagentPickerSnapshot {
        // Surface name only: the rows carry the per-worker states and the
        // picker chrome renders `picker_hints`/`panel_action_footer` for key
        // affordances. Nothing may silently re-add hints or counts here.
        title: title.to_owned(),
        items: ordered.iter().map(|entry| entry.label.clone()).collect(),
        descriptions: ordered
            .iter()
            .map(|entry| Some(entry.description.clone()))
            .collect(),
        node_ids: ordered.iter().map(|entry| entry.node_id.clone()).collect(),
        groups,
        notices,
    }
}

struct SubagentRefreshContext<'a> {
    extensions: &'a mut crate::extensions::ExecutableExtensions,
    last_error: Option<String>,
}

fn refresh_subagent_snapshot<'a, 'extensions>(
    context: &'a mut SubagentRefreshContext<'extensions>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = SubagentPickerSnapshot> + 'a>> {
    Box::pin(async move {
        let refresh_result = tokio::time::timeout(
            Duration::from_millis(750),
            context
                .extensions
                .execute_command_without_confirmation("subagents", vec!["status".into()]),
        )
        .await;
        let refresh_error = match refresh_result {
            Err(_) => Some(
                "subagent view live refresh timed out; showing the last accepted state".to_owned(),
            ),
            Ok(Ok(Some(output))) if output.contains("failed closed") => Some(
                "subagent view live refresh failed closed; showing the last accepted state"
                    .to_owned(),
            ),
            Ok(Ok(Some(_))) => None,
            Ok(Ok(None)) => Some(
                "subagent view live refresh is unavailable; showing the last accepted state"
                    .to_owned(),
            ),
            Ok(Err(error)) => Some(format!(
                "subagent view live refresh failed; showing the last accepted state: {error}"
            )),
        };
        let mut notices = context.extensions.drain_events();
        if refresh_error != context.last_error {
            if let Some(error) = refresh_error.as_ref() {
                notices.push(error.clone());
            }
            context.last_error = refresh_error;
        }
        match subagent_view_entries(context.extensions) {
            Some((title, entries)) => subagent_picker_snapshot(&title, &entries, notices),
            None => SubagentPickerSnapshot {
                title: "Subagents".into(),
                items: Vec::new(),
                descriptions: Vec::new(),
                node_ids: Vec::new(),
                groups: Vec::new(),
                notices,
            },
        }
    })
}

/// Whether an unknown-command text is the bare `/subagents` live view owned
/// by the octet-subagents extension. Only that view is safe to open mid-run:
/// it reads extension presentation state and never touches the running
/// agent session.
fn is_live_subagents_command(
    text: &str,
    extensions: &crate::extensions::ExecutableExtensions,
) -> bool {
    let Some(name) = text.strip_prefix('/') else {
        return false;
    };
    name.trim() == "subagents"
        && extensions.command_owner("subagents").as_deref() == Some("octet-subagents")
}

async fn subagents_view(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    command_output: String,
) -> anyhow::Result<()> {
    let unavailable = (!command_output.trim().is_empty()).then_some(command_output);
    if unavailable.as_deref().is_some_and(|output| {
        output.contains("failed closed")
            || output.contains(" Warning]")
            || output.contains(" Error]")
            || output.contains("confirmation denied")
            || output.contains("input cancelled")
            || output.contains("extension events because the consumer lagged")
            || output
                .lines()
                .any(|line| line.starts_with("warning:") || line.starts_with("error:"))
    }) {
        shell.notice(
            "subagent command reported extension diagnostics; showing the last accepted state (see /extensions status)",
        );
    }
    let mut selected_node_id = None::<String>;
    loop {
        let mut notices = app.executable_extensions.drain_events();
        notices.extend(app.synchronize_extension_provider_catalog());
        let Some((title, entries)) = subagent_view_entries(&app.executable_extensions) else {
            for notice in notices {
                shell.notice(notice);
            }
            read_only_document(
                shell,
                input,
                "Subagents",
                unavailable
                    .as_deref()
                    .unwrap_or("No subagents for this session.")
                    .to_owned(),
            )
            .await?;
            return Ok(());
        };
        if entries.is_empty() {
            for notice in notices {
                shell.notice(notice);
            }
            read_only_document(
                shell,
                input,
                "Subagents",
                unavailable
                    .as_deref()
                    .unwrap_or("No subagents for this session.")
                    .to_owned(),
            )
            .await?;
            return Ok(());
        }
        let initial_selected = selected_node_id
            .as_ref()
            .and_then(|id| entries.iter().position(|entry| &entry.node_id == id))
            .unwrap_or(0);
        let initial = subagent_picker_snapshot(&title, &entries, notices);
        let selected_id = {
            let mut refresh = SubagentRefreshContext {
                extensions: &mut app.executable_extensions,
                last_error: None,
            };
            subagent_picker(
                shell,
                input,
                initial,
                initial_selected,
                &mut refresh,
                refresh_subagent_snapshot,
            )
            .await?
        };
        let Some(selected_id) = selected_id else {
            return Ok(());
        };
        selected_node_id = Some(selected_id.clone());

        // Revalidate the stable node and typed reference against the newest
        // accepted presentation revision immediately before opening it.
        let mut notices = app.executable_extensions.drain_events();
        notices.extend(app.synchronize_extension_provider_catalog());
        for notice in notices {
            shell.notice(notice);
        }
        let Some((_, current_entries)) = subagent_view_entries(&app.executable_extensions) else {
            shell.error("subagent state changed before the transcript could open".into());
            continue;
        };
        let Some(entry) = current_entries
            .iter()
            .find(|entry| entry.node_id == selected_id)
        else {
            shell.error("the selected subagent is no longer available".into());
            continue;
        };
        let reference = entry.session_reference.clone();
        let principal = reference.as_deref().and_then(|reference| {
            app.executable_extensions
                .presentation_session_reference_principal(reference)
        });
        let node_id = entry.node_id.clone();
        let fallback_detail = entry.fallback_detail.clone();
        let theme = shell.theme();
        let verbose_tools = shell.verbose_tools();
        let initial_width = shell.read_only_document_width();
        let initial_text = if let (Some(principal), Some(reference)) =
            (principal.as_deref(), reference.as_deref())
        {
            match app
                .agent
                .open_delegated_session_reference(principal, reference)
            {
                Ok(Some(session)) => delegated_session_text(
                    &session,
                    &theme,
                    initial_width,
                    verbose_tools,
                )?,
                Ok(None) => format!(
                    "{}\n\nThe delegated transcript is no longer available for this parent session.",
                    fallback_detail
                ),
                Err(error) => format!(
                    "{}\n\nFailed to open the delegated transcript: {error}",
                    fallback_detail
                ),
            }
        } else {
            fallback_detail.clone()
        };
        let refresh = |width| {
            let current_fallback = subagent_view_entries(&app.executable_extensions)
                .and_then(|(_, entries)| {
                    entries
                        .into_iter()
                        .find(|candidate| candidate.node_id == node_id)
                })
                .map(|candidate| candidate.fallback_detail)
                .unwrap_or_else(|| fallback_detail.clone());
            let result: anyhow::Result<Option<String>> = if let (Some(principal), Some(reference)) =
                (principal.as_deref(), reference.as_deref())
            {
                match app
                    .agent
                    .open_delegated_session_reference(principal, reference)
                {
                    Ok(Some(session)) => delegated_session_text(
                        &session,
                        &theme,
                        width,
                        verbose_tools,
                    )
                    .map(Some),
                    Ok(None) => Ok(Some(format!(
                        "{}\n\nThe delegated transcript is no longer available for this parent session.",
                        current_fallback
                    ))),
                    Err(error) => Ok(Some(format!(
                        "{}\n\nFailed to open the delegated transcript: {error}",
                        current_fallback
                    ))),
                }
            } else {
                Ok(Some(current_fallback))
            };
            std::future::ready(result)
        };
        read_only_document_live_styled(
            shell,
            input,
            format!("{} · read-only transcript", entry.label),
            initial_text,
            refresh,
        )
        .await?;
        if shell.close_requested() {
            return Ok(());
        }
    }
}

fn restore_session_head(path: &std::path::Path, head: EntryId) -> anyhow::Result<()> {
    let mut session = Session::open(path)?;
    session.checkout(head)?;
    Ok(())
}

/// Which additive lifecycle notification one reconfiguration owes extensions.
#[derive(Clone, Copy)]
enum HostNotification {
    ModelSelected,
    ReasoningSelected,
    SessionInfoChanged,
}

impl HostNotification {
    /// Classify one reconfiguration before it consumes its payload.
    fn of(reconfig: &Reconfig) -> Self {
        match reconfig {
            Reconfig::Model(_) => Self::ModelSelected,
            Reconfig::Thinking(_) | Reconfig::ThinkingMode { .. } => Self::ReasoningSelected,
            Reconfig::NewSession | Reconfig::Resume(_) => Self::SessionInfoChanged,
        }
    }

    /// Publish the notification to the negotiated v2 subscribers.
    fn publish(self, extensions: &mut crate::extensions::ExecutableExtensions) {
        match self {
            Self::ModelSelected => {
                extensions.notify_model_selected_all();
                extensions.notify_session_info_changed_all();
            }
            Self::ReasoningSelected => extensions.notify_reasoning_selected_all(),
            Self::SessionInfoChanged => extensions.notify_session_info_changed_all(),
        }
    }
}

/// Apply a user thinking selection before saving its startup preference. Qualified
/// Responses controls can reject stateful Ultra/V2 transitions even when the
/// model advertises the requested choice.
async fn select_thinking<S>(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut S,
    reasoning: ReasoningConfig,
    picker: Option<(ReasoningMode, ThinkingLevel)>,
) -> anyhow::Result<App>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mode = picker.map(|(mode, _)| mode);
    // Preserve the portable picker default (e.g. high, not a model-specific
    // token budget); slash commands retain their effective reasoning label.
    let preference = picker.map_or_else(
        || reasoning_label(&reasoning),
        |(_, level)| level.label().to_owned(),
    );
    let qualified = app.model.responses_features().reasoning_effort_updates;
    if qualified {
        if let Err(error) = app.agent.set_reasoning(reasoning.clone()) {
            shell.error(format!("thinking unchanged: {error}"));
            return Ok(app);
        }
        app.reasoning = app.agent.reasoning().clone();
    }
    if let Err(error) = persist_configuration(Some(&mut app.executable_extensions), || {
        crate::cli::persist_reasoning(&preference)
    })
    .await
    {
        shell.error(format!("failed to save thinking preference: {error}"));
    }
    if let Some(mode) = mode {
        if let Err(error) = persist_configuration(Some(&mut app.executable_extensions), || {
            crate::cli::persist_reasoning_mode(mode)
        })
        .await
        {
            shell.error(format!("failed to save reasoning mode preference: {error}"));
        }
    }
    if qualified && mode.is_none_or(|mode| mode == app.reasoning_mode) {
        update_status(shell, &app);
        app.executable_extensions.notify_reasoning_selected_all();
        return Ok(app);
    }
    let reconfig = match mode {
        Some(mode) => Reconfig::ThinkingMode { mode, reasoning },
        None => Reconfig::Thinking(reasoning),
    };
    transition(app, shell, input, reconfig).await
}

async fn transition<S>(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut S,
    reconfig: Reconfig,
) -> anyhow::Result<App>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let direct_reasoning = match &reconfig {
        Reconfig::Thinking(reasoning) => Some(reasoning),
        Reconfig::ThinkingMode { mode, reasoning } if *mode == app.reasoning_mode => Some(reasoning),
        _ => None,
    };
    if let Some(reasoning) = direct_reasoning {
        if app.model.responses_features().reasoning_effort_updates {
            match app.agent.set_reasoning(reasoning.clone()) {
                Ok(()) => {
                    app.reasoning = app.agent.reasoning().clone();
                    update_status(shell, &app);
                    app.executable_extensions.notify_reasoning_selected_all();
                }
                Err(error) => shell.error(format!("thinking unchanged: {error}")),
            }
            return Ok(app);
        }
    }
    let _diagnostics = crate::output::defer_tui_diagnostics();
    let had_fast = app.agent.service_tier().is_some();
    let host_notification = HostNotification::of(&reconfig);
    // A transition may replace the active durable session (`/resume`, `/new`,
    // `/fork`, `/clone`). Herdr's pane reference must follow it, exactly as the
    // Pi integration refreshes its session reference on every `agent_start`.
    let previous_session_id = herdr_session_id(&app);
    let start_source = herdr_reconfig_source(&reconfig);
    let mut app = run_blocking_lifecycle(shell, input, "reconfiguring…", move || {
        apply_reconfig(app, reconfig)
    })
    .await?;
    shell.hydrate(app.agent.session())?;
    let session_id = herdr_session_id(&app);
    if session_id != previous_session_id {
        shell.herdr_session_changed(session_id, start_source, herdr_launch_scope(&app));
    }
    // Model and thinking changes are acknowledged by stable chrome, not a
    // duplicate transcript notice. Session-operation notices remain caller-owned.
    update_status(shell, &app);
    if had_fast && app.agent.service_tier().is_none() {
        shell.notice("Fast mode: off after the model/session change; use /fast on on a supported route to enable it again");
    }
    app.executable_extensions
        .activate_session_lifecycle_driver();
    host_notification.publish(&mut app.executable_extensions);
    Ok(app)
}

/// The opaque, path-free session id `octet --resume <id>` accepts, when the
/// active session has a usable durable identity.
///
/// Only the id is ever reported: Herdr's Pi integration also sends an
/// `agent_session_path`, and octet deliberately does not — its transcript path
/// never leaves the process.
fn herdr_session_id(app: &App) -> Option<String> {
    terminal_goal_session_id(app.agent.session()).ok()
}

/// A restored lookup needs the same store root and workspace as the live App;
/// a session id alone is only unique within that pair.
fn herdr_launch_scope(app: &App) -> crate::herdr::restore::LaunchScope {
    crate::herdr::restore::LaunchScope::new(
        &app.config.session_dir,
        &app.config.workspace,
        &app.config.invocation_cwd,
    )
}

/// The Pi-style `session_start` reason for a launch selection.
fn herdr_startup_source(selector: &ResumeSelector) -> Option<&'static str> {
    match selector {
        ResumeSelector::New => Some("startup"),
        ResumeSelector::Continue | ResumeSelector::Resume(_) => Some("resume"),
        ResumeSelector::Fork(_) => Some("fork"),
    }
}

/// The Pi-style `session_start` reason for an in-process session transition.
fn herdr_reconfig_source(reconfig: &Reconfig) -> Option<&'static str> {
    match reconfig {
        Reconfig::NewSession => Some("startup"),
        Reconfig::Resume(_) => Some("resume"),
        Reconfig::Model(_) | Reconfig::Thinking(_) | Reconfig::ThinkingMode { .. } => None,
    }
}

fn bounded_extension_session_id(session_id: String) -> anyhow::Result<String> {
    if session_id.len() > MAX_JSON_RPC_ID_BYTES {
        anyhow::bail!("session identifier exceeds the API 0.3 result bound");
    }
    Ok(session_id)
}

fn session_id_for_path(path: &Path) -> anyhow::Result<String> {
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("session path has no valid identifier"))?;
    bounded_extension_session_id(session_id)
}

async fn create_extension_session(
    app: &App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<String> {
    let path = app.sessions.new_path(&crate::modes::timestamp());
    run_blocking_lifecycle(shell, input, "creating session…", move || {
        let mut prepared = None;
        let session = open_launch_session(&mut prepared, SessionSelection::CreateNew(path))?;
        bounded_extension_session_id(terminal_goal_session_id(&session)?)
    })
    .await
}

async fn fork_extension_session(
    app: &App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<String> {
    let sessions = app.sessions.clone();
    let source_path = app.agent.session().path().to_owned();
    let destination = sessions.new_path(&crate::modes::timestamp());
    let checkpoint = app.agent.session().head();
    run_blocking_lifecycle(shell, input, "forking session…", move || {
        let path = fork_active_session(&sessions, &source_path, destination, checkpoint.as_ref())?;
        session_id_for_path(&path)
    })
    .await
}

async fn open_extension_session(
    path: PathBuf,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    label: &'static str,
) -> anyhow::Result<Session> {
    run_blocking_lifecycle(shell, input, label, move || {
        let mut prepared = None;
        open_launch_session(&mut prepared, SessionSelection::OpenExisting(path))
    })
    .await
}

fn replace_extension_active_session(app: &mut App, session: Session) -> anyhow::Result<String> {
    let session_id = bounded_extension_session_id(terminal_goal_session_id(&session)?)?;
    app.agent.replace_session_at_idle(session)?;
    app.goal_session_id = session_id.clone();
    let goal_store: Arc<dyn octet_agent::GoalStore> = app.goal_store.clone();
    app.goal_driver = octet_agent::GoalDriver::new(goal_store, session_id.clone());
    app.executable_extensions.transition_active_session(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    Ok(session_id)
}

async fn switch_extension_session(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    request: &ExtensionSessionLifecycleRequest,
    session_id: String,
) -> anyhow::Result<Option<String>> {
    let path = app.sessions.path_by_id(&session_id)?;
    let session = open_extension_session(path, shell, input, "switching session…").await?;
    if request.is_cancelled() {
        return Ok(None);
    }
    replace_extension_active_session(app, session).map(Some)
}

async fn reload_extension_session(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    request: &ExtensionSessionLifecycleRequest,
) -> anyhow::Result<Option<String>> {
    let path = app.agent.session().path().to_owned();
    let session = open_extension_session(path, shell, input, "reloading session…").await?;
    if request.is_cancelled() {
        return Ok(None);
    }
    replace_extension_active_session(app, session).map(Some)
}

async fn execute_extension_session_lifecycle(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    request: ExtensionSessionLifecycleRequest,
) -> bool {
    if request.is_cancelled() {
        return false;
    }
    let operation = request.operation().clone();
    let mut result = match operation.clone() {
        ExtensionSessionLifecycleOperation::Create => {
            create_extension_session(app, shell, input).await.map(Some)
        }
        ExtensionSessionLifecycleOperation::Fork => {
            fork_extension_session(app, shell, input).await.map(Some)
        }
        ExtensionSessionLifecycleOperation::Switch { session_id } => {
            switch_extension_session(app, shell, input, &request, session_id).await
        }
        ExtensionSessionLifecycleOperation::Reload => {
            reload_extension_session(app, shell, input, &request).await
        }
    };
    let active_session_operation = match &operation {
        ExtensionSessionLifecycleOperation::Switch { .. }
        | ExtensionSessionLifecycleOperation::Reload => true,
        ExtensionSessionLifecycleOperation::Create | ExtensionSessionLifecycleOperation::Fork => {
            false
        }
    };
    let active_session_replaced =
        active_session_operation && result.as_ref().is_ok_and(|session_id| session_id.is_some());
    if active_session_replaced {
        // A cancelled response must not leave the visible transcript bound to
        // the discarded session, so hydrate before observing cancellation.
        if let Err(error) = shell.hydrate(app.agent.session()) {
            result = Err(error);
        }
    }
    if request.is_cancelled() {
        return active_session_replaced;
    }
    let method = match operation {
        ExtensionSessionLifecycleOperation::Create => "create",
        ExtensionSessionLifecycleOperation::Fork => "fork",
        ExtensionSessionLifecycleOperation::Switch { .. } => "switch",
        ExtensionSessionLifecycleOperation::Reload => "reload",
    };
    match result {
        Ok(Some(session_id)) => {
            request.respond(Ok(session_id));
            request_extension_ui(shell, app);
            update_status(shell, app);
            shell.notice(format!("extension session {method} completed"));
        }
        Ok(None) => return false,
        Err(error) => {
            request.respond(Err(ExtensionSessionLifecycleError::Failed));
            shell.error(format!("extension session {method} failed: {error:#}"));
        }
    }
    shell.render();
    active_session_replaced
}

async fn pick_session_path(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    store: &crate::session_store::SessionStore,
    current_session_path: Option<&std::path::Path>,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    let store_for_listing = store.clone();
    let sessions = run_blocking_lifecycle(shell, input, "discovering sessions…", move || {
        Ok(store_for_listing.list())
    })
    .await?;
    session_picker(shell, input, &sessions, store, current_session_path).await
}

fn active_fork_messages(session: &Session) -> Vec<crate::tui::view::ForkMessage> {
    let mut newest_first = Vec::new();
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(id) else {
            break;
        };
        newest_first.push(entry);
        cursor = entry.parent.as_ref();
    }
    newest_first.reverse();

    let mut messages = newest_first
        .into_iter()
        .filter_map(|entry| {
            let octet_agent::EntryValue::Message(octet_ai::Message::User(user)) = &entry.value
            else {
                return None;
            };
            let text = user
                .content
                .iter()
                .filter_map(|part| match part {
                    octet_ai::UserPart::Text(text) => Some(text.as_str()),
                    octet_ai::UserPart::Media(_) | octet_ai::UserPart::ToolResult(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then(|| crate::tui::view::ForkMessage {
                entry_id: entry.id.0.clone(),
                text,
                whole_conversation: false,
            })
        })
        .collect::<Vec<_>>();

    if !messages.is_empty() {
        if let Some(head) = session.head_ref() {
            messages.push(crate::tui::view::ForkMessage {
                entry_id: head.0.clone(),
                text: String::new(),
                whole_conversation: true,
            });
        }
    }
    messages
}

fn fork_active_session(
    sessions: &crate::session_store::SessionStore,
    source_path: &std::path::Path,
    destination: std::path::PathBuf,
    checkpoint: Option<&EntryId>,
) -> anyhow::Result<std::path::PathBuf> {
    let source = Session::open_read_only(source_path).with_context(|| {
        format!(
            "could not open current session for forking: {}",
            source_path.display()
        )
    })?;
    let source_id = source
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow::anyhow!("current session has no valid id"))?;
    let forked = source.fork_to(destination.clone(), checkpoint)?;
    drop(forked);
    if let Some(checkpoint) = checkpoint {
        let destination_id = destination
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow::anyhow!("forked session has no valid id"))?;
        if let Err(error) =
            sessions.set_fork_provenance(destination_id, source_id, checkpoint.0.as_str())
        {
            let _ = std::fs::remove_file(&destination);
            return Err(error);
        }
    }
    Ok(destination)
}

async fn fork_active_session_lifecycle(
    app: &App,
    checkpoint: EntryId,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<std::path::PathBuf> {
    let sessions = app.sessions.clone();
    let source_path = app.agent.session().path().to_owned();
    let destination = sessions.new_path(&crate::modes::timestamp());
    run_blocking_lifecycle(shell, input, "forking session…", move || {
        fork_active_session(&sessions, &source_path, destination, Some(&checkpoint))
    })
    .await
}

async fn fork_session(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<App> {
    let messages = active_fork_messages(app.agent.session());
    if messages.is_empty() {
        shell.notice("No messages to fork from");
        return Ok(app);
    }
    let Some((entry_id, text)) = message_picker(shell, input, messages).await? else {
        return Ok(app);
    };
    let checkpoint = EntryId(entry_id);
    let destination = fork_active_session_lifecycle(&app, checkpoint, shell, input).await?;
    app = transition(app, shell, input, Reconfig::Resume(destination)).await?;
    shell.prefill_editor(text);
    shell.notice("Forked to new session");
    Ok(app)
}

async fn clone_session(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<App> {
    let Some(head) = app.agent.session().head() else {
        shell.notice("Nothing to clone yet");
        return Ok(app);
    };
    let destination = fork_active_session_lifecycle(&app, head, shell, input).await?;
    app = transition(app, shell, input, Reconfig::Resume(destination)).await?;
    shell.clear_editor();
    shell.notice("Cloned to new session");
    Ok(app)
}

#[allow(clippy::too_many_arguments)]
async fn apply_pending_actions(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    pending_actions: &mut VecDeque<PendingIdleAction>,
    goal_deadline: &mut Option<Instant>,
    // Reborrowed once per idle pass: the request must survive the loop without
    // handing the controller away.
    mut reexec: Option<&mut crate::reexec::ReexecController>,
    pending_reexec: &mut Option<crate::reexec::ReexecPlan>,
    reload: &mut crate::reload::ReloadSupervisor,
) -> anyhow::Result<App> {
    while !shell.close_requested() {
        let Some(action) = pending_actions.pop_front() else {
            break;
        };
        match action {
            PendingIdleAction::Login(provider) => match validate_provider(provider.as_deref()) {
                Ok("codex") => login_codex(&mut app, shell).await?,
                Ok("custom") => login_custom(shell)?,
                Ok(_) => unreachable!(),
                Err(e) => shell.error(e.to_string()),
            },
            PendingIdleAction::Logout(provider) => match validate_provider(provider.as_deref()) {
                Ok("codex") => {
                    app = logout_codex(app, shell, input).await?;
                }
                Ok("custom") => {
                    app = logout_custom(app, shell, input).await?;
                }
                Ok(_) => unreachable!(),
                Err(e) => shell.error(e.to_string()),
            },
            PendingIdleAction::Fast(enabled) => {
                apply_fast_command(&mut app, shell, Some(enabled));
            }
            PendingIdleAction::ChangeModel(id) => {
                app = transition(app, shell, input, Reconfig::Model(id)).await?;
            }
            PendingIdleAction::ChangeThinking(reasoning) => {
                app = select_thinking(app, shell, input, reasoning, None).await?;
            }
            PendingIdleAction::ChangeThinkingLevel(level) => {
                let reasoning = requested_thinking_to_reasoning(
                    level,
                    &app.model,
                    app.subagents_available(),
                )?;
                app = select_thinking(app, shell, input, reasoning, None).await?;
            }
            PendingIdleAction::CycleThinking => {
                let level = next_thinking_level(&app)?;
                let reasoning = requested_thinking_to_reasoning(
                    level,
                    &app.model,
                    app.subagents_available(),
                )?;
                app = select_thinking(app, shell, input, reasoning, None).await?;
            }
            PendingIdleAction::NewSession => {
                app = transition(app, shell, input, Reconfig::NewSession).await?;
                shell.notice("queued new session created");
            }
            PendingIdleAction::ResumeSession(Some(id)) => {
                let path = app.sessions.path_by_id(&id)?;
                app = transition(app, shell, input, Reconfig::Resume(path)).await?;
                shell.notice("queued session resumed");
            }
            PendingIdleAction::ResumeSession(None) => {
                if let Some(path) = pick_session_path(
                    shell,
                    input,
                    &app.sessions,
                    Some(app.agent.session().path()),
                )
                .await?
                {
                    app = transition(app, shell, input, Reconfig::Resume(path)).await?;
                    shell.notice("queued session resumed");
                }
            }
            PendingIdleAction::Fork => {
                app = fork_session(app, shell, input).await?;
            }
            PendingIdleAction::Clone => {
                app = clone_session(app, shell, input).await?;
            }
            PendingIdleAction::Compact => {
                compact_interactively(&mut app, shell, input, false, None).await;
            }
            PendingIdleAction::CompactWithInstructions(instructions) => {
                compact_interactively(&mut app, shell, input, false, Some(&instructions)).await;
            }
            PendingIdleAction::AutoCompact(setting) => {
                configure_auto_compaction(&mut app, shell, setting)?;
                update_status(shell, &app);
            }
            PendingIdleAction::ShowContext => {
                shell.show_context_report(crate::tui::context::ContextReport::capture(&app, &[]));
            }
            PendingIdleAction::ReloadResources => {
                let (next, plan) = reload_resources_with_reexec(
                    app,
                    shell,
                    input,
                    reexec.as_deref_mut(),
                    HostPass::ResourcesOnly,
                    reload,
                )
                .await?;
                shell.notice("resources reloaded");
                app = next;
                if let Some(plan) = plan {
                    *pending_reexec = Some(plan);
                    break;
                }
            }
            PendingIdleAction::PickModel => {
                if let Some(model) = open_model_picker(&mut app, shell, input).await? {
                    app = transition(app, shell, input, Reconfig::Model(model)).await?;
                }
            }
            PendingIdleAction::PickThinking => {
                if let Some((mode, level)) =
                    thinking_configuration_picker(&app, shell, input).await?
                {
                    let reasoning = requested_thinking_to_reasoning(
                        level,
                        &app.model,
                        app.subagents_available(),
                    )?;
                    app = select_thinking(
                        app,
                        shell,
                        input,
                        reasoning,
                        Some((mode, level)),
                    )
                    .await?;
                }
            }
            PendingIdleAction::Skills(sub) => {
                if sub == commands::SkillsSubcommand::Reload {
                    app = reload_resources(app, shell, input).await?;
                    shell.notice("queued skills and prompt templates reload applied");
                } else {
                    execute_skills_command(&mut app, shell, sub).await?;
                }
            }
            PendingIdleAction::Extensions(sub) => {
                // Reuse the idle dispatcher verbatim so the menu, reload, and
                // action semantics never diverge from `/extensions` at idle.
                match run_idle_command(
                    app,
                    shell,
                    input,
                    Command::Extensions(sub),
                    goal_deadline,
                    None,
                    reload,
                )
                .await?
                {
                    IdleCommandOutcome::Continue(next)
                    | IdleCommandOutcome::Quit(next)
                    | IdleCommandOutcome::Submit { app: next, .. }
                    // `/extensions` dispatch never plans a re-exec; the arm
                    // only keeps the match exhaustive.
                    | IdleCommandOutcome::Reexec { app: next, .. } => app = *next,
                }
            }
            PendingIdleAction::Settings(sub) => {
                // Reuse the idle dispatcher verbatim so a queued mutation takes
                // exactly the path an immediate `/settings` would.
                match run_idle_command(
                    app,
                    shell,
                    input,
                    Command::Settings(sub),
                    goal_deadline,
                    None,
                    reload,
                )
                .await?
                {
                    IdleCommandOutcome::Continue(next)
                    | IdleCommandOutcome::Quit(next)
                    | IdleCommandOutcome::Submit { app: next, .. }
                    // `/settings` dispatch never plans a re-exec; the arm only
                    // keeps the match exhaustive.
                    | IdleCommandOutcome::Reexec { app: next, .. } => app = *next,
                }
            }
            PendingIdleAction::ScopedModels(sub) => {
                apply_scoped_models_command(&mut app, shell, sub).await;
                shell.notice("model scope change applied at the idle boundary");
            }
            PendingIdleAction::RecordShellEscape(record) => {
                if let Err(error) = record_shell_escape(&mut app, &record) {
                    shell.error(format!("shell result could not be recorded: {error}"));
                } else {
                    shell.notice(if record.excluded() {
                        "shell result recorded; excluded from model context".to_owned()
                    } else {
                        "shell result recorded; added to model context".to_owned()
                    });
                }
            }
        }
        request_extension_ui(shell, &mut app);
        shell.render();
    }
    Ok(app)
}

async fn execute_skills_command(
    app: &mut App,
    shell: &mut InteractiveShell,
    sub: commands::SkillsSubcommand,
) -> anyhow::Result<()> {
    match sub {
        commands::SkillsSubcommand::List => {
            let mut text = String::from("Discovered skills:\n");
            let descriptors = app.skills.descriptors();
            if descriptors.is_empty() {
                text.push_str("  (none found)");
            } else {
                for desc in descriptors.iter() {
                    text.push_str(&format!(
                        "  - {} (v{}) [trust: {:?}]\n    {}\n",
                        desc.id,
                        desc.version.as_deref().unwrap_or("1.0"),
                        desc.trust,
                        desc.description
                    ));
                }
            }
            let diagnostics = app.skills.diagnostics();
            if !diagnostics.is_empty() {
                const SHOWN_DIAGNOSTICS: usize = 20;
                text.push_str("\nDiagnostics:\n");
                for diagnostic in diagnostics.iter().take(SHOWN_DIAGNOSTICS) {
                    text.push_str(&format!(
                        "  - {}\n    {}\n",
                        diagnostic.path.display(),
                        diagnostic.message
                    ));
                }
                if diagnostics.len() > SHOWN_DIAGNOSTICS {
                    text.push_str(&format!(
                        "  ... and {} more; narrow the configured skill directories\n",
                        diagnostics.len() - SHOWN_DIAGNOSTICS
                    ));
                }
            }
            shell.show_overlay_text(text);
        }
        commands::SkillsSubcommand::Show(id) => {
            let descriptors = app.skills.descriptors();
            if let Some(desc) = descriptors.iter().find(|d| d.id == id) {
                let text = format!(
                    "Skill: {}\nName: {}\nVersion: {}\nTrust Level: {:?}\nRequired Tools: {:?}\nTags: {:?}\n\nDescription:\n{}",
                    desc.id,
                    desc.name,
                    desc.version.as_deref().unwrap_or("1.0"),
                    desc.trust,
                    desc.required_tools,
                    desc.tags,
                    desc.description
                );
                shell.show_overlay_text(text);
            } else {
                shell.error(format!("Skill '{}' not found", id));
            }
        }
        commands::SkillsSubcommand::Active => {
            let mut text = String::from("Active skills:\n");
            if let Some(head_id) = app.agent.session().head() {
                match app.agent.session().resolve_active_skills(&head_id) {
                    Ok(state) => {
                        if state.active_skills.is_empty() {
                            text.push_str("  (none active)");
                        } else {
                            for skill in state.active_skills {
                                text.push_str(&format!(
                                    "  - {} (activation: {})\n",
                                    skill.descriptor.id, skill.activation_id.0
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        text.push_str(&format!("  (failed to resolve: {e})"));
                    }
                }
            } else {
                text.push_str("  (empty session)");
            }
            shell.show_overlay_text(text);
        }
        commands::SkillsSubcommand::Search(query) => {
            let results = app.skills.find(&octet_agent::skills::SkillQuery {
                text: query.clone(),
            });
            let mut text = format!("Skills matching {query:?}:\n");
            if results.is_empty() {
                text.push_str("  (none found)");
            } else {
                for result in results {
                    text.push_str(&format!(
                        "  - {} · {}\n    {}\n",
                        result.descriptor.id, result.descriptor.name, result.descriptor.description
                    ));
                }
            }
            shell.show_overlay_text(text);
        }
        commands::SkillsSubcommand::Load(id) => match app.skills.load(&id) {
            Ok(_) => {
                shell.restore_composed(ComposedInput::from_text(format!("/skill:{id}")));
                shell.notice(format!("skill invocation /skill:{id} is ready to submit"));
            }
            Err(error) => shell.error(format!("Failed to invoke skill '{id}': {error}")),
        },
        commands::SkillsSubcommand::Reload => {
            shell.error("skill reload must run at an idle resource boundary".into());
        }
        commands::SkillsSubcommand::Off(id) => {
            let mut act_id_opt = None;
            if let Some(head_id) = app.agent.session().head() {
                if let Ok(state) = app.agent.session().resolve_active_skills(&head_id) {
                    if let Some(skill) = state.active_skills.iter().find(|s| s.descriptor.id == id)
                    {
                        act_id_opt = Some(skill.activation_id.clone());
                    }
                }
            }
            if let Some(act_id) = act_id_opt {
                let event = octet_agent::session::EntryValue::SkillDeactivated {
                    activation_id: act_id.clone(),
                    skill_id: id.clone(),
                };
                match app.agent.session_mut().append(event) {
                    Ok(_) => {
                        shell.notice(format!(
                            "Skill '{}' deactivated (unloaded activation: {})",
                            id, act_id.0
                        ));
                    }
                    Err(e) => {
                        shell.error(format!("Failed to record skill deactivation: {e}"));
                    }
                }
            } else {
                shell.error(format!(
                    "Skill '{}' is not currently active on this branch",
                    id
                ));
            }
        }
    }
    Ok(())
}

enum IdleCommandOutcome {
    Continue(Box<App>),
    Submit {
        app: Box<App>,
        input: ComposedInput,
    },
    Quit(Box<App>),
    /// A validated re-exec: the caller leaves the TUI, then `plan.exec()`.
    Reexec {
        app: Box<App>,
        plan: crate::reexec::ReexecPlan,
    },
}

/// How one interactive process lifetime ended.
enum InteractiveExit {
    /// No process replacement was requested.
    Finished,
    /// The terminal was left and the wrapper above must `exec` the plan.
    Reexec(crate::reexec::ReexecPlan),
}

fn prompt_templates_text(app: &App) -> String {
    let descriptors = app.prompts.descriptors();
    let mut text = String::from("Prompt templates:\n");
    if descriptors.is_empty() {
        text.push_str("  (none found under ~/.octet/prompts, .octet/prompts, or explicit paths)");
    } else {
        for descriptor in descriptors.iter() {
            let hint = descriptor
                .argument_hint
                .as_deref()
                .map(|hint| format!(" {hint}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  /{}{hint}\n    {} · {:?}\n",
                descriptor.name, descriptor.description, descriptor.trust
            ));
        }
    }
    let diagnostics = app.prompts.diagnostics();
    if !diagnostics.is_empty() {
        text.push_str("\nDiagnostics:\n");
        for diagnostic in diagnostics.iter() {
            text.push_str(&format!(
                "  - {}: {}\n",
                diagnostic.path.display(),
                diagnostic.message
            ));
        }
    }
    text
}

fn split_prompt_invocation(invocation: &str) -> Option<(&str, &str)> {
    let invocation = invocation.trim().trim_start_matches('/');
    let end = invocation
        .find(char::is_whitespace)
        .unwrap_or(invocation.len());
    let name = &invocation[..end];
    (!name.is_empty()).then(|| (name, invocation[end..].trim_start()))
}

fn expand_prompt_invocation(
    app: &mut App,
    invocation: &str,
    require_match: bool,
    selection: Option<&str>,
) -> anyhow::Result<Option<RenderedPrompt>> {
    let Some((name, arguments)) = split_prompt_invocation(invocation) else {
        return Ok(None);
    };
    if require_match && !app.prompts.contains(name) {
        return Ok(None);
    }
    let prompts = app.prompts.clone();
    let workspace = app.config.workspace.clone();
    render_and_record(
        &prompts,
        app.agent.session_mut(),
        &workspace,
        name,
        arguments,
        selection,
    )
    .map(Some)
    .map_err(Into::into)
}

/// Prepare the model-picker surface for a launch whose readiness plan was
/// narrowed to the selected route.
///
/// Startup latency defers the other configured providers, so a surface that
/// enumerates every route must complete the catalog first: the `/model` picker
/// would otherwise list only the launch's own provider even though other
/// credentials are present. Completion is idempotent, and a failure keeps the
/// narrowed catalog and reports it instead of hiding the missing providers.
/// Write the hidden `/debug` report and report the owner-private destination.
///
/// The frame is asked of the renderer; the messages come from the active branch
/// so the log matches exactly what the provider receives. A failure is surfaced,
/// never swallowed: a diagnostics surface that silently does nothing is useless.
async fn write_debug_report(
    shell: &mut InteractiveShell,
    session: &Session,
    terminal: Option<(u16, u16)>,
    rendered: Option<Vec<String>>,
) {
    let Some(path) = crate::cli::debug_log_path() else {
        shell.error("/debug unavailable: user home directory is unavailable".to_owned());
        return;
    };
    write_debug_report_at(shell, &path, session, terminal, rendered).await;
}

/// The same report against an explicit destination, so the writer is testable
/// without touching the operator's real `~/.octet` directory.
async fn write_debug_report_at(
    shell: &mut InteractiveShell,
    path: &std::path::Path,
    session: &Session,
    terminal: Option<(u16, u16)>,
    rendered: Option<Vec<String>>,
) {
    let messages = session.context().unwrap_or_default();
    let report = commands::debug_report_text(terminal, rendered.as_deref(), &messages);
    if let Err(error) = crate::auth::write_private_atomic(path, report.as_bytes(), ".octet-debug-")
    {
        shell.error(format!(
            "/debug could not write {}: {error}",
            path.display()
        ));
        return;
    }
    shell.notice(format!("debug log written: {}", path.display()));
}

async fn open_model_picker<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<Option<ModelId>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    // The launch catalog is already usable. Only the deferred fleet inventory
    // belongs on a worker; App, its extension projection, and the terminal
    // remain owned by this idle event loop.
    let pending = (!app.readiness.is_fleet()).then(|| {
        let offline = app.config.offline;
        tokio::task::spawn_blocking(move || {
            crate::app::bootstrap::model_catalog_for_readiness(
                offline,
                &crate::app::bootstrap::CatalogReadiness::Fleet,
            )
        })
    });
    pickers::optional_model_picker_live(shell, input, app, pending).await
}

/// Whether the active branch ends in an assistant tool call that has no
/// durable result yet. Appending a user message in that state would place text
/// between a tool call and its required result, so the shell record stays
/// non-model-visible until the call is settled at the next run boundary.
fn session_has_unresolved_tool_calls(session: &Session) -> bool {
    let mut settled = std::collections::HashSet::new();
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        match &entry.value {
            octet_agent::EntryValue::Message(octet_ai::Message::Assistant(assistant)) => {
                if assistant.content.iter().any(|part| {
                    matches!(part, octet_ai::AssistantPart::ToolCall(call) if !settled.contains(&call.id))
                }) {
                    return true;
                }
            }
            octet_agent::EntryValue::Message(octet_ai::Message::User(user)) => {
                for part in &user.content {
                    if let octet_ai::UserPart::ToolResult(result) = part {
                        settled.insert(result.tool_call_id.clone());
                    }
                }
            }
            _ => {}
        }
        cursor = entry.parent.clone();
    }
    false
}

/// Append one finished shell escape to the durable session.
///
/// `!command` results become ordinary user messages (model-visible).
/// `!!command` results use a non-model-visible config envelope whose
/// presentation metadata still retains the exact command, exit status, and
/// output, so the execution is accounted for without entering model context.
/// An unresolved tool call forces the excluded form even for `!command`, so a
/// pending call can never be split from its result.
fn record_shell_escape(app: &mut App, record: &commands::ShellEscapeRecord) -> anyhow::Result<()> {
    let session = app.agent.session_mut();
    if record.excluded() || session_has_unresolved_tool_calls(session) {
        session.append_with_metadata(
            octet_agent::EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            Some(octet_agent::EntryMetadata {
                display_text: Some(record.transcript_text()),
                ..octet_agent::EntryMetadata::default()
            }),
        )?;
    } else {
        session.append(octet_agent::EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text(record.context_text())],
            },
        )))?;
    }
    Ok(())
}

/// Ordinary process admission for one local shell escape.
///
/// This builds the same canonical `bash` intent the model tool would produce
/// and lets the shared [`EffectBroker`] decide: `unsafe_host` admits it,
/// `controlled` asks only for commands its shell-safety analysis flags, and
/// `controlled_bash_approval` asks for every command. The confirmation is the
/// same picker the model run uses; a denial is reported, never swallowed.
enum ShellEscapeApproval {
    Approved,
    Denied(String),
}

async fn approve_shell_escape<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    policy: octet_agent::EffectPolicy,
    command: &str,
) -> anyhow::Result<ShellEscapeApproval>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let arguments = serde_json::json!({ "command": command });
    let intent = match EffectIntent::new(
        "local-shell",
        "local-shell-escape",
        0,
        "local-shell-escape",
        "bash",
        ToolEffect::HostProcess,
        &arguments,
    ) {
        Ok(intent) => intent,
        Err(error) => {
            return Ok(ShellEscapeApproval::Denied(format!(
                "shell command could not be classified: {error}"
            )));
        }
    };
    let broker = EffectBroker::new(policy);
    let (sink, mut progress) = ToolProgressSink::bounded_channel();
    let mut authorization = Box::pin(broker.authorize(&intent, Some(&sink)));
    let result = loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.request_close();
                return Ok(ShellEscapeApproval::Denied(
                    "shell command stopped during shutdown".to_owned(),
                ));
            }
            result = &mut authorization => break result,
            update = progress.recv() => match update {
                Some(ToolProgress::Confirmation(request)) => {
                    let confirmed = confirmation_picker(shell, input, &request).await?;
                    request.respond(confirmed);
                }
                Some(_) => {}
                None => {}
            },
        }
    };
    match result {
        Ok(_receipt) => Ok(ShellEscapeApproval::Approved),
        Err(error) => Ok(ShellEscapeApproval::Denied(error.to_string())),
    }
}

/// One finished local shell invocation.
struct ShellEscapeOutcome {
    /// Combined bounded stdout/stderr as rendered in the live transcript.
    output: String,
    exit_code: i32,
    /// Set when the command never executed (spawn/prepare failure).
    refusal: Option<String>,
    /// The user interrupted it or it exceeded the product deadline.
    stopped: bool,
    /// The product is shutting down; the caller owns the exit.
    shutting_down: bool,
}

/// Run one local shell escape with the product's bounded capture, deadline,
/// interrupt handling, and process-group cleanup.
///
/// The approval decision is the caller's (it owns the picker); this function
/// only executes an approved command under the launch sandbox limits.
async fn run_local_shell<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    workspace: &Path,
    sandbox: &SandboxPolicy,
    command: &str,
) -> anyhow::Result<ShellEscapeOutcome>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let shell_id = shell.append_shell_in_progress(command.to_owned());
    shell.render();

    // Spawn the child process with piped output.
    let mut process = tokio::process::Command::new("sh");
    process
        .current_dir(workspace)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);
    #[cfg(unix)]
    let launch = match BashProcessLaunch::prepare(&mut process, command) {
        Ok(launch) => launch,
        Err(error) => {
            let message = format!("failed to prepare lifecycle supervision: {error}");
            shell.finalize_shell(&shell_id, message.clone(), -1);
            return Ok(ShellEscapeOutcome {
                output: message.clone(),
                exit_code: -1,
                refusal: Some(message),
                stopped: false,
                shutting_down: false,
            });
        }
    };
    #[cfg(unix)]
    process.arg("-c").arg(launch.source());
    #[cfg(not(unix))]
    process.arg("-c").arg(command);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = format!("failed to spawn: {error}");
            shell.finalize_shell(&shell_id, message.clone(), -1);
            return Ok(ShellEscapeOutcome {
                output: message.clone(),
                exit_code: -1,
                refusal: Some(message),
                stopped: false,
                shutting_down: false,
            });
        }
    };
    #[cfg(unix)]
    let (group_guard, handoff) = match launch.register(child.id()).await {
        Ok(registered) => registered,
        Err(error) => {
            let _ = child.wait().await;
            let message = format!("failed to register lifecycle supervision: {error}");
            shell.finalize_shell(&shell_id, message.clone(), -1);
            return Ok(ShellEscapeOutcome {
                output: message.clone(),
                exit_code: -1,
                refusal: Some(message),
                stopped: false,
                shutting_down: false,
            });
        }
    };

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let output_budget = sandbox.max_output_bytes;
    let stdout_budget = output_budget / 2;
    let stderr_budget = output_budget.saturating_sub(stdout_budget);
    let stdout = Arc::new(Mutex::new(BoundedShellOutput::new(stdout_budget)));
    let stderr = Arc::new(Mutex::new(BoundedShellOutput::new(stderr_budget)));
    let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel();
    let command_timeout = Duration::from_secs(sandbox.bash_timeout_secs);
    #[cfg(unix)]
    let command_started = tokio::time::Instant::now();
    let stdout_capture = stdout.clone();
    let stderr_capture = stderr.clone();
    let stdout_updates = output_tx.clone();
    let stderr_updates = output_tx;
    let work = async {
        #[cfg(unix)]
        let (_, _, status) = tokio::join!(
            drain_shell_pipe(&mut stdout_pipe, &stdout_capture, &stdout_updates,),
            drain_shell_pipe(&mut stderr_pipe, &stderr_capture, &stderr_updates,),
            wait_for_bash_process(&mut child, handoff),
        );
        #[cfg(not(unix))]
        let (_, _, status) = tokio::join!(
            drain_shell_pipe(&mut stdout_pipe, &stdout_capture, &stdout_updates,),
            drain_shell_pipe(&mut stderr_pipe, &stderr_capture, &stderr_updates,),
            child.wait(),
        );
        status
    };
    let mut work = Box::pin(work);
    let deadline = tokio::time::sleep(command_timeout);
    tokio::pin!(deadline);
    let mut input_open = true;
    let mut interrupted = false;
    let mut timed_out = false;
    let mut shutting_down = false;

    let exit = loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                interrupted = true;
                shutting_down = true;
                break Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "command stopped during shutdown",
                ));
            }
            status = &mut work => {
                break status;
            }
            event = input.next(), if input_open => match event {
                Some(Ok(Event::Key(key))) if keymap::is_close_key(&key) => {
                    shell.request_close();
                    interrupted = true;
                    shutting_down = true;
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "command stopped during close",
                    ));
                }
                Some(Ok(Event::Key(key)))
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                        && key.code == KeyCode::Char('c')
                        && key.modifiers == KeyModifiers::CONTROL =>
                {
                    interrupted = true;
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "command cancelled",
                    ));
                }
                Some(Ok(Event::Key(key)))
                    if key.kind == KeyEventKind::Press
                        && key.code == KeyCode::Char('o')
                        && key.modifiers == KeyModifiers::CONTROL =>
                {
                    shell.toggle_disclosure();
                    shell.render();
                }
                Some(Ok(Event::Resize(columns, rows))) => {
                    shell.set_size(columns, rows);
                    shell.render();
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => input_open = false,
            },
            _ = &mut deadline => {
                timed_out = true;
                break Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "command timed out",
                ));
            }
            update = output_rx.recv() => {
                if update.is_some() {
                    // Collapse a burst into one bounded tail update. This keeps
                    // the latest process lines visible without repainting once
                    // per read syscall.
                    while output_rx.try_recv().is_ok() {}
                    shell.update_shell_output(
                        &shell_id,
                        rendered_shell_captures(&stdout, &stderr),
                    );
                    shell.render();
                }
            }
        }
    };

    if shutting_down {
        #[cfg(unix)]
        {
            let process_cleanup = octet_agent::extension_process::terminate_bash_process_groups(
                Duration::from_millis(400),
            );
            let _ = tokio::time::timeout(Duration::from_millis(500), async {
                tokio::join!(&mut work, process_cleanup)
            })
            .await;
            group_guard.terminate_now();
        }
        drop(work);
        #[cfg(not(unix))]
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let mut combined = rendered_shell_captures(&stdout, &stderr);
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("command stopped during shutdown");
        shell.finalize_shell(&shell_id, combined.clone(), -1);
        shell.render();
        return Ok(ShellEscapeOutcome {
            output: combined,
            exit_code: -1,
            refusal: None,
            stopped: true,
            shutting_down: true,
        });
    }

    if interrupted || timed_out {
        #[cfg(unix)]
        {
            group_guard.terminate_now();
            // Retain output already in the pipes when ordinary descendants
            // close promptly, but an escaped child must not defeat the
            // execution deadline forever.
            let _ = tokio::time::timeout(Duration::from_millis(500), &mut work).await;
        }
    } else {
        #[cfg(unix)]
        group_guard.supervise_bash_descendants(
            command_timeout.saturating_sub(command_started.elapsed()),
            Default::default(),
        );
    }
    // Releasing the concurrent wait/drain future closes any descriptors
    // retained by an escaped descendant.
    drop(work);
    #[cfg(not(unix))]
    if interrupted || timed_out {
        let _ = child.kill().await;
    }

    let exit_code = match exit {
        Ok(status) => status.code().unwrap_or(-1),
        Err(error) => {
            let mut combined = rendered_shell_captures(&stdout, &stderr);
            if !combined.is_empty() {
                combined.push('\n');
            }
            if interrupted {
                combined.push_str("command cancelled");
            } else if timed_out {
                combined.push_str(&format!(
                    "command exceeded the {}s execution limit",
                    sandbox.bash_timeout_secs
                ));
            } else {
                combined.push_str(&format!("process error: {error}"));
            }
            shell.finalize_shell(&shell_id, combined.clone(), -1);
            shell.render();
            return Ok(ShellEscapeOutcome {
                output: combined,
                exit_code: -1,
                refusal: None,
                stopped: interrupted || timed_out,
                shutting_down: false,
            });
        }
    };

    let combined = rendered_shell_captures(&stdout, &stderr);
    shell.finalize_shell(&shell_id, combined.clone(), exit_code);
    shell.render();
    Ok(ShellEscapeOutcome {
        output: combined,
        exit_code,
        refusal: None,
        stopped: false,
        shutting_down: false,
    })
}

/// `/settings` at the idle boundary. Reports render immediately; mutations
/// persist through the shared configuration writer and apply in-session.
async fn apply_settings_command(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    sub: commands::SettingsCommand,
) -> anyhow::Result<()> {
    use commands::SettingsCommand as Sub;
    match sub {
        Sub::Show => shell.show_report_text(
            "Settings",
            "Effective display and default preferences",
            commands::settings_text(&commands::SettingsSurface::capture(app)),
        ),
        Sub::Transport => shell.notice(format!(
            "transport {} (declared by the {} route; not a user preference)",
            commands::transport_label(&app.model),
            app.model.endpoint.id.0,
        )),
        Sub::Padding => shell.notice(
            "editor padding is compiled into the active theme layout; octet persists no padding override"
                .to_owned(),
        ),
        Sub::Theme(requested) => {
            configure_terminal_theme(
                shell,
                input,
                &mut app.config,
                requested,
                false,
                Some(&mut app.executable_extensions),
            )
            .await?;
        }
        Sub::Images(requested) => {
            let enabled = requested.unwrap_or(!app.config.show_images);
            if let Err(error) = persist_configuration(Some(&mut app.executable_extensions), || {
                crate::cli::persist_show_images(enabled)
            })
            .await
            {
                shell.error(format!("failed to save the image display preference: {error}"));
                return Ok(());
            }
            app.config.show_images = enabled;
            shell.set_runtime_config(app.config.clone());
            shell.notice(format!(
                "inline tool-result images {}",
                if enabled { "enabled" } else { "disabled" }
            ));
        }
        Sub::DefaultModel(id) => match id {
            None => shell.notice(format!(
                "default model for new sessions: {}",
                app.config
                    .model
                    .as_ref()
                    .map(|model| model.0.as_str())
                    .unwrap_or("(chosen at startup or by the session)")
            )),
            Some(id) => {
                let model = ModelId(id);
                if app.catalog.resolve(&model).is_err() {
                    shell.error(format!(
                        "unknown or unavailable model {:?}; /model lists the credential-scoped catalog",
                        model.0
                    ));
                    return Ok(());
                }
                if let Err(error) = persist_configuration(Some(&mut app.executable_extensions), || {
                    crate::cli::persist_model(&model.0)
                })
                .await
                {
                    shell.error(format!("failed to save the default model: {error}"));
                    return Ok(());
                }
                shell.notice(format!(
                    "default model for new sessions: {} (use /model to switch this session now)",
                    model.0
                ));
            }
        },
        Sub::DefaultReasoning(level) => match level {
            None => shell.notice(format!(
                "default reasoning: {}",
                reasoning_label(&app.reasoning)
            )),
            Some(level) => match crate::config::parse_reasoning(&level) {
                Ok(reasoning) => {
                    let label = reasoning_label(&reasoning);
                    if let Err(error) =
                        persist_configuration(Some(&mut app.executable_extensions), || {
                            crate::cli::persist_reasoning(&label)
                        })
                        .await
                    {
                        shell.error(format!("failed to save the default reasoning: {error}"));
                        return Ok(());
                    }
                    shell.notice(format!(
                        "default reasoning for new sessions: {label} (use /thinking to change this session now)"
                    ));
                }
                Err(error) => shell
                    .error(format!("invalid reasoning level {level:?}: {error}")),
            },
        },
    }
    Ok(())
}

/// One `/scoped-models` mutation against the App's ordered scope.
///
/// Returns the persistence decision (`None` = remove the key, `Some(patterns)`
/// = write them) or a reportable error. Deliberately free of the shell and the
/// config writer so the ordering rules are unit-testable without touching the
/// user's real configuration.
fn apply_scope_mutation(
    app: &mut App,
    sub: &commands::ScopedModelsCommand,
) -> Result<Option<Option<String>>, String> {
    use crate::cli::parity::ScopedModel;
    use commands::ScopedModelsCommand as Sub;
    let mut available = app
        .catalog
        .models()
        .map(|spec| (spec.id.0.clone(), spec.endpoint.0.clone()))
        .collect::<Vec<_>>();
    // The catalog is a map, so sort before turning it into an order the user
    // sees, cycles through, or persists; the result is stable across runs.
    available.sort();
    let materialized = || {
        available
            .iter()
            .map(|(id, _)| ScopedModel {
                id: ModelId(id.clone()),
                pattern: id.clone(),
                reasoning: None,
            })
            .collect::<Vec<_>>()
    };
    // Every mutation operates on an explicit ordered scope. An unrestricted
    // scope is materialized from the current catalog first, so toggling one
    // provider never silently changes every other model's membership.
    if app.model_scope.is_none() && !matches!(sub, Sub::All | Sub::Clear) {
        app.model_scope = Some(materialized());
    }
    match sub {
        Sub::Show => Ok(None),
        Sub::All => {
            app.model_scope = Some(materialized());
            // `*` re-selects every currently available model on the next
            // interactive launch without pinning today's catalog contents.
            Ok(Some(Some("*".to_owned())))
        }
        Sub::Clear => {
            app.model_scope = None;
            Ok(Some(None))
        }
        Sub::Enable(target) | Sub::Disable(target) => {
            let enable = matches!(sub, Sub::Enable(_));
            let scope = app
                .model_scope
                .as_mut()
                .expect("a mutation scope is always materialized above");
            commands::set_scope_target(scope, &available, target, enable)?;
            Ok(Some(commands::scope_patterns_string(scope)))
        }
        Sub::Toggle(target) => {
            let scope = app
                .model_scope
                .as_mut()
                .expect("a mutation scope is always materialized above");
            commands::toggle_scope_target(scope, &available, target)?;
            Ok(Some(commands::scope_patterns_string(scope)))
        }
        Sub::Move { model, direction } => {
            let scope = app
                .model_scope
                .as_mut()
                .expect("a mutation scope is always materialized above");
            commands::move_scope_model(scope, model, *direction)?;
            Ok(Some(commands::scope_patterns_string(scope)))
        }
    }
}

/// `/scoped-models` at the idle boundary. The ordered scope applies to the
/// live shell's cycling bindings immediately and persists as the exact ordered
/// pattern list the next interactive launch reads back.
async fn apply_scoped_models_command(
    app: &mut App,
    shell: &mut InteractiveShell,
    sub: commands::ScopedModelsCommand,
) {
    if matches!(sub, commands::ScopedModelsCommand::Show) {
        let mut available = app
            .catalog
            .models()
            .map(|spec| (spec.id.0.clone(), spec.endpoint.0.clone()))
            .collect::<Vec<_>>();
        available.sort();
        shell.show_report_text(
            "Scoped models",
            "Ordered model cycling scope",
            commands::scoped_models_text(app.model_scope.as_deref(), &available),
        );
        return;
    }
    let persistence = match apply_scope_mutation(app, &sub) {
        Ok(persistence) => persistence,
        Err(error) => {
            shell.error(error);
            return;
        }
    };
    // Report the decision from the resulting scope, so the notice can never
    // disagree with what cycling actually follows. An unrestricted scope
    // covers the whole catalog.
    let count = app
        .model_scope
        .as_ref()
        .map_or_else(|| app.catalog.models().count(), Vec::len);
    shell.notice(format!(
        "model scope now covers {count} model(s); /scoped-models lists the order"
    ));
    if let Some(patterns) = persistence {
        if let Err(error) = persist_configuration(Some(&mut app.executable_extensions), || {
            crate::cli::persist_scoped_models(patterns.as_deref())
        })
        .await
        {
            shell.error(format!("failed to save the model scope: {error}"));
        }
    }
    update_status(shell, app);
}

/// Idle dispatch for one local shell escape. The command keeps the ordinary
/// process gates, the ordinary approval decision, the ordinary bounded
/// capture/cleanup, and an explicit context decision.
async fn run_idle_shell_escape(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    escape: commands::BashEscape,
) -> anyhow::Result<IdleCommandOutcome> {
    if !app.config.sandbox.process_execution_allowed() {
        shell.error("shell commands are disabled by --no-process/--no-shell".to_owned());
        return Ok(IdleCommandOutcome::Continue(Box::new(app)));
    }
    let prefix = if escape.excluded { "!!" } else { "!" };
    if escape.command.trim().is_empty() {
        shell.notice(format!("usage: {prefix}<shell command>"));
        return Ok(IdleCommandOutcome::Continue(Box::new(app)));
    }
    if !matches!(
        approve_shell_escape(shell, input, app.config.effect_policy, &escape.command).await?,
        ShellEscapeApproval::Approved
    ) {
        shell.error(format!("{prefix} command was not approved"));
        return Ok(IdleCommandOutcome::Continue(Box::new(app)));
    }
    app.executable_extensions
        .notify_user_bash_all(&escape.command);
    shell.on_local_command_submitted(&format!("{prefix}{}", escape.command));
    let outcome = run_local_shell(
        shell,
        input,
        &app.config.workspace,
        &app.config.sandbox,
        &escape.command,
    )
    .await?;
    if outcome.shutting_down {
        return Ok(IdleCommandOutcome::Quit(Box::new(app)));
    }
    if let Some(refusal) = outcome.refusal {
        shell.error(refusal);
        return Ok(IdleCommandOutcome::Continue(Box::new(app)));
    }
    let record = commands::ShellEscapeRecord::new(
        escape.command.clone(),
        outcome.output,
        outcome.exit_code,
        escape.excluded,
    );
    if let Err(error) = record_shell_escape(&mut app, &record) {
        shell.error(format!("shell result could not be recorded: {error}"));
        return Ok(IdleCommandOutcome::Continue(Box::new(app)));
    }
    let context_note = if escape.excluded {
        "excluded from model context"
    } else {
        "added to model context"
    };
    shell.notice(if outcome.stopped {
        format!("shell result recorded ({context_note}); the command was stopped")
    } else {
        format!("shell result {context_note}")
    });
    Ok(IdleCommandOutcome::Continue(Box::new(app)))
}

async fn run_idle_command(
    mut app: App,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    command: Command,
    goal_deadline: &mut Option<Instant>,
    reexec: Option<&mut crate::reexec::ReexecController>,
    reload: &mut crate::reload::ReloadSupervisor,
) -> anyhow::Result<IdleCommandOutcome> {
    match command {
        Command::Changelog => shell.show_changelog(),
        Command::Hotkeys => show_hotkeys(shell),
        Command::Copy => copy_last_assistant(shell),
        Command::Session => shell.show_report_text(
            "Session",
            "Durable session facts",
            commands::session_text(app.agent.session()),
        ),
        Command::Help(topic) => {
            shell.show_report_text(
                "Help",
                "Browse commands and keyboard shortcuts",
                commands::help_text(&app.config.workspace, topic.as_deref()),
            );
        }
        Command::Status => {
            shell.show_status_text_with_telemetry(commands::status_text(&app, None));
        }
        Command::Context => {
            shell.show_context_report(crate::tui::context::ContextReport::capture(&app, &[]));
        }
        Command::Cost => shell.show_report_text(
            "Cost",
            "Review session token usage and estimated cost",
            commands::cost_text(app.agent.session(), &app.model),
        ),
        Command::Cache => shell.show_report_text(
            "Cache",
            "Review session cache accounting",
            commands::cache_text(app.agent.session()),
        ),
        Command::Update => {
            match await_lifecycle(shell, input, "checking for updates…", async {
                crate::update::check().await
            })
            .await
            {
                Ok(status) => shell.show_overlay_text(match status {
                    crate::update::UpdateStatus::Available { .. } => {
                        format!("{}\n\nRun `octet update` to install.", status)
                    }
                    status => status.to_string(),
                }),
                Err(error) => shell.error(format!("update check failed: {error}")),
            }
        }
        Command::Name(name) => {
            let id = app
                .agent
                .session()
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| anyhow::anyhow!("current session has no valid id"))?
                .to_owned();
            match name {
                Some(name) => {
                    let metadata = app.sessions.rename(&id, &name)?;
                    shell.notice(format!(
                        "session named {}",
                        metadata.name.as_deref().unwrap_or("(unnamed)")
                    ));
                    request_extension_ui(shell, &mut app);
                }
                None => {
                    let metadata = app.sessions.load_metadata(&id)?;
                    shell.notice(format!(
                        "session name: {}",
                        metadata
                            .name
                            .as_deref()
                            .unwrap_or("(derived from first prompt)")
                    ));
                }
            }
        }
        Command::Export(output) => {
            let id = app
                .agent
                .session()
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| anyhow::anyhow!("current session has no valid id"))?;
            let report = crate::session_commands::export_portable(
                &app.sessions,
                id,
                output.map(std::path::PathBuf::from),
                &app.config.invocation_cwd,
                false,
                false,
            )?;
            shell.show_overlay_text(format!(
                "Exported {}\nRedacted {} potentially sensitive values{}",
                report.destination.display(),
                report.redaction_count,
                if report.ignored_torn_tail {
                    "\nIgnored an interrupted final append; use `octet sessions repair`."
                } else {
                    ""
                }
            ));
        }
        Command::Prompt(None) => shell.show_overlay_text(prompt_templates_text(&app)),
        Command::Answer(instruction) => {
            return Ok(IdleCommandOutcome::Submit {
                app: Box::new(app),
                input: answer_now_input(instruction),
            });
        }
        Command::Prompt(Some(invocation)) => {
            let selection = shell.selected_plain_text();
            match expand_prompt_invocation(&mut app, &invocation, false, selection.as_deref()) {
                Ok(Some(rendered)) => {
                    if app.config.debug_prompt {
                        shell.show_overlay_text(crate::prompts::debug_expansion(&rendered));
                    }
                    return Ok(IdleCommandOutcome::Submit {
                        app: Box::new(app),
                        input: ComposedInput::from_text(rendered.text),
                    });
                }
                Ok(None) => shell.error("usage: /prompt <name> [arguments]".into()),
                Err(error) => shell.error(error.to_string()),
            }
        }
        Command::Extensions(commands::ExtensionsSubcommand::Menu) => {
            app = extension_management_menu(app, shell, input).await?;
        }
        Command::Extensions(commands::ExtensionsSubcommand::Status) => {
            request_extension_ui(shell, &mut app);
            shell.show_overlay_text(app.executable_extensions.inspect_text());
        }
        Command::Extensions(commands::ExtensionsSubcommand::Reload) => {
            let report = await_lifecycle(shell, input, "reloading extensions…", async {
                Ok(app.executable_extensions.reload_report().await)
            })
            .await?;
            let mut messages = extension_reload_notices(reload, report, true);
            let catalog = app
                .executable_extensions
                .synchronize_provider_catalog_report(&mut app.catalog, &app.client);
            messages.extend(provider_reload_notices(reload, catalog, true));
            if messages.is_empty() {
                shell.notice("no running executable extensions to reload");
            } else {
                shell.show_overlay_text(messages.join("\n"));
            }
            request_extension_ui(shell, &mut app);
        }
        Command::Extensions(commands::ExtensionsSubcommand::Inspect { reference }) => {
            let _ = app.executable_extensions.drain_events();
            app.synchronize_extension_provider_catalog();
            let principal = app
                .executable_extensions
                .presentation_session_reference_principal(&reference);
            if let Some(principal) = principal {
                match app
                    .agent
                    .open_delegated_session_reference(&principal, &reference)
                {
                    Ok(Some(session)) => {
                        let theme = shell.theme();
                        let width = shell.read_only_document_width();
                        let verbose_tools = shell.verbose_tools();
                        match delegated_session_text(&session, &theme, width, verbose_tools) {
                            Ok(text) => shell.show_styled_overlay_text(
                                delegated_session_overlay_text(&text, &theme),
                            ),
                            Err(error) => {
                                shell.error(format!("failed to inspect delegated session: {error}"))
                            }
                        }
                    }
                    Ok(None) => shell
                        .error("delegated session reference is unavailable for this parent".into()),
                    Err(error) => {
                        shell.error(format!("failed to inspect delegated session: {error}"))
                    }
                }
            } else {
                shell.error(
                    "delegated session reference is unavailable, stale, or owned by another extension"
                        .into(),
                );
            }
        }
        Command::Extensions(commands::ExtensionsSubcommand::Action { extension, action }) => {
            let result = {
                let dialogs = app.executable_extensions.lifecycle_snapshot();
                let mut confirmations = InteractiveExtensionConfirmations {
                    shell,
                    input,
                    dialogs: &dialogs,
                };
                app.executable_extensions
                    .execute_presentation_action_with_confirmation(
                        &extension,
                        &action,
                        &mut confirmations,
                    )
                    .await
            };
            match result {
                Ok(output) if output.trim().is_empty() => {
                    shell.notice(format!("{extension} action {action} completed"));
                }
                Ok(output) => shell.show_overlay_text(output),
                Err(error) => shell.error(format!("extension action failed: {error}")),
            }
            request_extension_ui(shell, &mut app);
        }
        Command::Exit => return Ok(IdleCommandOutcome::Quit(Box::new(app))),
        Command::Login(provider) => match validate_provider(provider.as_deref()) {
            Ok("codex") => login_codex(&mut app, shell).await?,
            Ok("custom") => login_custom(shell)?,
            Ok(_) => unreachable!(),
            Err(e) => shell.error(e.to_string()),
        },
        Command::Logout(provider) => match validate_provider(provider.as_deref()) {
            Ok("codex") => {
                app = logout_codex(app, shell, input).await?;
            }
            Ok("custom") => {
                app = logout_custom(app, shell, input).await?;
            }
            Ok(_) => unreachable!(),
            Err(e) => shell.error(e.to_string()),
        },
        Command::New => {
            app = transition(app, shell, input, Reconfig::NewSession).await?;
            shell.notice("created a new session");
        }
        Command::Resume(Some(id)) => {
            let path = app.sessions.path_by_id(&id)?;
            app = transition(app, shell, input, Reconfig::Resume(path)).await?;
            shell.notice("resumed session");
        }
        Command::Resume(None) => {
            if let Some(path) = pick_session_path(
                shell,
                input,
                &app.sessions,
                Some(app.agent.session().path()),
            )
            .await?
            {
                app = transition(app, shell, input, Reconfig::Resume(path)).await?;
                shell.notice("resumed session");
            }
        }
        Command::Fork => {
            app = fork_session(app, shell, input).await?;
        }
        Command::Clone => {
            app = clone_session(app, shell, input).await?;
        }
        Command::Fast(requested) => apply_fast_command(&mut app, shell, requested),
        Command::Model(Some(id)) => {
            app = transition(app, shell, input, Reconfig::Model(ModelId(id))).await?;
        }
        Command::Theme(requested) => {
            configure_terminal_theme(
                shell,
                input,
                &mut app.config,
                requested,
                false,
                Some(&mut app.executable_extensions),
            )
            .await?;
        }
        Command::Thinking(Some(level)) => {
            let level = ThinkingLevel::parse(&level)?;
            let reasoning =
                requested_thinking_to_reasoning(level, &app.model, app.subagents_available())?;
            app = select_thinking(app, shell, input, reasoning, None).await?;
        }
        Command::Debug => {
            let rendered = shell.dump_rendered_frame().await;
            write_debug_report(shell, app.agent.session(), None, rendered).await;
        }
        Command::Model(None) => {
            if let Some(model) = open_model_picker(&mut app, shell, input).await? {
                app = transition(app, shell, input, Reconfig::Model(model)).await?;
            }
        }
        Command::Thinking(None) => {
            if let Some((mode, level)) = thinking_configuration_picker(&app, shell, input).await? {
                let reasoning = requested_thinking_to_reasoning(
                    level,
                    &app.model,
                    app.subagents_available(),
                )?;
                app = select_thinking(
                    app,
                    shell,
                    input,
                    reasoning,
                    Some((mode, level)),
                )
                .await?;
            }
        }
        Command::Verbose(value) => {
            let enabled = value.unwrap_or(!shell.verbose_tools());
            shell.set_verbose_tools(enabled);
            shell.notice(format!(
                "verbose transcript {}",
                if enabled { "enabled" } else { "disabled" }
            ));
        }
        Command::Compact => {
            compact_interactively(&mut app, shell, input, true, None).await;
        }
        Command::CompactWithInstructions(instructions) => {
            compact_interactively(&mut app, shell, input, true, Some(&instructions)).await;
        }
        Command::AutoCompact(setting) => {
            configure_auto_compaction(&mut app, shell, setting)?;
            update_status(shell, &app);
        }
        Command::Reload => {
            // Plain `/reload` is resources-only: it never probes or replaces
            // the process image. `/reload --force` is handled before
            // `commands::parse` and is the explicit host path.
            let (next, plan) = reload_resources_with_reexec(
                app,
                shell,
                input,
                reexec,
                HostPass::ResourcesOnly,
                reload,
            )
            .await?;
            shell.notice("resources reloaded");
            app = next;
            if let Some(plan) = plan {
                return Ok(IdleCommandOutcome::Reexec {
                    app: Box::new(app),
                    plan,
                });
            }
        }
        Command::Skills(commands::SkillsSubcommand::Load(id)) => {
            if let Err(error) = app.skills.load(&id) {
                shell.error(format!("Failed to invoke skill '{id}': {error}"));
            } else {
                return Ok(IdleCommandOutcome::Submit {
                    app: Box::new(app),
                    input: ComposedInput::from_text(format!("/skill:{id}")),
                });
            }
        }
        Command::Skills(commands::SkillsSubcommand::Reload) => {
            app = reload_resources(app, shell, input).await?;
            request_extension_ui(shell, &mut app);
            shell.notice("skills and prompt templates reloaded");
        }
        Command::Skills(sub) => {
            execute_skills_command(&mut app, shell, sub).await?;
        }
        Command::Goal(goal) => {
            apply_idle_goal_command(&app, shell, goal, goal_deadline)?;
        }
        Command::Settings(sub) => {
            apply_settings_command(&mut app, shell, input, sub).await?;
        }
        Command::ScopedModels(sub) => {
            apply_scoped_models_command(&mut app, shell, sub).await;
        }
        Command::Bash(escape) => {
            return run_idle_shell_escape(app, shell, input, escape).await;
        }
        Command::Unknown(text) => {
            let (extension_name, extension_arguments) = split_prompt_invocation(&text)
                .map(|(name, arguments)| {
                    (
                        name.to_owned(),
                        arguments
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                    )
                })
                .unwrap_or_default();
            let presentation_owner = app.executable_extensions.command_owner(&extension_name);
            let open_subagents = extension_name == "subagents"
                && extension_arguments.is_empty()
                && presentation_owner.as_deref() == Some("octet-subagents");
            let result = {
                let dialogs = app.executable_extensions.lifecycle_snapshot();
                let mut confirmations = InteractiveExtensionConfirmations {
                    shell,
                    input,
                    dialogs: &dialogs,
                };
                app.executable_extensions
                    .execute_command_with_confirmation(
                        &extension_name,
                        extension_arguments,
                        &mut confirmations,
                    )
                    .await
            };
            match result {
                Ok(Some(output)) if open_subagents => {
                    subagents_view(&mut app, shell, input, output).await?;
                }
                Ok(Some(output)) => {
                    let presentation = presentation_owner
                        .as_deref()
                        .and_then(|owner| app.executable_extensions.presentation_text_for(owner));
                    let mut visible_blocks = Vec::new();
                    if !output.trim().is_empty() {
                        visible_blocks.push(output);
                    }
                    visible_blocks.extend(presentation);
                    let visible = visible_blocks.join("\n\n");
                    if visible.trim().is_empty() {
                        shell.notice(format!("/{extension_name} completed"));
                    } else {
                        shell.show_extension_output(&extension_name, visible);
                    }
                }
                Ok(None) => {
                    let selection = shell.selected_plain_text();
                    match expand_prompt_invocation(&mut app, &text, true, selection.as_deref()) {
                        Ok(Some(rendered)) => {
                            if app.config.debug_prompt {
                                shell.show_overlay_text(crate::prompts::debug_expansion(&rendered));
                            }
                            return Ok(IdleCommandOutcome::Submit {
                                app: Box::new(app),
                                input: ComposedInput::from_text(rendered.text),
                            });
                        }
                        Ok(None) => {
                            // A slash command that no running extension
                            // contributes may still belong to an extension
                            // that is starting, degraded, or parked. Say so
                            // instead of a bare unknown.
                            let not_ready: Vec<String> = app
                                .executable_extensions
                                .summaries()
                                .into_iter()
                                .filter(|summary| {
                                    summary.enabled
                                        && (!summary.running
                                            || summary.health.as_ref().is_some_and(|health| {
                                                health.state
                                                    != octet_agent::ExtensionHealthState::Ready
                                            }))
                                })
                                .map(|summary| summary.name)
                                .collect();
                            if text.starts_with('/') && !not_ready.is_empty() {
                                shell.error(format!(
                                    "unknown command: {text} (extensions not ready: {} — see /extensions status)",
                                    not_ready.join(", ")
                                ));
                            } else {
                                shell.error(format!("unknown command: {text}"));
                            }
                        }
                        Err(error) => shell.error(error.to_string()),
                    }
                }
                Err(error) => shell.error(format!("extension command failed: {error}")),
            }
        }
    }
    shell.render();
    Ok(IdleCommandOutcome::Continue(Box::new(app)))
}

#[derive(Default)]
struct BoundedShellOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: usize,
    budget: usize,
}

impl BoundedShellOutput {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        let head_capacity = self.budget / 2;
        let tail_capacity = self.budget.saturating_sub(head_capacity);
        let mut remaining = bytes;
        if self.head.len() < head_capacity {
            let keep = remaining.len().min(head_capacity - self.head.len());
            self.head.extend_from_slice(&remaining[..keep]);
            remaining = &remaining[keep..];
        }
        if remaining.is_empty() || tail_capacity == 0 {
            return;
        }
        if remaining.len() >= tail_capacity {
            self.tail.clear();
            self.tail
                .extend(remaining[remaining.len() - tail_capacity..].iter().copied());
            return;
        }
        let overflow = self
            .tail
            .len()
            .saturating_add(remaining.len())
            .saturating_sub(tail_capacity);
        if overflow > 0 {
            self.tail.drain(..overflow);
        }
        self.tail.extend(remaining.iter().copied());
    }

    fn render(&self, stream: &str) -> String {
        if self.total_bytes <= self.budget {
            let mut complete = Vec::with_capacity(self.total_bytes);
            complete.extend_from_slice(&self.head);
            complete.extend(self.tail.iter().copied());
            return String::from_utf8_lossy(&complete).into_owned();
        }
        let omitted = self
            .total_bytes
            .saturating_sub(self.head.len())
            .saturating_sub(self.tail.len());
        let tail = self.tail.iter().copied().collect::<Vec<_>>();
        format!(
            "{}\n[… {stream} truncated; {omitted} bytes omitted …]\n{}",
            String::from_utf8_lossy(&self.head),
            String::from_utf8_lossy(&tail)
        )
    }
}

async fn drain_shell_pipe<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut Option<R>,
    capture: &std::sync::Arc<std::sync::Mutex<BoundedShellOutput>>,
    updates: &tokio::sync::mpsc::UnboundedSender<()>,
) {
    use tokio::io::AsyncReadExt as _;

    let Some(reader) = reader.as_mut() else {
        return;
    };
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                capture
                    .lock()
                    .expect("shell output mutex poisoned")
                    .push(&buffer[..read]);
                let _ = updates.send(());
            }
        }
    }
}

fn rendered_shell_captures(
    stdout: &std::sync::Arc<std::sync::Mutex<BoundedShellOutput>>,
    stderr: &std::sync::Arc<std::sync::Mutex<BoundedShellOutput>>,
) -> String {
    let out = stdout
        .lock()
        .expect("shell stdout mutex poisoned")
        .render("stdout");
    let err = stderr
        .lock()
        .expect("shell stderr mutex poisoned")
        .render("stderr");
    let mut combined = String::new();
    if !out.is_empty() {
        combined.push_str(out.trim_end());
    }
    if !err.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(err.trim_end());
    }
    combined
}

fn resume_command_for_session(session: &Session, config: &crate::config::Config) -> Option<String> {
    let id = crate::app::bootstrap::terminal_goal_session_id(session).ok()?;
    let mut command = format!(
        "To resume this session: octet --resume {}",
        posix_shell_quote(&id)
    );

    let session_dir = absolute_resume_path(&config.session_dir, &config.invocation_cwd);
    let default_session_dir = absolute_resume_path(
        &crate::config::default_session_dir(),
        &config.invocation_cwd,
    );
    if session_dir != default_session_dir {
        command.push_str(" --session-dir ");
        command.push_str(&resume_scope_path(&session_dir, &config.invocation_cwd)?);
    }
    if config.workspace != config.invocation_cwd {
        command.push_str(" --workspace ");
        command.push_str(&resume_scope_path(
            &config.workspace,
            &config.invocation_cwd,
        )?);
    }
    Some(command)
}

/// Quote one argument for interpretation by a POSIX shell.
///
/// Single quotes preserve every character except an embedded single quote. At
/// each embedded quote, close the quoted section, emit the quote through an
/// unquoted backslash escape, and reopen the quoted section.
fn posix_shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

fn absolute_resume_path(path: &Path, cwd: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let mut absolute = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => absolute.push(prefix.as_os_str()),
            Component::RootDir => absolute.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                absolute.pop();
            }
            Component::Normal(component) => absolute.push(component),
        }
    }
    absolute
}

fn resume_scope_path(path: &Path, cwd: &Path) -> Option<String> {
    // Keep private absolute paths out of the notice. Resolve parent components
    // inside the user's shell before passing the path to SessionStore, which
    // deliberately rejects traversal components.
    let relative = relative_resume_path(cwd, path)?;
    let mut parents = Vec::new();
    let mut suffix = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::ParentDir if suffix.as_os_str().is_empty() => parents.push(".."),
            Component::Normal(component) => suffix.push(component),
            Component::CurDir => {}
            _ => return None,
        }
    }

    let base = if parents.is_empty() {
        "\"$(pwd -P)/\"".to_owned()
    } else {
        let parents = parents.join("/");
        format!("\"$(cd \"$(pwd -P)/{parents}\" && pwd -P)/\"")
    };
    let suffix = suffix.to_str()?;
    if suffix.is_empty() {
        return Some(base);
    }
    let suffix = suffix.replace('\'', "'\\''");
    Some(format!("{base}'{suffix}'"))
}

fn relative_resume_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = absolute_resume_path(from, from);
    let to = absolute_resume_path(to, from.as_path());
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in &from[common..] {
        match component {
            Component::Normal(_) => relative.push(".."),
            Component::CurDir => {}
            _ => return None,
        }
    }
    for component in &to[common..] {
        match component {
            Component::Normal(component) => relative.push(component),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

fn print_resume_command(command: Option<&str>) {
    // A coordinated signal is still a forced termination for the courtesy-line
    // contract. The signal owner will apply the conventional exit status after
    // frontend cleanup, so do not add output to that path.
    if crate::tui::terminal::received_shutdown_signal().is_none() {
        if let Some(command) = command {
            crate::output::stdout_line(command);
        }
    }
}

async fn shutdown_for_exit(app: &mut App) {
    if crate::tui::terminal::received_shutdown_signal().is_some() {
        octet_agent::extension_process::terminate_bash_process_groups(Duration::from_millis(400))
            .await;
        let _ = tokio::time::timeout(
            Duration::from_millis(1400),
            app.executable_extensions.shutdown(),
        )
        .await;
        octet_agent::extension_process::force_kill_registered_process_groups();
    } else {
        app.executable_extensions.shutdown().await;
    }
}

fn explicit_terminal_background_override() -> bool {
    std::env::var("OCTET_COLOR_SCHEME")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "dark" | "light" | "unknown" | "universal"
            )
        })
        .unwrap_or(false)
}

async fn apply_detected_terminal_background<S>(
    shell: &mut InteractiveShell,
    input: &mut EventStream<S>,
    config: &crate::config::Config,
) where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    if explicit_terminal_background_override()
        || TerminalThemeChoice::from_config(config)
            .and_then(TerminalThemeChoice::explicit_background)
            .is_some()
        || shell.theme().background() != TerminalBackground::Unknown
    {
        return;
    }
    if shell.theme().capabilities().color == crate::tui::terminal::ColorDepth::None {
        // NO_COLOR still gets the deterministic Auto fallback, but should not
        // receive a background query or any other colour-oriented control.
        return;
    }
    let Some((red, green, blue)) =
        crate::tui::terminal::query_terminal_background_color(input, Duration::from_millis(120))
            .await
    else {
        return;
    };
    let background = background_from_terminal_rgb(red, green, blue);
    shell.set_theme(load_theme_for_background(config, background));
}

fn terminal_theme_picker_data() -> (Vec<String>, Vec<Option<String>>) {
    let items = TerminalThemeChoice::all()
        .into_iter()
        .map(|choice| choice.label().to_owned())
        .collect();
    let descriptions = vec![
        Some(
            "Detect the terminal background; use a readable neutral fallback when unavailable"
                .into(),
        ),
        Some("Use light-terminal contrast without painting the terminal canvas".into()),
        Some("Use dark-terminal contrast without painting the terminal canvas".into()),
    ];
    (items, descriptions)
}

async fn pick_terminal_theme<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    config: &Config,
    onboarding: bool,
) -> anyhow::Result<Option<TerminalThemeChoice>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let (items, descriptions) = terminal_theme_picker_data();
    let current = TerminalThemeChoice::from_config(config).unwrap_or(TerminalThemeChoice::Auto);
    let original = shell.theme();
    let mut preview_config = config.clone();
    preview_config.theme = Some(TerminalThemeChoice::Auto.key().to_owned());
    // Preserve Auto's already-resolved background, including an earlier OSC
    // response. Otherwise use environment detection/fallback once; never query
    // the terminal while the picker owns its input stream.
    let auto = match current {
        TerminalThemeChoice::Auto => original.clone(),
        _ => load_theme(&preview_config),
    };
    let previews = TerminalThemeChoice::all().map(|choice| {
        if choice == TerminalThemeChoice::Auto {
            auto.clone()
        } else {
            preview_config.theme = Some(choice.key().to_owned());
            load_theme_for_background(&preview_config, auto.background())
        }
    });
    let title = if onboarding {
        "Choose terminal appearance"
    } else {
        "Terminal appearance"
    };
    let action = PanelAction::ProviderSetup(items.clone());
    let selected = pick_list_with_preview(
        shell,
        input,
        OrdinarySurfaceMetadata::new(title),
        items,
        descriptions,
        current.index(),
        action,
        |shell, index| {
            let theme = index.map_or(&original, |index| &previews[index]);
            shell.set_theme(theme.clone());
        },
    )
    .await;
    if !matches!(&selected, Ok(Some(_))) {
        // Escape, EOF, shutdown, coordinated close and input errors all leave
        // the original in-memory appearance intact, without saving a preview.
        shell.set_theme(original);
        shell.render();
    }
    selected.map(|index| index.map(|index| TerminalThemeChoice::all()[index]))
}

async fn configure_terminal_theme<S>(
    shell: &mut InteractiveShell,
    input: &mut EventStream<S>,
    config: &mut Config,
    requested: Option<String>,
    onboarding: bool,
    extensions: Option<&mut crate::extensions::ExecutableExtensions>,
) -> anyhow::Result<Option<TerminalThemeChoice>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let used_picker = requested.is_none();
    let selected = match requested {
        Some(value) => match TerminalThemeChoice::parse(&value) {
            Some(choice) => Some(choice),
            None => {
                shell.error(format!(
                    "invalid terminal appearance {value:?}; use /theme auto, /theme light, or /theme dark"
                ));
                shell.render();
                return Ok(None);
            }
        },
        None => pick_terminal_theme(shell, input, config, onboarding).await?,
    };
    // A first-run dismissal still commits the recommended default, so a user
    // who leaves from the picker is not forced through the same onboarding on
    // every launch. The caller still honors a pending close request.
    let Some(choice) = selected.or_else(|| onboarding.then_some(TerminalThemeChoice::Auto)) else {
        return Ok(None);
    };

    config.theme = Some(choice.key().to_owned());
    shell.set_runtime_config(config.clone());
    // A confirmed picker already installed the compiled appearance. Retain it
    // so confirming Auto does not discard its cached background resolution.
    if !used_picker || selected.is_none() {
        shell.set_theme(load_theme(config));
    }
    if choice == TerminalThemeChoice::Auto {
        apply_detected_terminal_background(shell, input, config).await;
    }
    if let Err(error) = persist_configuration(extensions, || {
        crate::cli::persist_theme_choice(choice.key())
    })
    .await
    {
        shell.error(format!("failed to save terminal appearance: {error}"));
    } else if !onboarding {
        shell.notice(format!("terminal appearance: {}", choice.label()));
    }
    shell.render();
    Ok(Some(choice))
}

fn startup_launch_outcome<T>(
    shell: &InteractiveShell,
    result: anyhow::Result<T>,
) -> anyhow::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(_) if shell.close_requested() => Ok(None),
        Err(error) => Err(error),
    }
}

async fn run_interactive_without_model(
    boot: Bootstrap,
    launch: crate::app::bootstrap::LaunchSelection,
    shell: &mut InteractiveShell,
    input: &mut EventStream,
) -> anyhow::Result<Option<String>> {
    let mut boot = boot;
    let workspace = boot.config.workspace.clone();
    let mut prepared = boot.take_prepared_session();
    let selection = launch.session;
    let session =
        run_blocking_startup_lifecycle(shell, input, STARTUP_SESSION_OPERATION, move || {
            crate::app::bootstrap::open_launch_session(&mut prepared, selection)
        })
        .await?;

    let resume_command = resume_command_for_session(&session, &boot.config);
    shell.set_identity("", "", "");
    shell.set_status_detail("no configured model · read-only session".to_owned());
    shell.set_workspace(workspace.clone());
    shell.set_input_modalities(octet_ai::ModalitySet::none());
    shell.set_session_telemetry(&session, None);
    shell.hydrate(&session)?;
    shell.notice(
        "No configured model. Use /login, /model, or /reload to configure one; prompts are disabled until then.",
    );
    // Keep onboarding and model-less prompt/template behavior unchanged, but
    // honor a positional read-only command once the session is ready.
    if boot.config.prompt_template.is_none()
        && boot
            .config
            .initial_prompt
            .as_deref()
            .is_some_and(|prompt| matches!(commands::parse(prompt), Command::Changelog))
    {
        shell.show_changelog();
    }
    // The same single ready frame as the model path: onboarding text and the
    // optional changelog surface arrive with readiness, never as a partial
    // paint before it.
    crate::app::bootstrap::startup_phase("frame.ready");
    shell.finish_startup();
    shell.render();

    let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
    scroll_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
    extension_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    // Model-less mode has no `App`, no agent, and no session, so there is
    // nothing a pass could re-read. The supervisor stays disabled and can never
    // report a due pass; keeping the same call shape means the idle loop has
    // exactly one arm set.
    let mut reload =
        crate::reload::ReloadSupervisor::new(crate::reload::ReloadSettings::disabled());
    let reload_watcher = crate::reload::ReloadWatcher::new(crate::reload::ReloadWatchSet::new());
    let mut reload_tick = tokio::time::interval(reload.settings().tick_interval());
    reload_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        match wait_for_prompt(
            shell,
            input,
            &mut scroll_tick,
            &mut extension_tick,
            &mut extensions,
            None,
            &mut reload_tick,
            &reload_watcher,
            &mut reload,
        )
        .await?
        {
            Idle::Quit => return Ok(resume_command),
            // Unreachable while the supervisor is disabled; kept explicit so the
            // variant stays honest instead of silently ignored.
            Idle::ReloadDue => {}
            Idle::CycleThinking => {
                shell.notice("thinking is unavailable until a model is configured");
                shell.render();
            }
            Idle::GoalContinuation => unreachable!("model-less mode has no goal deadline"),
            Idle::SessionLifecycle(request) => {
                request.respond(Err(ExtensionSessionLifecycleError::Unavailable));
            }
            Idle::Submit(_) => {
                shell.error(
                    "no configured model; set an API key and restart before submitting prompts"
                        .to_owned(),
                );
                shell.render();
            }
            Idle::Command(raw) => match commands::parse(&raw) {
                Command::Exit => return Ok(resume_command),
                Command::Hotkeys => {
                    show_hotkeys(shell);
                    shell.render();
                }
                Command::Copy => {
                    copy_last_assistant(shell);
                    shell.render();
                }
                Command::Session => {
                    shell.show_report_text(
                        "Session",
                        "Durable session facts",
                        commands::session_text(&session),
                    );
                    shell.render();
                }
                Command::Changelog => {
                    shell.show_changelog();
                    shell.render();
                }
                Command::Help(topic) => {
                    shell.show_report_text(
                        "Help",
                        "Browse commands and keyboard shortcuts",
                        commands::help_text(&workspace, topic.as_deref()),
                    );
                    shell.render();
                }
                Command::Status => {
                    shell.show_report_text(
                        "Status",
                        "Review the read-only session configuration",
                        "No model is configured. The session can be read, but prompts are disabled."
                            .to_owned(),
                    );
                    shell.render();
                }
                Command::Theme(requested) => {
                    configure_terminal_theme(
                        shell,
                        input,
                        &mut boot.config,
                        requested,
                        false,
                        None,
                    )
                    .await?;
                }
                Command::Login(provider) => match validate_provider(provider.as_deref()) {
                    Ok("codex") => {
                        if let Some(catalog) = login_codex_catalog(shell).await? {
                            boot.catalog = catalog;
                            shell.clear_error();
                            shell.notice(
                                "signed in to ChatGPT; use /model to select a model, then restart octet to chat",
                            );
                            shell.render();
                        }
                    }
                    Ok("custom") => {
                        login_custom(shell)?;
                        shell.render();
                    }
                    Ok(_) => unreachable!(),
                    Err(error) => {
                        shell.error(error.to_string());
                        shell.render();
                    }
                },
                Command::Model(model) => {
                    if boot.catalog.models().next().is_none() {
                        shell.notice(
                            "no configured models are available; use /login or edit the custom provider, then /reload",
                        );
                    } else {
                        let selected = match model {
                            Some(id) => {
                                let id = ModelId(id);
                                if boot.catalog.resolve(&id).is_err() {
                                    shell.error(format!("model {} is not available", id.0));
                                    None
                                } else {
                                    if let Err(error) = crate::cli::persist_model(&id.0) {
                                        shell.error(format!(
                                            "failed to save model preference: {error}"
                                        ));
                                    }
                                    Some(id)
                                }
                            }
                            None => optional_model_picker(shell, input, &boot.catalog).await?,
                        };
                        if let Some(model) = selected {
                            shell.notice(format!(
                                "model {} selected; restart octet to start chatting",
                                model.0
                            ));
                        }
                    }
                    shell.render();
                }
                Command::Reload => {
                    let catalog = run_blocking_lifecycle(
                        shell,
                        input,
                        "reloading models…",
                        crate::app::bootstrap::model_catalog,
                    )
                    .await?;
                    let has_models = catalog.models().next().is_some();
                    boot.catalog = catalog;
                    if has_models {
                        shell.notice(
                            "models reloaded; use /model to select one, then restart octet to chat",
                        );
                    } else {
                        shell.notice("model reload completed, but no configured models were found");
                    }
                    shell.render();
                }
                _ => {
                    shell.notice("this command is unavailable until a model is configured");
                    shell.render();
                }
            },
        }
    }
}

/// Dispatch positional release notes locally before a startup prompt or prewarm
/// can send context. Template expansions retain their explicit prompt semantics.
fn prepare_startup_input(
    app: &App,
    shell: &mut InteractiveShell,
    prompt: Option<String>,
) -> Option<ComposedInput> {
    if app.config.prompt_template.is_none()
        && prompt
            .as_deref()
            .is_some_and(|prompt| matches!(commands::parse(prompt), Command::Changelog))
    {
        shell.show_changelog();
        None
    } else {
        schedule_responses_prewarm(app);
        prompt.map(ComposedInput::from_text)
    }
}

fn schedule_idle_responses_prewarm(app: &App, command: &Command) {
    // Local controls/status must not submit provider requests. The next
    // normal request (including any later prewarm) reads the selected tier.
    if !matches!(
        command,
        Command::Changelog
            | Command::Fast(_)
            | Command::Hotkeys
            | Command::Copy
            | Command::Session
            | Command::Settings(_)
            | Command::ScopedModels(_)
            | Command::Bash(_)
    ) {
        schedule_responses_prewarm(app);
    }
}

fn schedule_responses_prewarm(app: &App) {
    let Ok(Some((client, model, request))) = app.agent.responses_prewarm_request() else {
        return;
    };
    tokio::spawn(async move {
        let _ = client.prewarm_responses(&model, request).await;
    });
}

#[derive(Clone, Copy)]
enum GuidedSetupPreset {
    LmStudio,
    OpenAiCompatible,
}

async fn guided_setup_input<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    prompt: &str,
    secret: bool,
) -> anyhow::Result<Option<String>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    // This is the existing bounded temporary-input surface. Secret values are
    // never copied into the ordinary composer or rendered frame.
    extension_input_picker(
        shell,
        input,
        &ExtensionInputRequest {
            parent_request_id: 0,
            prompt: prompt.to_owned(),
            secret,
        },
    )
    .await
}

fn valid_guided_manual_model(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// First-run interactive onboarding for an explicitly selected compatible
/// endpoint. Every path before `commit_and_rebuild` is in-memory only.
async fn guided_provider_setup(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    config: &crate::config::Config,
) -> anyhow::Result<Option<CompletedSetup>> {
    let mut replace_existing = false;
    'setup: loop {
        let Some(preset) = provider_setup_picker(
            shell,
            input,
            "Set up a provider",
            vec![
                "LM Studio".to_owned(),
                "OpenAI-compatible endpoint".to_owned(),
                "Cancel setup".to_owned(),
            ],
            vec![
                Some("Use an explicitly selected local OpenAI-compatible endpoint".to_owned()),
                Some("Use one endpoint you enter; octet will not scan for services".to_owned()),
                Some("Cancel local endpoint setup without writing provider data".to_owned()),
            ],
            0,
        )
        .await?
        else {
            return Ok(None);
        };
        let preset = match preset {
            0 => GuidedSetupPreset::LmStudio,
            1 => GuidedSetupPreset::OpenAiCompatible,
            _ => return Ok(None),
        };

        let endpoint = match preset {
            GuidedSetupPreset::LmStudio => {
                let Some(choice) = provider_setup_picker(
                    shell,
                    input,
                    "LM Studio endpoint",
                    vec![
                        "Use http://localhost:1234/v1/".to_owned(),
                        "Enter a different endpoint".to_owned(),
                        "Back".to_owned(),
                    ],
                    vec![
                        Some("The default LM Studio OpenAI-compatible server".to_owned()),
                        Some("Only the endpoint you enter will be contacted".to_owned()),
                        Some("Return to provider selection without writing state".to_owned()),
                    ],
                    0,
                )
                .await?
                else {
                    continue 'setup;
                };
                match choice {
                    0 => "http://localhost:1234/v1/".to_owned(),
                    1 => {
                        let Some(endpoint) =
                            guided_setup_input(shell, input, "Endpoint URL:", false).await?
                        else {
                            continue 'setup;
                        };
                        endpoint
                    }
                    _ => continue 'setup,
                }
            }
            GuidedSetupPreset::OpenAiCompatible => {
                let Some(endpoint) =
                    guided_setup_input(shell, input, "Endpoint URL:", false).await?
                else {
                    continue 'setup;
                };
                endpoint
            }
        };

        let Some(authentication_choice) = provider_setup_picker(
            shell,
            input,
            "Credential source",
            vec![
                "No authentication".to_owned(),
                "Enter an API key".to_owned(),
                "Read an API key from an environment variable".to_owned(),
                "Back".to_owned(),
            ],
            vec![
                Some("Do not send or store a credential".to_owned()),
                Some("The key is hidden and stored only after final confirmation".to_owned()),
                Some("The environment value is read only at runtime and is not stored".to_owned()),
                Some("Return to provider selection without writing state".to_owned()),
            ],
            0,
        )
        .await?
        else {
            continue 'setup;
        };
        let authentication = match authentication_choice {
            0 => SetupAuthentication::no_authentication(),
            1 => {
                let Some(value) = guided_setup_input(shell, input, "API key:", true).await? else {
                    continue 'setup;
                };
                SetupAuthentication::api_key(value)
            }
            2 => {
                let Some(variable) =
                    guided_setup_input(shell, input, "Environment variable name:", false).await?
                else {
                    continue 'setup;
                };
                SetupAuthentication::environment(variable)
            }
            _ => continue 'setup,
        };

        let is_default_lm_studio = matches!(preset, GuidedSetupPreset::LmStudio)
            && endpoint == "http://localhost:1234/v1/"
            && matches!(&authentication, SetupAuthentication::None);
        let draft = if is_default_lm_studio {
            SetupDraft::lm_studio().replace_existing(replace_existing)
        } else {
            let (provider_id, label) = match preset {
                GuidedSetupPreset::LmStudio => ("local", "LM Studio"),
                GuidedSetupPreset::OpenAiCompatible => ("custom", "OpenAI-compatible endpoint"),
            };
            SetupDraft::new(provider_id, label, endpoint, authentication)
                .replace_existing(replace_existing)
        };
        let service = ProviderSetupService::new(
            crate::auth::custom::CredentialStore::new(crate::auth::custom::default_path()),
            config.offline,
        );
        let transaction = match service.begin(draft) {
            Ok(transaction) => transaction,
            Err(error)
                if matches!(
                    &error,
                    ProviderSetupError::State(ProviderSetupState::ProviderAlreadyConfigured)
                ) =>
            {
                shell.error(error.to_string());
                shell.render();
                match provider_setup_picker(
                    shell,
                    input,
                    "Provider already configured",
                    vec![
                        "Replace the existing provider after review".to_owned(),
                        "Edit setup values".to_owned(),
                        "Cancel setup".to_owned(),
                    ],
                    vec![
                        Some(
                            "The existing provider is changed only after final confirmation"
                                .to_owned(),
                        ),
                        Some("Return to endpoint and credential selection".to_owned()),
                        Some("No provider state is written".to_owned()),
                    ],
                    0,
                )
                .await?
                {
                    Some(0) => {
                        replace_existing = true;
                        continue 'setup;
                    }
                    Some(1) => {
                        replace_existing = false;
                        continue 'setup;
                    }
                    _ => return Ok(None),
                }
            }
            Err(error) => {
                shell.error(error.to_string());
                shell.render();
                match provider_setup_picker(
                    shell,
                    input,
                    "Provider setup needs attention",
                    vec!["Edit setup values".to_owned(), "Cancel setup".to_owned()],
                    vec![
                        Some("Correct the endpoint or credential source and try again".to_owned()),
                        Some("No provider state is written".to_owned()),
                    ],
                    0,
                )
                .await?
                {
                    Some(0) => continue 'setup,
                    _ => return Ok(None),
                }
            }
        };

        let mut transaction = transaction;
        let mut discover = if config.offline {
            false
        } else {
            let Some(choice) =
                provider_setup_picker(
                    shell,
                    input,
                    "Model inventory",
                    vec![
                        "Discover models from this endpoint".to_owned(),
                        "Enter a model ID manually".to_owned(),
                        "Back".to_owned(),
                    ],
                    vec![
                    Some("Send one bounded request only to this selected endpoint's /models path"
                        .to_owned()),
                    Some("Do not contact the endpoint; useful for offline or unsupported discovery"
                        .to_owned()),
                    Some("Return to provider selection without writing state".to_owned()),
                ],
                    0,
                )
                .await?
            else {
                let _ = transaction.cancel();
                continue 'setup;
            };
            match choice {
                0 => true,
                1 => false,
                _ => {
                    let _ = transaction.cancel();
                    continue 'setup;
                }
            }
        };

        let prepared = 'prepare: loop {
            if !discover {
                let Some(model) = guided_setup_input(shell, input, "Model ID:", false).await?
                else {
                    let _ = transaction.cancel();
                    continue 'setup;
                };
                if !valid_guided_manual_model(&model) {
                    shell.error("provider setup configuration is invalid".to_owned());
                    shell.render();
                    continue 'prepare;
                }
                break 'prepare service.prepare_manual(transaction, &model)?;
            }

            let discovery_service = service.clone();
            let (returned_transaction, discovery) =
                run_blocking_lifecycle(shell, input, "discovering models…", move || {
                    let mut transaction = transaction;
                    let result = discovery_service.discover(&mut transaction);
                    Ok((transaction, result))
                })
                .await?;
            transaction = returned_transaction;
            match discovery {
                Ok(models) => {
                    let items = models
                        .iter()
                        .map(|model| model.display_name.clone())
                        .collect::<Vec<_>>();
                    let descriptions = models
                        .iter()
                        .map(|model| Some(model.api_name.clone()))
                        .collect::<Vec<_>>();
                    let Some(selected) = provider_setup_picker(
                        shell,
                        input,
                        "Select a discovered model",
                        items,
                        descriptions,
                        0,
                    )
                    .await?
                    else {
                        let _ = transaction.cancel();
                        continue 'setup;
                    };
                    break 'prepare service
                        .prepare_discovered(transaction, &models[selected].api_name)?;
                }
                Err(error) => {
                    let diagnostic = error
                        .state()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| error.to_string());
                    shell.error(diagnostic.clone());
                    shell.render();
                    match provider_setup_picker(
                        shell,
                        input,
                        "Model discovery needs attention",
                        vec![
                            "Retry this endpoint".to_owned(),
                            "Enter a model ID manually".to_owned(),
                            "Edit endpoint or credentials".to_owned(),
                            "Cancel setup".to_owned(),
                        ],
                        vec![
                            Some(diagnostic),
                            Some("Continue without any additional endpoint request".to_owned()),
                            Some("Return to provider selection without writing state".to_owned()),
                            Some("No provider state is written".to_owned()),
                        ],
                        0,
                    )
                    .await?
                    {
                        Some(0) => {
                            discover = true;
                            continue 'prepare;
                        }
                        Some(1) => {
                            discover = false;
                            continue 'prepare;
                        }
                        Some(2) => {
                            let _ = transaction.cancel();
                            continue 'setup;
                        }
                        _ => {
                            let _ = transaction.cancel();
                            return Ok(None);
                        }
                    }
                }
            }
        };

        let receipt = prepared
            .receipt()
            .render(Some(&SetupAuthority::from_config(config)));
        match provider_setup_picker(
            shell,
            input,
            "Review provider setup",
            vec![
                "Confirm and save".to_owned(),
                "Edit setup values".to_owned(),
                "Cancel setup".to_owned(),
            ],
            vec![
                Some(receipt),
                Some("Discard this review and return to provider selection".to_owned()),
                Some("Discard this review; no provider state is written".to_owned()),
            ],
            0,
        )
        .await?
        {
            Some(0) => {
                let commit_service = service.clone();
                match run_blocking_lifecycle(shell, input, "saving provider setup…", move || {
                    commit_service
                        .commit_and_rebuild(prepared)
                        .map_err(Into::into)
                })
                .await
                {
                    Ok(completed) => {
                        if let Err(error) = crate::cli::persist_model(&completed.model.0) {
                            shell.error(format!(
                                "provider saved, but the selected model preference could not be saved: {error}"
                            ));
                            shell.render();
                        }
                        return Ok(Some(completed));
                    }
                    Err(error) => {
                        shell.error(format!("provider setup could not be finalized: {error}"));
                        shell.render();
                        match provider_setup_picker(
                            shell,
                            input,
                            "Provider setup needs attention",
                            vec!["Start over".to_owned(), "Cancel setup".to_owned()],
                            vec![
                                Some(
                                    "Reload current provider state and review it again".to_owned(),
                                ),
                                Some("Return without making another change".to_owned()),
                            ],
                            0,
                        )
                        .await?
                        {
                            Some(0) => continue 'setup,
                            _ => return Ok(None),
                        }
                    }
                }
            }
            Some(1) => {
                let _ = prepared.cancel();
                continue 'setup;
            }
            _ => {
                let _ = prepared.cancel();
                return Ok(None);
            }
        }
    }
}

/// Own exactly one best-effort startup check. JoinSet's drop aborts it on every
/// exit path; neither input nor renderer work awaits release discovery.
fn startup_update_task(
    offline: bool,
    check: impl std::future::Future<Output = Option<semver::Version>> + Send + 'static,
    notify: impl FnOnce(semver::Version) + Send + 'static,
) -> tokio::task::JoinSet<()> {
    let mut tasks = tokio::task::JoinSet::new();
    if !offline {
        tasks.spawn(async move {
            if let Some(version) = check.await {
                notify(version);
            }
        });
    }
    tasks
}

/// Run the interactive frontend with explicit idle and active borrow phases.
pub async fn run_interactive(config: Config) -> anyhow::Result<()> {
    run_interactive_with_model_scope(config, None).await
}

/// Interactive launch retaining the ordered `--models` patterns, including
/// reasoning suffixes that cannot be recovered from a list of bare ModelIds.
///
/// A `/reload` that finds a changed on-disk executable unwinds the TUI and
/// re-execs into it. `exec` returns only on failure, so this wrapper rebuilds
/// the TUI and extension children from the durable session head and keeps the
/// current process usable.
pub async fn run_interactive_with_model_scope(
    config: Config,
    model_scope_patterns: Option<String>,
) -> anyhow::Result<()> {
    let mut config = config;
    loop {
        match run_interactive_once(config.clone(), model_scope_patterns.clone()).await? {
            InteractiveExit::Finished => return Ok(()),
            InteractiveExit::Reexec(plan) => {
                let session_id = plan.session_id().to_owned();
                // `exec` returns only when the image was not replaced: either
                // the platform call failed or the final probe→exec re-check
                // found the binary changed again. Both recover the same way.
                let notice = match plan.exec() {
                    crate::reexec::ReexecDecision::ExecFailed { notice, .. } => notice,
                    decision => match decision.notice() {
                        Some(notice) => notice.to_owned(),
                        None => continue,
                    },
                };
                // The terminal was left before `exec`. Re-entering the loop
                // rebuilds the TUI and extension children from the durable
                // session head and resumes the exact session, so the process
                // stays usable.
                crate::output::stderr_line(notice);
                config.initial_prompt = None;
                config.resume = crate::config::ResumeSelector::Resume(Some(session_id));
            }
        }
    }
}

/// One interactive process lifetime. Returns after the terminal was left.
async fn run_interactive_once(
    config: Config,
    model_scope_patterns: Option<String>,
) -> anyhow::Result<InteractiveExit> {
    let mut config = config;
    let initial_prompt = config.initial_prompt.clone();
    let theme = load_theme(&config);
    let size = Arc::new(Mutex::new(crossterm::terminal::size().unwrap_or((80, 24))));
    let mut shell =
        InteractiveShell::enter_with_mouse(theme, size, config.mouse.application_owned())?;
    // Best-effort re-exec support: a host that cannot report its own executable
    // keeps the resources-only `/reload`.
    let mut reexec = crate::reexec::ReexecController::capture().ok();
    shell.set_runtime_config(config.clone());
    let _update_task = startup_update_task(
        config.offline,
        crate::update::startup_available_update(),
        shell.startup_update_notifier(),
    );
    let mut input = EventStream::new().with_cede_flag(shell.terminal_input_parking());
    apply_detected_terminal_background(&mut shell, &mut input, &config).await;
    if crate::cli::should_offer_theme_onboarding(&config)
        && shell.theme().capabilities().interactive
        && !config.plain
    {
        configure_terminal_theme(&mut shell, &mut input, &mut config, None, true, None).await?;
        if shell.close_requested() {
            shell.leave();
            return Ok(InteractiveExit::Finished);
        }
    }
    // Cold/expired model inventories can require network discovery. Give the
    // terminal an input owner before that work, just as for session/extension
    // startup below. Editing is live; submission still waits for full startup.
    // Startup renders nothing: the phase is named only in diagnostics, and its
    // duration stays attributable through `OCTET_STARTUP_TRACE=1`.
    let mut boot = run_blocking_startup_lifecycle(
        &mut shell,
        &mut input,
        STARTUP_MODELS_OPERATION,
        move || crate::app::bootstrap::bootstrap(config),
    )
    .await?;
    if shell.close_requested() {
        shell.leave();
        return Ok(InteractiveExit::Finished);
    }
    // Onboarding keeps explicit selection and resumed provenance authoritative.
    // Cloud authentication refreshes this bootstrap before the ordinary model
    // picker runs; local setup retains its reviewed default model.
    onboarding::run(&mut shell, &mut input, &mut boot).await?;
    if shell.close_requested() {
        shell.leave();
        return Ok(InteractiveExit::Finished);
    }
    // The shell owns a dedicated renderer thread, but sexy-tui still renders
    // synchronously when that thread receives a request. This clock only
    // coalesces high-rate wheel input on the input loop.
    let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
    scroll_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
    extension_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let launch_result = resolve_launch_interactive(&boot, &mut shell, &mut input).await;
    let Some(launch) = startup_launch_outcome(&shell, launch_result)? else {
        shell.leave();
        return Ok(InteractiveExit::Finished);
    };
    if boot.is_modeless() {
        let result = run_interactive_without_model(boot, launch, &mut shell, &mut input).await;
        shell.leave();
        return match result {
            Ok(resume_command) => {
                print_resume_command(resume_command.as_deref());
                Ok(InteractiveExit::Finished)
            }
            Err(error) => Err(error),
        };
    }
    let mut app =
        run_blocking_startup_lifecycle(&mut shell, &mut input, STARTUP_APP_OPERATION, move || {
            let system = compose_instructions(&boot.config)?;
            build_app(boot, launch, system)
        })
        .await?;
    // `--models` always wins. Without it, the interactive launch reads the
    // user-level ordered scope persisted by `/scoped-models`; headless modes
    // never consult it. A hand-edited invalid value warns instead of breaking
    // startup, and the command line still fails closed on the same input.
    match model_scope_patterns.clone() {
        Some(patterns) => app.set_model_scope_patterns(Some(&patterns))?,
        None => {
            if let Some(persisted) = crate::cli::persisted_scoped_models() {
                if let Err(error) = app.set_model_scope_patterns(Some(&persisted)) {
                    shell.notice(format!("ignored the persisted model scope: {error}"));
                }
            }
        }
    }
    let mut startup_prompt = initial_prompt;
    if let Some(name) = app.config.prompt_template.clone() {
        let arguments = startup_prompt.take().unwrap_or_default();
        let rendered =
            expand_prompt_invocation(&mut app, &format!("{name} {arguments}"), false, None)?
                .ok_or_else(|| anyhow::anyhow!("prompt template name is missing"))?;
        if app.config.debug_prompt {
            shell.show_overlay_text(crate::prompts::debug_expansion(&rendered));
        }
        startup_prompt = Some(rendered.text);
    }
    // One atomic ready frame. History, identity, status, extension UI and the
    // startup prompt are all installed before `finish_startup` opens the
    // branded surface, so the terminal never sees an incremental
    // partial-then-corrected paint. Hydration cost stays attributable
    // off-screen through `OCTET_STARTUP_TRACE=1`.
    crate::app::bootstrap::startup_phase("history.hydrate");
    shell.hydrate(app.agent.session())?;
    app.executable_extensions
        .activate_session_lifecycle_driver();
    update_status(&mut shell, &app);
    request_extension_ui(&mut shell, &mut app);
    // The pane is ready for input. Reporting here (not earlier) means the
    // session identity is already resolved, and a headless/plain run never
    // reaches this point at all — the equivalent of Pi's TUI-only gate.
    shell.herdr_attach(
        herdr_session_id(&app),
        herdr_startup_source(&app.config.resume),
        herdr_launch_scope(&app),
    );
    let mut startup_input = prepare_startup_input(&app, &mut shell, startup_prompt);
    crate::app::bootstrap::startup_phase("frame.ready");
    shell.finish_startup();
    shell.render();

    let mut pending_actions = VecDeque::new();
    let mut goal_deadline = recovered_goal_deadline(&app)?;
    let mut next_prompt_source = GoalTurnSource::User;
    // A validated re-exec: the loop breaks, the TUI is left, and the wrapper
    // above replaces the process image (or recovers from a failed exec).
    let mut pending_reexec: Option<crate::reexec::ReexecPlan> = None;
    // Live-reload supervisor (`crates/octet-coding-agent/src/reload.rs`).
    //
    // Sampling is metadata-only, gated by the poll interval, and every pass is
    // applied only at this idle boundary: a run owns the session inside the
    // `Idle::Submit` arm below, which never reaches the supervisor. The
    // `reload`, `reload_poll_ms`, `reload_debounce_ms`, and `reload_max_files`
    // keys come from the user-level config layer, and `ReloadWatcher::poll`
    // reads both the interval and the inspection budget from the supervisor, so
    // exactly one `ReloadSettings` value is in play. The supervisor announces
    // itself once, on its first observation.
    let mut reload = crate::reload::ReloadSupervisor::new(crate::cli::live_reload_settings());
    let reload_watcher =
        crate::reload::ReloadWatcher::new(crate::reload::ReloadWatchSet::from_config(&app.config));
    let mut reload_tick = tokio::time::interval(reload.settings().tick_interval());
    reload_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    'interactive: loop {
        if shell.close_requested() {
            shutdown_for_exit(&mut app).await;
            break;
        }
        let queued_input = if startup_input.is_none() {
            shell.take_ready_follow_up()
        } else {
            None
        };
        let queued_submission = queued_input.is_some();
        let idle = match startup_input.take().or(queued_input) {
            Some(input) if !input.is_empty() => Idle::Submit(input),
            _ => {
                wait_for_prompt(
                    &mut shell,
                    &mut input,
                    &mut scroll_tick,
                    &mut extension_tick,
                    &mut app.executable_extensions,
                    goal_deadline,
                    &mut reload_tick,
                    &reload_watcher,
                    &mut reload,
                )
                .await?
            }
        };
        match idle {
            Idle::Quit => {
                shutdown_for_exit(&mut app).await;
                break;
            }
            Idle::SessionLifecycle(request) => {
                let active_session_replaced =
                    execute_extension_session_lifecycle(&mut app, &mut shell, &mut input, request)
                        .await;
                if active_session_replaced {
                    // Replacement installs a fresh GoalDriver. Re-arm it from
                    // the current durable session instead of letting a stale
                    // deadline target the discarded driver.
                    goal_deadline = recovered_goal_deadline(&app)?;
                    next_prompt_source = GoalTurnSource::User;
                    // `session/switch` and `session/reload` are the only
                    // lifecycle operations that replace the active durable
                    // session; `create`/`fork` return dormant ids without
                    // switching. Herdr's stored reference follows the active
                    // session, exactly as the Pi integration refreshes it.
                    shell.herdr_session_changed(
                        herdr_session_id(&app),
                        Some("resume"),
                        herdr_launch_scope(&app),
                    );
                }
            }
            // The one place a live-reload pass is applied. Only idle-boundary
            // code reaches this arm, and `begin` refuses anything but `Idle`.
            Idle::ReloadDue => {
                let now = std::time::Instant::now();
                let Some(plan) = reload.begin(now, crate::reload::ReloadBoundary::Idle) else {
                    continue;
                };
                app = apply_live_reload_plan(
                    app,
                    &mut shell,
                    &mut input,
                    &mut reload,
                    plan,
                    reexec.as_mut(),
                    &mut pending_reexec,
                )
                .await?;
                if pending_reexec.is_some() {
                    break 'interactive;
                }
            }
            Idle::GoalContinuation => {
                goal_deadline = None;
                match app.goal_driver.fire_continuation() {
                    Ok(Some(continuation)) => {
                        next_prompt_source = GoalTurnSource::Continuation;
                        startup_input = Some(ComposedInput::from_text(continuation.prompt));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = app.goal_driver.session_error();
                        shell.error(format!("goal continuation unavailable: {error}"));
                        shell.render();
                    }
                }
            }
            Idle::CycleThinking => {
                let level = next_thinking_level(&app)?;
                let reasoning = requested_thinking_to_reasoning(
                    level,
                    &app.model,
                    app.subagents_available(),
                )?;
                app =
                    select_thinking(app, &mut shell, &mut input, reasoning, None).await?;
                schedule_responses_prewarm(&app);
                shell.render();
            }
            Idle::Command(command_input) => {
                if command_input.trim_start().starts_with("/skill:") {
                    startup_input = Some(ComposedInput::from_text(command_input));
                    continue;
                }
                // The supervisor's explicit user actions. Plain `/reload` keeps
                // its existing transactional path (`run_idle_command` below);
                // only these two documented flags hand control to the
                // live-reload supervisor, so a forced pass can never be
                // triggered by the watcher noticing a file change.
                if let Some(action) = crate::reload::ReloadUserAction::parse(&command_input) {
                    let now = std::time::Instant::now();
                    match action {
                        crate::reload::ReloadUserAction::DryRun => {
                            shell.notice(reload_watcher.watches().arming_notice(reload.settings()));
                            let report = reload.dry_run(now, crate::reload::ReloadBoundary::Idle);
                            for notice in reload_interruption_preview(&app) {
                                shell.notice(notice);
                            }
                            for notice in report.notices() {
                                shell.notice(notice);
                            }
                            shell.notice(report.summary());
                            shell.render();
                        }
                        crate::reload::ReloadUserAction::Force => {
                            // Current queued work is a risk, not an actual loss.
                            for notice in reload_interruption_preview(&app) {
                                shell.notice(notice);
                            }
                            let plan = reload.force();
                            app = apply_live_reload_plan(
                                app,
                                &mut shell,
                                &mut input,
                                &mut reload,
                                plan,
                                reexec.as_mut(),
                                &mut pending_reexec,
                            )
                            .await?;
                            if pending_reexec.is_some() {
                                break 'interactive;
                            }
                        }
                    }
                    continue;
                }
                let command = commands::parse(&command_input);
                match run_idle_command(
                    app,
                    &mut shell,
                    &mut input,
                    command.clone(),
                    &mut goal_deadline,
                    reexec.as_mut(),
                    &mut reload,
                )
                .await?
                {
                    IdleCommandOutcome::Continue(next) => {
                        app = *next;
                        schedule_idle_responses_prewarm(&app, &command);
                    }
                    IdleCommandOutcome::Submit { app: next, input } => {
                        app = *next;
                        schedule_responses_prewarm(&app);
                        startup_input = Some(input);
                    }
                    IdleCommandOutcome::Reexec { app: next, plan } => {
                        app = *next;
                        pending_reexec = Some(plan);
                        break 'interactive;
                    }
                    IdleCommandOutcome::Quit(next) => {
                        app = *next;
                        shutdown_for_exit(&mut app).await;
                        break;
                    }
                }
            }
            Idle::Submit(mut composed) => {
                let prompt_source = next_prompt_source;
                next_prompt_source = GoalTurnSource::User;
                if prompt_source == GoalTurnSource::User {
                    goal_deadline = None;
                    app.goal_driver.user_spoke();
                }
                // Shell escapes have the same authority as the model `bash`
                // tool and executable extensions. Never let this local UX bypass
                // the product-wide process gate, the ordinary process approval,
                // or bounded process cleanup. Active-run follow-ups are model
                // input, never delayed local shell escapes that gain process
                // authority on dispatch.
                if !queued_submission {
                    if let Some(escape) = commands::BashEscape::parse(&composed.display_text) {
                        match run_idle_command(
                            app,
                            &mut shell,
                            &mut input,
                            Command::Bash(escape),
                            &mut goal_deadline,
                            None,
                            &mut reload,
                        )
                        .await?
                        {
                            IdleCommandOutcome::Continue(next) => {
                                app = *next;
                                continue;
                            }
                            IdleCommandOutcome::Quit(next) => {
                                app = *next;
                                shutdown_for_exit(&mut app).await;
                                break 'interactive;
                            }
                            IdleCommandOutcome::Submit { app: next, input } => {
                                app = *next;
                                startup_input = Some(input);
                                continue;
                            }
                            IdleCommandOutcome::Reexec { app: next, .. } => {
                                // A bash escape cannot plan a re-exec; keep the
                                // application and continue instead of dropping it.
                                app = *next;
                                continue;
                            }
                        }
                    }
                }
                if let Some(message) = cost_limit_message(&app) {
                    shell.restore_composed(composed);
                    shell.error(message);
                    shell.render();
                    continue;
                }
                let model_prompt = match expand_skill_command(
                    app.skills.as_ref(),
                    &composed.transcript_text,
                    &app.agent.registered_tool_names(),
                ) {
                    Ok(Some(expanded)) => expanded,
                    Ok(None) => composed.transcript_text.clone(),
                    Err(error) => {
                        shell.restore_composed(composed);
                        shell.error(format!("skill invocation failed: {error}"));
                        shell.render();
                        continue;
                    }
                };
                app.executable_extensions.refresh_host_state(
                    app.agent.session(),
                    &app.model,
                    &app.reasoning,
                    &app.sessions,
                );
                let composition = tokio::select! {
                    biased;
                    _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                        shell.restore_composed(composed);
                        shutdown_for_exit(&mut app).await;
                        break 'interactive;
                    }
                    result = await_with_ctrl_c(
                        app.executable_extensions.compose_prompt(
                            &app.system,
                            model_prompt,
                        ),
                        &mut shell,
                        &mut input,
                    ) => result,
                };
                let Some(composition) = composition else {
                    shell.restore_composed(composed);
                    shell.notice("extension prompt composition cancelled");
                    shell.render();
                    continue;
                };
                let composition = match composition {
                    Ok(composition) => composition,
                    Err(error) => {
                        shell.restore_composed(composed);
                        shell.error(format!("extension prompt composition failed: {error}"));
                        shell.render();
                        continue;
                    }
                };
                let pending_context_count = composition.pending_context_count;
                for notification in composition.notifications {
                    shell.notice(notification);
                }
                app.agent.set_system_prompt(composition.system);
                let answer_only = composed.answer_only;
                let retry_composed = composed.clone();
                composed.replace_model_text(composition.prompt);
                // Keep extension context in the replayable model message, but
                // persist the exact user-facing draft separately for title and
                // transcript reconstruction.
                app.agent.set_owner_tool_images_enabled(true);
                app.agent
                    .set_prompt_display_text(Some(composed.transcript_text.clone()));
                // Capacity checks and autonomous compaction live inside the
                // cancellable Agent run. Frontends must not start an
                // unabortable provider request before RunControl exists.
                shell.set_context_estimate(
                    estimate_next_request_tokens(&app, &composed.parts),
                    context_window(&app.model),
                );
                // Do this directly at the request boundary. A provider update
                // can remove the old host-stream transport; never let an Agent
                // retaining its inert endpoint fall through to localhost.
                let provider_diagnostics =
                    match app.synchronize_extension_provider_catalog_for_request() {
                        Ok(diagnostics) => diagnostics,
                        Err(error) => {
                            app.agent.set_system_prompt(app.system.clone());
                            shell.restore_composed(retry_composed);
                            shell.error(format!("prompt was not saved: {error}"));
                            shell.render();
                            continue;
                        }
                    };
                for diagnostic in provider_diagnostics {
                    shell.notice(diagnostic);
                }

                // Capture the pane's session identity before the run borrows
                // the agent mutably: Herdr must learn the session reference at
                // the same moment the prompt is accepted.
                let pane_session_id = herdr_session_id(&app);
                // Snapshot the read-only application facts the run cannot lend
                // out (it owns `&mut Agent`), so inspection commands still work.
                let inspection = ActiveRunInspection::capture(&app);
                let mut run = {
                    let user_input = composed.into_user_input();
                    let run_result = if answer_only {
                        app.agent.prompt_without_tools(user_input).await
                    } else {
                        app.agent.prompt(user_input).await
                    };
                    match run_result {
                        Ok(run) => run,
                        Err(error) => {
                            // No context commit occurred. The restored draft's next
                            // attempt recomposes from `app.system` and overwrites
                            // this transient composed Agent system before append.
                            shell.restore_composed(retry_composed);
                            let error = octet_agent::public_error_diagnostic(
                                &error,
                                &app.model.endpoint.id.0,
                                &app.model.spec.id.0,
                            );
                            shell.error(format!("prompt was not saved: {error}"));
                            shell.render();
                            continue;
                        }
                    }
                };
                let extension_turn = app.executable_extensions.begin_turn().await;
                app.executable_extensions
                    .commit_prompt_context(pending_context_count);
                prepare_prompt(&mut shell);
                shell.on_composed_prompt_submitted(&retry_composed);
                let run_id = shell.begin_run(&app.model.endpoint.id.0);
                // Working from the moment the prompt is accepted, before any
                // provider event: Pi reports `working` on `agent_start`.
                shell.herdr_run_started(pane_session_id);
                shell.mark_prompt_persisted();
                shell.set_awaiting_provider(run_id);
                shell.render();
                let control = run.control();
                let mut quit_requested = false;
                let mut made_tool_call = false;
                let ended = drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut scroll_tick,
                    &mut pending_actions,
                    &mut quit_requested,
                    app.config.max_cost_microdollars,
                    app.config.cost_warning_microdollars,
                    &mut app.executable_extensions,
                    &mut made_tool_call,
                    &inspection,
                    &mut goal_deadline,
                )
                .await?;
                drop(run);
                if app.model.responses_features().reasoning_effort_updates {
                    app.reasoning = app.agent.reasoning().clone();
                    update_status(&mut shell, &app);
                    app.executable_extensions.notify_reasoning_selected_all();
                }
                app.executable_extensions
                    .settle_turn(extension_turn, &ended)
                    .await;
                app.agent.set_system_prompt(app.system.clone());
                if crate::tui::terminal::received_shutdown_signal().is_some() {
                    shutdown_for_exit(&mut app).await;
                    break 'interactive;
                }
                let goal_decision = if ended.allows_after_response() {
                    let response = crate::extensions::latest_assistant_text(app.agent.session());
                    let notifications = tokio::select! {
                        biased;
                        _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                            shutdown_for_exit(&mut app).await;
                            break 'interactive;
                        }
                        result = await_with_ctrl_c(
                            app.executable_extensions.after_response(&response),
                            &mut shell,
                            &mut input,
                        ) => result,
                    };
                    if let Some(notifications) = notifications {
                        for notification in notifications {
                            shell.notice(notification);
                        }
                    } else {
                        shell.notice("extension after_response hooks cancelled");
                    }
                    settle_goal(
                        &app,
                        &mut shell,
                        prompt_source,
                        &response,
                        made_tool_call,
                        true,
                    )
                } else {
                    settle_goal(&app, &mut shell, prompt_source, "", made_tool_call, false)
                };
                goal_deadline = match goal_decision {
                    Some(GoalDecision::Wait { delay, .. }) => Some(Instant::now() + delay),
                    Some(GoalDecision::Complete) => {
                        shell.notice("goal completed");
                        None
                    }
                    Some(GoalDecision::Blocked) => {
                        shell.notice("goal blocked");
                        None
                    }
                    Some(GoalDecision::BudgetLimited) => {
                        shell.notice("goal continuation budget exhausted");
                        None
                    }
                    Some(GoalDecision::Suppressed) => None,
                    Some(GoalDecision::Paused) | Some(GoalDecision::Inactive) | None => None,
                };
                // The run's tools may have created files; refresh mention
                // completion lazily on the next `@`.
                shell.invalidate_file_index();
                update_status(&mut shell, &app);
                request_extension_ui(&mut shell, &mut app);
                // `drive_active_run` settles the semantic outcome, while these
                // idle-boundary refreshes settle the final composer/footer.
                // Always publish that complete frame even when no queued idle
                // action follows to trigger another render.
                shell.render();
                if shell.close_requested() {
                    shutdown_for_exit(&mut app).await;
                    break 'interactive;
                }
                if quit_requested {
                    shutdown_for_exit(&mut app).await;
                    break;
                }
                app = apply_pending_actions(
                    app,
                    &mut shell,
                    &mut input,
                    &mut pending_actions,
                    &mut goal_deadline,
                    reexec.as_mut(),
                    &mut pending_reexec,
                    &mut reload,
                )
                .await?;
                if pending_reexec.is_some() {
                    break 'interactive;
                }
            }
        }
    }
    let resume_command = resume_command_for_session(app.agent.session(), &app.config);
    shell.leave();
    print_resume_command(resume_command.as_deref());
    match pending_reexec {
        Some(plan) => Ok(InteractiveExit::Reexec(plan)),
        None => Ok(InteractiveExit::Finished),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn explicit_resource_reload_clears_only_reinitialized_extension_problems() {
        use crate::reload::{ReloadComponent, ReloadSettings, ReloadSupervisor};
        let mut reload = ReloadSupervisor::new(ReloadSettings::default());
        let before: std::collections::BTreeMap<String, (String, u64)> =
            std::collections::BTreeMap::from([
                ("replaced".into(), ("old".into(), 1)),
                ("retained".into(), ("shared".into(), 1)),
                ("stopped".into(), ("old-stopped".into(), 1)),
            ]);
        let after = std::collections::BTreeMap::from([
            ("replaced".into(), ("new".into(), 1)),
            ("retained".into(), ("shared".into(), 1)),
        ]);
        let problem = || vec!["fixture failure".to_owned()];
        for name in before.keys() {
            reload.checked_problems(ReloadComponent::Extension(name.clone()), problem(), false);
        }
        reload.checked_problems(ReloadComponent::Host, problem(), false);
        remember_rebuilt_extensions(&mut reload, &before, &after);
        assert!(!reload
            .checked_problems(
                ReloadComponent::Extension("replaced".into()),
                problem(),
                false
            )
            .is_empty());
        for name in ["retained", "stopped"] {
            assert!(reload
                .checked_problems(ReloadComponent::Extension(name.into()), problem(), false)
                .is_empty());
        }
        assert!(reload
            .checked_problems(ReloadComponent::Host, problem(), false)
            .is_empty());
    }

    #[test]
    fn automatic_reload_cannot_prompt_for_binary_or_worker_consent() {
        assert!(!HostPass::ResourcesOnly.may_prompt());
        assert!(!HostPass::Allowed {
            redirect_confirmed: false
        }
        .may_prompt());
        assert!(HostPass::Allowed {
            redirect_confirmed: true
        }
        .may_prompt());
    }

    #[test]
    fn automatic_extension_reload_silences_success_but_preserves_each_event() {
        use crate::extensions::{ExtensionReloadReport, ExtensionRescanReport};
        use crate::reload::{ReloadSettings, ReloadSupervisor};
        let mut reload = ReloadSupervisor::new(ReloadSettings::default());
        let success = |generation| ExtensionReloadReport {
            processes: vec![(
                "fixture".into(),
                Ok(format!("reloaded fixture generation {generation}")),
            )],
            rescans: ExtensionRescanReport {
                checked: vec![("resource:fixture".into(), Vec::new())],
                details: vec![format!("rescanned fixture generation {generation}")],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(extension_reload_notices(&mut reload, success(1), false).is_empty());
        assert!(extension_reload_notices(&mut reload, success(2), false).is_empty());
        assert_eq!(
            extension_reload_notices(&mut reload, success(3), true).len(),
            2
        );
        let failure = || ExtensionReloadReport {
            processes: vec![("fixture".into(), Err("fixture unavailable".into()))],
            events: vec![
                "discarded host request".into(),
                "discarded host request".into(),
            ],
            ..Default::default()
        };
        assert_eq!(
            extension_reload_notices(&mut reload, failure(), false).len(),
            3
        );
        assert_eq!(
            extension_reload_notices(&mut reload, failure(), false).len(),
            2
        );
        // No process check in this pass: the previous failure remains remembered.
        extension_reload_notices(&mut reload, ExtensionReloadReport::default(), false);
        assert_eq!(
            extension_reload_notices(&mut reload, failure(), false).len(),
            2
        );
        assert_eq!(
            extension_reload_notices(&mut reload, success(4), true).len(),
            2
        );
        assert_eq!(
            extension_reload_notices(&mut reload, failure(), false).len(),
            3
        );
        assert_eq!(
            extension_reload_notices(&mut reload, failure(), true).len(),
            3
        );
    }

    use super::*;
    use octet_agent::EntryValue;

    fn test_theme() -> crate::tui::theme::OctetTheme {
        crate::tui::theme::test_theme()
    }

    pub(super) fn terminal_theme_test_config(workspace: PathBuf) -> Config {
        use crate::config::{CompactionPolicy, Mode, ResumeSelector, SandboxPolicy};

        Config {
            workspace: workspace.clone(),
            invocation_cwd: workspace,
            model: None,
            model_explicit: false,
            reasoning: None,
            reasoning_explicit: false,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
            reasoning_mode_explicit: false,
            cache_retention: octet_ai::CacheRetention::Short,
            effect_policy: octet_agent::EffectPolicy::Controlled,
            sandbox: SandboxPolicy::default(),
            theme: None,
            system_prompt: None,
            theme_paths: vec![],
            color: crate::config::ColorMode::Auto,
            mouse: crate::config::MouseMode::Auto,
            plain: false,
            show_images: false,
            session_dir: PathBuf::from("sessions"),
            compaction: CompactionPolicy::default(),
            max_cost_microdollars: None,
            cost_warning_microdollars: None,
            max_turns: Some(40),
            show_reasoning_in_print: false,
            initial_prompt: None,
            prompt_template: None,
            debug_prompt: false,
            prompt_paths: vec![],
            mode: Mode::Interactive,
            resume: ResumeSelector::New,
            skill_paths: vec![],
            extension_paths: vec![],
            enabled_extensions: vec![],
            extension_activation_overridden: false,
            trusted_extensions: vec![],
            invocation_trusted_extensions: vec![],
            experimental_streamable_http_mcp: false,
            extension_flag_values: Default::default(),
            tools: crate::config::ToolPolicy::default(),
            telemetry: None,
            context_files: true,
            offline: true,
            workspace_trusted: true,
        }
    }

    fn theme_picker_key(code: KeyCode) -> std::io::Result<Event> {
        Ok(Event::Key(crossterm::event::KeyEvent::new(
            code,
            KeyModifiers::NONE,
        )))
    }

    #[tokio::test]
    async fn terminal_theme_picker_confirms_compiled_previews_without_changing_config() {
        for choice in TerminalThemeChoice::all() {
            let mut config = terminal_theme_test_config(PathBuf::from("."));
            config.theme = Some("auto".into());
            let mut shell = InteractiveShell::test_shell();
            let mut original = crate::tui::theme::test_theme_for(
                TerminalBackground::Dark,
                shell.theme().capabilities(),
            );
            original.override_token("foreground", "#123456");
            shell.set_theme(original.clone());
            let mut events = vec![
                theme_picker_key(KeyCode::End),
                theme_picker_key(KeyCode::Home),
            ];
            for _ in 0..choice.index() {
                events.push(theme_picker_key(KeyCode::Down));
            }
            // Filtering leaves a single displayed row whose original index
            // is still the choice's index, not necessarily zero.
            events.extend(
                choice
                    .key()
                    .chars()
                    .map(|key| theme_picker_key(KeyCode::Char(key))),
            );
            events.push(theme_picker_key(KeyCode::Enter));
            let mut input = tokio_stream::iter(events);

            assert_eq!(
                pick_terminal_theme(&mut shell, &mut input, &config, false)
                    .await
                    .unwrap(),
                Some(choice)
            );
            assert_eq!(
                shell.theme().background(),
                choice
                    .explicit_background()
                    .unwrap_or(TerminalBackground::Dark),
                "Auto must retain its already-resolved background after visiting other rows"
            );
            if choice == TerminalThemeChoice::Auto {
                assert_eq!(shell.theme().capabilities(), original.capabilities());
                assert_eq!(
                    shell.theme().role_rgb("foreground"),
                    original.role_rgb("foreground")
                );
            }
            assert_eq!(config.theme.as_deref(), Some("auto"));
            assert!(!shell.has_panel());
        }
    }

    #[tokio::test]
    async fn terminal_theme_preview_restores_on_cancel_eof_error_and_close() {
        for exit in ["escape", "eof", "error", "close"] {
            let mut config = terminal_theme_test_config(PathBuf::from("."));
            config.theme = Some("light".into());
            let mut shell = InteractiveShell::test_shell();
            let mut original = crate::tui::theme::test_theme_for(
                TerminalBackground::Light,
                shell.theme().capabilities(),
            );
            original.override_token("foreground", "#123456");
            shell.set_theme(original.clone());
            let mut events = vec![theme_picker_key(KeyCode::End)];
            match exit {
                "escape" => events.push(theme_picker_key(KeyCode::Esc)),
                "eof" => {}
                "error" => events.push(Err(std::io::Error::other("preview input failed"))),
                "close" => events.push(Ok(Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('d'),
                    KeyModifiers::CONTROL,
                )))),
                _ => unreachable!(),
            }
            let mut input = EventStream::from_stream(tokio_stream::iter(events));
            // Exercise the configuration boundary too: none of these /theme
            // outcomes may reach its persistence branch or mutate the config.
            let result =
                configure_terminal_theme(&mut shell, &mut input, &mut config, None, false, None)
                    .await;
            if exit == "error" {
                assert_eq!(result.unwrap_err().to_string(), "preview input failed");
            } else {
                assert_eq!(result.unwrap(), None);
            }
            assert_eq!(shell.theme().background(), original.background(), "{exit}");
            assert_eq!(
                shell.theme().role_rgb("foreground"),
                original.role_rgb("foreground"),
                "{exit}"
            );
            assert_eq!(
                shell.theme().capabilities(),
                original.capabilities(),
                "{exit}"
            );
            assert_eq!(config.theme.as_deref(), Some("light"), "{exit}");
            assert_eq!(shell.close_requested(), exit == "close");
            assert!(!shell.has_panel());
        }
    }

    #[tokio::test]
    async fn terminal_theme_onboarding_dismissal_returns_no_preview_choice() {
        let config = terminal_theme_test_config(PathBuf::from("."));
        let mut shell = InteractiveShell::test_shell();
        let original = shell.theme();
        let mut input = tokio_stream::iter([
            theme_picker_key(KeyCode::End),
            theme_picker_key(KeyCode::Esc),
        ]);
        // configure_terminal_theme retains its dismissal-to-Auto fallback;
        // the highlighted Dark preview must not masquerade as confirmation.
        assert_eq!(
            pick_terminal_theme(&mut shell, &mut input, &config, true)
                .await
                .unwrap(),
            None
        );
        assert_eq!(shell.theme().background(), original.background());
        assert_eq!(shell.theme().capabilities(), original.capabilities());
        assert!(config.theme.is_none());
    }

    #[test]
    fn posix_shell_quote_round_trips_shell_sensitive_selector() {
        let selector = "resume id;$(printf pwned);'\"$HOME\" * 雪";
        let script = format!("set -- {}; printf '%s' \"$1\"", posix_shell_quote(selector));
        let output = std::process::Command::new("sh")
            .args(["-c", &script, "resume-test"])
            .env_clear()
            .env("HOME", "should-not-expand")
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("run POSIX shell");
        assert!(
            output.status.success(),
            "POSIX shell rejected quoted selector: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, selector.as_bytes());
    }

    #[test]
    fn answer_now_prompt_preserves_optional_instruction_and_enforces_tool_free_synthesis() {
        let bare = answer_now_prompt(None);
        assert!(bare.contains("evidence already gathered"));
        assert!(bare.contains("Do not call tools"));

        let instructed = answer_now_prompt(Some("Be concise".into()));
        assert!(instructed.starts_with("Be concise\n\n"));
        assert!(instructed.contains("Do not call tools"));

        let input = answer_now_input(Some("Be concise".into()));
        assert!(input.answer_only);
        assert_eq!(input.display_text, "/answer Be concise");
        assert!(matches!(
            input.parts.as_slice(),
            [octet_agent::InputPart::Text(text)] if text.contains("Do not call tools")
        ));
    }

    #[test]
    fn confirmation_notices_identify_core_tools_and_extensions() {
        assert_eq!(
            confirmation_notice(Some("write"), true),
            "write action approved"
        );
        assert_eq!(
            confirmation_notice(Some("bash"), false),
            "bash action denied"
        );
        assert_eq!(
            confirmation_notice(Some("custom_tool"), true),
            "extension action approved"
        );
        assert_eq!(confirmation_notice(None, false), "tool action denied");
    }

    #[test]
    fn fork_message_projection_uses_the_active_branch_and_adds_a_head_row() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text("first prompt".into())],
                },
            )))
            .unwrap();
        session
            .append(EntryValue::Message(octet_ai::Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text("answer".into())],
                    model: octet_ai::ModelId("test".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        session
            .append(EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text("second prompt".into())],
                },
            )))
            .unwrap();

        let messages = active_fork_messages(&session);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].text, "first prompt");
        assert_eq!(messages[1].text, "second prompt");
        assert!(messages[2].whole_conversation);
        assert_eq!(messages[2].entry_id, session.head().unwrap().0);
    }

    #[test]
    fn extension_lifecycle_fork_copies_the_active_head_and_records_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let sessions =
            crate::session_store::SessionStore::new(&directory.path().join("sessions"), &workspace);
        let source_path = sessions.new_path("2026-03-16");
        let mut prepared = None;
        let mut source = open_launch_session(
            &mut prepared,
            SessionSelection::CreateNew(source_path.clone()),
        )
        .unwrap();
        let head = source
            .append(EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text("current head".into())],
                },
            )))
            .unwrap();
        drop(source);

        let destination = sessions.new_path("2026-03-17");
        let fork_path =
            fork_active_session(&sessions, &source_path, destination, Some(&head)).unwrap();
        let fork = Session::open(&fork_path).unwrap();
        let source_id = session_id_for_path(&source_path).unwrap();
        let fork_id = session_id_for_path(&fork_path).unwrap();

        assert_eq!(fork.head(), Some(head.clone()));
        assert!(fork.entry(&head).is_some());
        let metadata = sessions.load_metadata(&fork_id).unwrap();
        assert_eq!(
            metadata.forked_from_session_id.as_deref(),
            Some(source_id.as_str())
        );
        assert_eq!(
            metadata.forked_from_entry_id.as_deref(),
            Some(head.0.as_str())
        );
    }

    #[test]
    fn extension_lifecycle_session_ids_fit_the_generated_result_bound() {
        assert_eq!(
            bounded_extension_session_id("x".repeat(MAX_JSON_RPC_ID_BYTES)).unwrap(),
            "x".repeat(MAX_JSON_RPC_ID_BYTES)
        );
        assert!(bounded_extension_session_id("x".repeat(MAX_JSON_RPC_ID_BYTES + 1)).is_err());
    }

    #[test]
    fn web_search_menu_recommends_brave_and_keeps_searxng_and_disable() {
        let (items, descriptions) = web_search_menu_entries(true);
        assert_eq!(
            items,
            [
                "Brave Search (recommended)",
                "SearXNG",
                "Disable octet-web-search"
            ]
        );
        assert!(descriptions[0]
            .as_deref()
            .is_some_and(|description| description.contains("API key")));

        let (items, _) = web_search_menu_entries(false);
        assert_eq!(items, ["Brave Search (recommended)", "SearXNG"]);
    }

    #[test]
    fn delegated_session_document_is_bounded_path_free_and_styled() {
        let directory = tempfile::tempdir().unwrap();
        let private_path = directory.path().join("private-child.jsonl");
        let mut session = Session::create(&private_path).unwrap();
        session
            .append(EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text(
                        "Inspect the worker result.".into(),
                    )],
                },
            )))
            .unwrap();
        let theme = test_theme();
        let text = delegated_session_text(&session, &theme, 80, false).unwrap();
        let overlay = delegated_session_overlay_text(&text, &theme);
        let plain = crate::tui::view::sanitize_for_terminal(&overlay);
        assert!(plain.contains("Delegated worker transcript"));
        assert!(plain.contains("read-only · mutation remains owner-bound"));
        assert!(plain.contains("Inspect the worker result."));
        assert!(
            overlay.contains('\x1b'),
            "the trusted theme styling was lost"
        );
        assert!(!overlay.contains(private_path.to_str().unwrap()));
        assert!(text.len() <= 128 * 1024);
    }

    #[test]
    fn delegated_session_overlay_keeps_newest_output_within_the_exact_byte_cap() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("child.jsonl")).unwrap();
        for index in 0..20 {
            let marker = if index == 19 {
                "NEWEST-FINAL-WORKER-RESULT"
            } else {
                "older-worker-output"
            };
            session
                .append(EntryValue::Message(octet_ai::Message::Assistant(
                    octet_ai::AssistantMessage {
                        content: vec![octet_ai::AssistantPart::Text(format!(
                            "block-{index:02}-{marker}-{}",
                            "x".repeat(16 * 1024)
                        ))],
                        model: octet_ai::ModelId("worker-test".into()),
                        protocol: octet_ai::Protocol::OpenAiChat,
                    },
                )))
                .unwrap();
        }

        let text = delegated_session_text(&session, &test_theme(), 80, false).unwrap();
        assert!(text.contains("NEWEST-FINAL-WORKER-RESULT"), "{text}");
        assert!(!text.contains("block-00-older-worker-output"));
        assert!(text.contains("[older transcript entries omitted]"));
        assert!(text.len() <= 128 * 1024, "{}", text.len());
    }

    #[test]
    fn delegated_session_renders_markdown_like_the_main_transcript() {
        // The worker transcript must flow through the exact same rich
        // markdown renderer as the main conversation: headings, bold, and
        // inline code keep their theme styling instead of being flattened to
        // raw text.
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("child.jsonl")).unwrap();
        let markdown = "# Heading One\n\nplain **bold** and `code` tail";
        session
            .append(EntryValue::Message(octet_ai::Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text(markdown.into())],
                    model: octet_ai::ModelId("worker-test".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        let text = delegated_session_text(&session, &test_theme(), 80, false).unwrap();
        let plain = crate::tui::view::sanitize_for_terminal(&text);

        assert!(text.contains('\x1b'), "styled block not found in {text}");
        assert!(plain.contains("Heading One"), "{plain}");
        assert!(plain.contains("plain bold and code tail"), "{plain}");
        assert!(!plain.contains("# Heading One"), "{plain}");
        assert!(!plain.contains("**bold**"), "{plain}");
        assert!(!plain.contains("`code`"), "{plain}");
    }

    #[test]
    fn delegated_session_hydrates_main_transcript_tool_cards_and_results() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("child.jsonl")).unwrap();
        let call_id = octet_ai::ToolCallId("worker-write".into());
        session
            .append(EntryValue::Message(octet_ai::Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                        async_execution: false,
                        id: call_id.clone(),
                        name: "write".into(),
                        arguments_json: serde_json::json!({
                            "path": "worker.rs",
                            "content": "pub fn worker() {}\n",
                        })
                        .to_string(),
                        argument_error: None,
                    })],
                    model: octet_ai::ModelId("worker-test".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        session
            .append(EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::ToolResult(octet_ai::ToolResult {
                        tool_call_id: call_id,
                        content: vec![octet_ai::ToolResultPart::Text(
                            "ok\nworker.rs  created\n--- /dev/null\n+++ b/worker.rs\n@@ -0,0 +1 @@\n+SUBAGENT-TOOL-RESULT"
                                .into(),
                        )],
                        is_error: false,
                        added_tool_names: None,
                    })],
                },
            )))
            .unwrap();

        let text = delegated_session_text(&session, &test_theme(), 80, false).unwrap();
        let plain = crate::tui::view::sanitize_for_terminal(&text);
        assert!(plain.contains("Write"), "{plain}");
        assert!(plain.contains("SUBAGENT-TOOL-RESULT"), "{plain}");
    }

    #[test]
    fn subagent_presentation_becomes_navigable_rows_with_opaque_session_references() {
        let mut snapshot: octet_agent::ExtensionPresentationSnapshot =
            serde_json::from_str(include_str!("../../fixtures/extension-presentation.json"))
                .unwrap();
        let collection = snapshot.collection.as_mut().unwrap();
        collection.nodes[0].references = collection.detail.as_ref().unwrap().references.clone();
        let (title, entries) =
            subagent_view_entries_from_presentation(crate::extensions::ExtensionPresentationView {
                extension: "octet-subagents".into(),
                generation: 1,
                extension_instance_id: "instance".into(),
                resource_owner: Some("owner".into()),
                snapshot,
            })
            .unwrap();

        // The picker header is the collection's stable surface name only: the
        // per-worker states are on the rows and the key affordances are in the
        // panel action footer, so no counts or hints may be composed into it.
        assert_eq!(title, "Subagents");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "test-review");
        assert!(entries[0].description.contains("running"));
        assert_eq!(
            entries[0].session_reference.as_deref(),
            Some("session-worker-1")
        );
        assert!(entries[0].fallback_detail.contains("bounded child session"));
        assert!(!entries[0].fallback_detail.contains(".jsonl"));
    }

    #[tokio::test]
    async fn cancellable_wait_returns_none_on_ctrl_c() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        drop(sender);
        let mut input = ReceiverStream::new(receiver);

        let mut shell = InteractiveShell::test_shell();
        let result = await_with_ctrl_c(std::future::pending::<()>(), &mut shell, &mut input).await;
        assert!(result.is_none());
        assert!(!shell.close_requested());
    }

    #[tokio::test]
    async fn cancellable_wait_finishes_after_input_stream_closes() {
        let mut input = tokio_stream::empty::<std::io::Result<Event>>();
        let mut shell = InteractiveShell::test_shell();
        assert_eq!(
            await_with_ctrl_c(async { 42 }, &mut shell, &mut input).await,
            Some(42)
        );
    }

    #[tokio::test]
    async fn cancellable_wait_propagates_ctrl_d_as_a_close_request() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        drop(sender);
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();

        let result = await_with_ctrl_c(std::future::pending::<()>(), &mut shell, &mut input).await;

        assert!(result.is_none());
        assert!(shell.close_requested());
    }

    #[tokio::test]
    async fn cancellable_wait_preserves_input_and_disclosure_before_escape() {
        use crossterm::event::KeyEvent;

        let mut input = tokio_stream::iter([
            Ok(Event::Resize(46, 8)),
            Ok(Event::Paste("draft during wait".into())),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL,
            ))),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Ok(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
        ]);
        let mut shell = InteractiveShell::test_shell();
        let was_verbose = shell.verbose_tools();
        // This budget bounds input handling, not the provider's timeout.
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            await_with_ctrl_c(std::future::pending::<()>(), &mut shell, &mut input),
        )
        .await
        .expect("Escape must interrupt a held-open operation within 250 ms");
        assert!(result.is_none());
        assert_eq!(shell.pending(), "draft during wait");
        assert_ne!(shell.verbose_tools(), was_verbose);
        assert!(!shell.close_requested());
    }

    #[tokio::test]
    async fn silent_startup_lifecycle_keeps_typed_input_and_names_the_phase_off_screen() {
        use crossterm::event::KeyEvent;

        // Typing before readiness is buffered into the draft, exactly as the
        // labeled wait does; nothing renders a phase label.
        let events = [
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            ))),
            Ok(Event::Paste(" draft during startup".into())),
        ];
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let mut done_tx = Some(done_tx);
        let source = tokio_stream::iter(events).chain(futures_util::stream::poll_fn(move |_| {
            if let Some(done_tx) = done_tx.take() {
                let _ = done_tx.send(());
            }
            std::task::Poll::Ready(None)
        }));
        let mut input = EventStream::from_stream(source);
        let mut shell = InteractiveShell::test_shell();
        let result = run_blocking_startup_lifecycle(
            &mut shell,
            &mut input,
            STARTUP_APP_OPERATION,
            move || {
                done_rx.recv()?;
                Ok(7)
            },
        )
        .await
        .expect("the silent startup phase completes");
        assert_eq!(result, 7);
        assert_eq!(shell.pending(), "x draft during startup");

        // A failed silent phase stays attributable without rendering: the
        // operation name is carried by the diagnostic alone.
        let mut input =
            EventStream::from_stream(tokio_stream::iter(Vec::<std::io::Result<Event>>::new()));
        let error = run_blocking_startup_lifecycle(
            &mut shell,
            &mut input,
            STARTUP_MODELS_OPERATION,
            || -> anyhow::Result<()> { anyhow::bail!("catalog unavailable") },
        )
        .await
        .expect_err("the failed startup phase surfaces");
        assert_eq!(
            error.to_string(),
            "model discovery failed: catalog unavailable"
        );
    }

    #[tokio::test]
    async fn osc11_startup_lifecycle_keeps_handed_off_typing_paste_and_shortcuts() {
        use crossterm::event::KeyEvent;

        let events = [
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            ))),
            Ok(Event::Paste(" draft during startup".into())),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL,
            ))),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
        ];
        let (finished, settled) = tokio::sync::oneshot::channel();
        let mut finished = Some(finished);
        // Settle only after the input owner has processed every queued event.
        let source = tokio_stream::iter(events).chain(futures_util::stream::poll_fn(move |_| {
            if let Some(finished) = finished.take() {
                let _ = finished.send(());
            }
            std::task::Poll::Ready(None)
        }));
        let mut input = EventStream::from_stream(source);
        let mut shell = InteractiveShell::test_shell();
        let verbose = shell.verbose_tools();
        let result = await_lifecycle(&mut shell, &mut input, "starting…", async move {
            settled.await?;
            Ok(42)
        })
        .await
        .unwrap();
        assert_eq!(result, 42);
        assert_eq!(shell.pending(), "x draft during startup");
        assert_ne!(shell.verbose_tools(), verbose);
        assert!(!shell.close_requested());
    }

    #[test]
    fn startup_picker_close_is_a_graceful_exit_but_other_errors_survive() {
        let mut shell = InteractiveShell::test_shell();
        shell.request_close();
        assert_eq!(
            startup_launch_outcome::<u8>(&shell, Err(anyhow::anyhow!("selection cancelled")))
                .unwrap(),
            None
        );

        let shell = InteractiveShell::test_shell();
        let error =
            startup_launch_outcome::<u8>(&shell, Err(anyhow::anyhow!("selection cancelled")))
                .unwrap_err();
        assert_eq!(error.to_string(), "selection cancelled");
    }

    #[test]
    fn bounded_shell_output_keeps_head_and_tail_within_budget() {
        let mut output = BoundedShellOutput::new(10);
        output.push(b"0123");
        output.push(b"456789");
        output.push(b"abcdef");

        assert_eq!(output.head, b"01234");
        assert_eq!(output.tail, b"bcdef");
        assert_eq!(output.total_bytes, 16);
        let rendered = output.render("stdout");
        assert!(rendered.starts_with("01234\n"), "{rendered:?}");
        assert!(rendered.contains("stdout truncated; 6 bytes omitted"));
        assert!(rendered.ends_with("\nbcdef"), "{rendered:?}");
    }

    #[test]
    fn bounded_shell_output_does_not_claim_untruncated_tail_was_omitted() {
        let mut output = BoundedShellOutput::new(10);
        output.push("012345é".as_bytes());

        assert_eq!(output.total_bytes, 8);
        assert_eq!(output.render("stdout"), "012345é");
    }

    #[tokio::test]
    async fn shell_pipes_are_drained_concurrently_with_process_exit() {
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("yes o | head -c 1048576 & yes e | head -c 1048576 >&2 & wait")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let stdout = std::sync::Arc::new(std::sync::Mutex::new(BoundedShellOutput::new(1024)));
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(BoundedShellOutput::new(1024)));
        let (updates, mut update_rx) = tokio::sync::mpsc::unbounded_channel();

        let status = tokio::time::timeout(Duration::from_secs(5), async {
            let (_, _, status) = tokio::join!(
                drain_shell_pipe(&mut stdout_pipe, &stdout, &updates),
                drain_shell_pipe(&mut stderr_pipe, &stderr, &updates),
                child.wait(),
            );
            status
        })
        .await
        .expect("full stdout and stderr pipes must not deadlock")
        .unwrap();

        assert!(status.success());
        let stdout = stdout.lock().unwrap();
        let stderr = stderr.lock().unwrap();
        assert_eq!(stdout.total_bytes, 1_048_576);
        assert_eq!(stderr.total_bytes, 1_048_576);
        assert_eq!(stdout.head.len() + stdout.tail.len(), 1024);
        assert_eq!(stderr.head.len() + stderr.tail.len(), 1024);
        assert!(
            update_rx.try_recv().is_ok(),
            "pipe reads must wake live rendering"
        );
    }

    #[test]
    fn a_failed_checkout_can_restore_the_previous_durable_head() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollback.jsonl");
        let mut session = Session::create(&path).unwrap();
        let previous = session
            .append(EntryValue::Config {
                model: Some("model".to_string()),
                reasoning: Some("off".to_string()),
                reasoning_mode: None,
            })
            .unwrap();
        let target = session
            .append(EntryValue::Config {
                model: Some("missing-model".to_string()),
                reasoning: None,
                reasoning_mode: None,
            })
            .unwrap();
        session.checkout(target).unwrap();
        drop(session);

        restore_session_head(&path, previous.clone()).unwrap();
        assert_eq!(Session::open(path).unwrap().head(), Some(previous));
    }

    #[test]
    fn adjacent_reconfigurations_coalesce_but_boundaries_survive() {
        let mut queue = VecDeque::new();
        push_pending_action(
            &mut queue,
            PendingIdleAction::ChangeModel(ModelId("a".into())),
        );
        push_pending_action(
            &mut queue,
            PendingIdleAction::ChangeModel(ModelId("b".into())),
        );
        push_pending_action(&mut queue, PendingIdleAction::NewSession);
        push_pending_action(
            &mut queue,
            PendingIdleAction::ChangeModel(ModelId("c".into())),
        );
        assert_eq!(
            queue,
            VecDeque::from([
                PendingIdleAction::ChangeModel(ModelId("b".into())),
                PendingIdleAction::NewSession,
                PendingIdleAction::ChangeModel(ModelId("c".into())),
            ])
        );
    }

    #[test]
    fn command_queue_parses_reconfiguration_values() {
        let mut queue = VecDeque::new();
        queue_command(Command::Login(None), &mut queue).unwrap();
        queue_command(Command::Thinking(Some("high".into())), &mut queue).unwrap();
        queue_command(Command::Resume(Some("id".into())), &mut queue).unwrap();
        assert_eq!(queue.pop_front(), Some(PendingIdleAction::Login(None)));
        assert!(matches!(
            queue.pop_front(),
            Some(PendingIdleAction::ChangeThinkingLevel(ThinkingLevel::High))
        ));
        assert_eq!(
            queue.pop_front(),
            Some(PendingIdleAction::ResumeSession(Some("id".into())))
        );
    }

    #[tokio::test]
    async fn startup_update_is_skipped_offline_and_cancelled_with_its_owner() {
        let offline = startup_update_task(
            true,
            async { panic!("offline startup must not poll the release request") },
            |_| panic!("offline startup must not publish a notice"),
        );
        assert!(offline.is_empty());

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
        let check = startup_update_task(
            false,
            async move {
                let _ = started_tx.send(());
                let _ = held_rx.await;
                Some(semver::Version::new(9, 8, 7))
            },
            |_| panic!("exited startup must not publish a notice"),
        );
        started_rx.await.unwrap();
        assert_eq!(check.len(), 1);
        drop(check);
        let mut held_tx = held_tx;
        tokio::time::timeout(Duration::from_secs(1), held_tx.closed())
            .await
            .expect("owner exit must cancel the held check");
    }

    #[tokio::test]
    async fn startup_update_publishes_once_and_failure_is_quiet() {
        for latest in [None, Some(semver::Version::new(9, 8, 7))] {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = calls.clone();
            let expected = usize::from(latest.is_some());
            let mut check = startup_update_task(false, async move { latest }, move |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            assert_eq!(check.len(), 1);
            check.join_next().await.unwrap().unwrap();
            assert!(check.is_empty());
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), expected);
        }
    }

    #[tokio::test]
    async fn idle_slash_enter_returns_the_highlighted_command_to_dispatch() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let mut shell = InteractiveShell::test_shell();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        for event in [
            Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ] {
            sender.send(Ok(event)).await.unwrap();
        }
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
        let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let (reload_watcher, mut reload, mut reload_tick) = test_reload();

        let idle = wait_for_prompt(
            &mut shell,
            &mut input,
            &mut scroll_tick,
            &mut extension_tick,
            &mut extensions,
            None,
            &mut reload_tick,
            &reload_watcher,
            &mut reload,
        )
        .await
        .unwrap();
        let Idle::Command(command) = idle else {
            panic!("highlighted slash command was not handed to the idle dispatcher");
        };
        assert_eq!(command.trim(), "/resume");
        assert!(shell.pending_is_empty());
        assert!(!shell.slash_popup_open());
    }

    /// Drive the idle input owner with a fixed event list and return the shell.
    async fn idle_shell_after(events: Vec<Event>) -> (InteractiveShell, Idle) {
        use tokio_stream::wrappers::ReceiverStream;

        let mut shell = InteractiveShell::test_shell();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        for event in events {
            sender.send(Ok(event)).await.unwrap();
        }
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
        let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let (reload_watcher, mut reload, mut reload_tick) = test_reload();
        let idle = wait_for_prompt(
            &mut shell,
            &mut input,
            &mut scroll_tick,
            &mut extension_tick,
            &mut extensions,
            None,
            &mut reload_tick,
            &reload_watcher,
            &mut reload,
        )
        .await
        .unwrap();
        (shell, idle)
    }

    /// A disabled live-reload supervisor for idle-loop tests: it samples
    /// nothing and can never report a due pass, so the existing idle-loop
    /// assertions keep their meaning unchanged.
    fn test_reload() -> (
        crate::reload::ReloadWatcher,
        crate::reload::ReloadSupervisor,
        tokio::time::Interval,
    ) {
        let reload =
            crate::reload::ReloadSupervisor::new(crate::reload::ReloadSettings::disabled());
        let watcher = crate::reload::ReloadWatcher::new(crate::reload::ReloadWatchSet::new());
        let mut tick = tokio::time::interval(reload.settings().tick_interval());
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        (watcher, reload, tick)
    }

    fn ctrl_key(character: char) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        ))
    }

    #[tokio::test]
    async fn idle_clipboard_gesture_inserts_native_text_without_submitting() {
        clipboard_read::set_test_text(Some("pasted from the clipboard".to_owned()));
        // Ctrl-D settles the idle wait without submitting or discarding the
        // draft the paste created.
        let (shell, idle) = idle_shell_after(vec![ctrl_key('v'), ctrl_key('d')]).await;
        clipboard_read::clear_test_text();

        assert!(matches!(idle, Idle::Quit));
        assert_eq!(shell.pending(), "pasted from the clipboard");
    }

    #[tokio::test]
    async fn idle_clipboard_gesture_without_text_keeps_the_existing_fallback() {
        clipboard_read::set_test_text(None);
        let (shell, idle) = idle_shell_after(vec![
            ctrl_key('v'),
            // The terminal-originated bracketed paste remains the fallback when
            // no native transport produced text.
            Event::Paste("terminal bracketed paste".to_owned()),
            ctrl_key('d'),
        ])
        .await;
        clipboard_read::clear_test_text();

        assert!(matches!(idle, Idle::Quit));
        assert_eq!(shell.pending(), "terminal bracketed paste");
    }

    #[tokio::test]
    async fn active_clipboard_completion_preserves_native_and_terminal_paste() {
        for native in [Some("native draft".to_owned()), None] {
            let (_server, _workspace, mut agent) =
                scripted_agent_with_delay(Duration::from_secs(2)).await;
            let mut shell = InteractiveShell::test_shell();
            clipboard_read::set_test_text(native.clone());
            let gesture = Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                if cfg!(windows) {
                    KeyModifiers::ALT
                } else {
                    KeyModifiers::CONTROL
                },
            ));
            let mut events = vec![Ok(gesture)];
            if native.is_none() {
                events.push(Ok(Event::Paste("terminal draft".into())));
            }
            events.push(Ok(ctrl_key('d')));
            let mut input = tokio_stream::iter(events).chain(futures_util::stream::pending());
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("initial").await.unwrap();
            shell.set_awaiting_provider(run_id);
            let control = run.control();
            let mut ticker = tokio::time::interval(Duration::from_millis(16));
            let mut pending = VecDeque::new();
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let mut deadline = None;
            let ended = tokio::time::timeout(
                Duration::from_secs(1),
                drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut ticker,
                    &mut pending,
                    &mut quit,
                    None,
                    None,
                    &mut extensions,
                    &mut false,
                    test_run_inspection(),
                    &mut deadline,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            clipboard_read::clear_test_text();
            assert_eq!(ended, HostRunOutcome::Aborted);
            assert!(quit);
            assert_eq!(
                shell.pending(),
                native.as_deref().unwrap_or("terminal draft")
            );
            assert!(pending.is_empty());
        }
    }

    #[tokio::test]
    async fn active_clipboard_slow_helper_cancels_on_ctrl_c_and_settlement() {
        for cancel in [true, false] {
            let (_server, _workspace, mut agent) = scripted_agent_with_delay(if cancel {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(10)
            })
            .await;
            let mut shell = InteractiveShell::test_shell();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel::<()>();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            // A deterministic wedged helper: input cannot send Ctrl-C until the
            // read is actually polled, and the helper cannot finish on its own.
            clipboard_read::set_test_helper(async move {
                let _guard = dropped_tx;
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Some("must never reach the draft".into())
            });
            let gesture = Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                if cfg!(windows) {
                    KeyModifiers::ALT
                } else {
                    KeyModifiers::CONTROL
                },
            ));
            let mut input = tokio_stream::iter([Ok(gesture)])
                .chain(
                    futures_util::stream::once(async move {
                        started_rx.await.unwrap();
                        if cancel {
                            Ok(ctrl_key('c'))
                        } else {
                            futures_util::future::pending().await
                        }
                    })
                    .boxed(),
                )
                .chain(futures_util::stream::pending());
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("initial").await.unwrap();
            shell.set_awaiting_provider(run_id);
            let control = run.control();
            let mut ticker = tokio::time::interval(Duration::from_millis(16));
            let mut pending = VecDeque::new();
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let mut deadline = None;
            let ended = tokio::time::timeout(
                Duration::from_secs(1),
                drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut ticker,
                    &mut pending,
                    &mut quit,
                    None,
                    None,
                    &mut extensions,
                    &mut false,
                    test_run_inspection(),
                    &mut deadline,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                ended,
                if cancel {
                    HostRunOutcome::Aborted
                } else {
                    HostRunOutcome::Completed
                }
            );
            assert!(shell.pending().is_empty());
            assert!(!quit);
            assert!(matches!(
                dropped_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ));
            shell.extension_set_editor("replacement composer".into());
            assert!(
                release_tx.send(()).is_err(),
                "settled helper must be dropped"
            );
            tokio::task::yield_now().await;
            assert_eq!(shell.pending(), "replacement composer");
        }
    }

    #[tokio::test]
    async fn active_clipboard_editor_ownership_fences_text_and_fallback() {
        for text in [Some("stale native text".to_owned()), None] {
            for owner in ["extension", "search", "panel"] {
                let text = text.clone();
                let mut shell = InteractiveShell::test_shell();
                shell.begin_run("test");
                shell.extension_set_editor("original draft".into());
                let revision = shell.extension_editor_snapshot().revision;
                let (started_tx, started_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
                let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel::<()>();
                clipboard_read::set_test_helper(async move {
                    let _guard = dropped_tx;
                    let _ = started_tx.send(());
                    let _ = release_rx.await;
                    text
                });
                let mut read = Box::pin(clipboard_read::read_text());
                assert!(futures_util::poll!(&mut read).is_pending());
                started_rx.await.unwrap();
                // These changes bypass InputAction ownership-transfer checks.
                // Search and panels leave the normal editor revision unchanged.
                match owner {
                    "extension" => {
                        shell.extension_set_editor("replacement composer".into());
                        assert_ne!(shell.extension_editor_snapshot().revision, revision);
                    }
                    "search" => {
                        assert!(shell.intercept_transcript_input(&transcript_search_open_key()));
                        assert!(shell.transcript_search_active());
                    }
                    "panel" => shell.open_panel(Panel::ReadOnlyDocument {
                        title: "Inspection".into(),
                        text: "Read-only document".into(),
                        styled: false,
                        scroll_from_bottom: 0,
                    }),
                    _ => unreachable!(),
                }
                if owner != "extension" {
                    let editor = shell.extension_editor_snapshot();
                    assert_eq!(editor.revision, revision);
                    assert!(!editor.focused);
                }
                let before = shell.debug_snapshot();
                release_tx.send(()).unwrap();
                let gesture = Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('v'),
                    if cfg!(windows) {
                        KeyModifiers::ALT
                    } else {
                        KeyModifiers::CONTROL
                    },
                ));
                let fallback =
                    settle_active_clipboard_read(&mut shell, revision, read.await, Some(gesture));
                assert!(
                    fallback.is_none(),
                    "stale gestures must not be replayed either"
                );
                assert_eq!(
                    shell.pending(),
                    if owner == "extension" {
                        "replacement composer"
                    } else {
                        "original draft"
                    }
                );
                assert_eq!(
                    shell.debug_snapshot(),
                    before,
                    "{owner} must not receive stale paste"
                );
                assert!(matches!(
                    dropped_rx.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed)
                ));
            }
        }
    }

    #[tokio::test]
    async fn active_clipboard_draft_handoff_drops_pending_read_before_replacement() {
        for boundary in ["queue", "steer", "recall", "command"] {
            let (_server, _workspace, mut agent) =
                scripted_agent_with_delay(Duration::from_secs(2)).await;
            let mut shell = InteractiveShell::test_shell();
            shell.extension_set_editor(if boundary == "command" {
                "/answer answer now".into()
            } else {
                "original draft".into()
            });
            let boundary_key = match boundary {
                "steer" => ctrl_key('s'),
                "recall" => {
                    let queued = shell.drain_composed();
                    shell.queue_follow_up(queued);
                    Event::Key(crossterm::event::KeyEvent::new(
                        KeyCode::Up,
                        KeyModifiers::ALT,
                    ))
                }
                _ => Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )),
            };
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel::<()>();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            clipboard_read::set_test_helper(async move {
                let _guard = dropped_tx;
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Some("late clipboard payload".into())
            });
            let gesture = Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                if cfg!(windows) {
                    KeyModifiers::ALT
                } else {
                    KeyModifiers::CONTROL
                },
            ));
            let mut input = tokio_stream::iter([Ok(gesture)])
                .chain(
                    futures_util::stream::once(async move {
                        started_rx.await.unwrap();
                        Ok(boundary_key)
                    })
                    .boxed(),
                )
                .chain(
                    futures_util::stream::once(async move {
                        // Checked before close or run settlement can clean up the
                        // helper: ownership transfer itself must cancel the read.
                        assert!(
                            matches!(
                                dropped_rx.try_recv(),
                                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
                            ),
                            "{boundary}"
                        );
                        assert!(release_tx.send(()).is_err(), "{boundary}");
                        Ok(Event::Paste(" replacement composer".into()))
                    })
                    .boxed(),
                )
                .chain(tokio_stream::iter([Ok(ctrl_key('d'))]))
                .chain(futures_util::stream::pending());
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("initial").await.unwrap();
            shell.set_awaiting_provider(run_id);
            let control = run.control();
            let mut ticker = tokio::time::interval(Duration::from_millis(16));
            let mut pending = VecDeque::new();
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let mut deadline = None;
            let ended = tokio::time::timeout(
                Duration::from_secs(1),
                drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut ticker,
                    &mut pending,
                    &mut quit,
                    None,
                    None,
                    &mut extensions,
                    &mut false,
                    test_run_inspection(),
                    &mut deadline,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(ended, HostRunOutcome::Aborted, "{boundary}");
            assert!(quit);
            assert!(
                shell.pending().contains("replacement composer"),
                "{boundary}"
            );
            assert!(
                !shell.pending().contains("late clipboard payload"),
                "{boundary}"
            );
        }
    }

    #[tokio::test]
    async fn clipboard_gesture_is_consumed_on_the_active_run_path_too() {
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");
        let gesture = ctrl_key('v');

        clipboard_read::set_test_text(Some("steer text".to_owned()));
        assert!(paste_clipboard_text(&mut shell, &gesture).await);
        assert_eq!(shell.pending(), "steer text");

        // Only the declared gesture is consumed.
        let typed = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ));
        assert!(!paste_clipboard_text(&mut shell, &typed).await);

        // A failed read reports that nothing was pasted and leaves the draft.
        clipboard_read::set_test_text(None);
        assert!(!paste_clipboard_text(&mut shell, &gesture).await);
        assert_eq!(shell.pending(), "steer text");
        clipboard_read::clear_test_text();
    }

    fn transcript_search_open_key() -> Event {
        let bindings = keymap::keybindings::KeybindingsManager::current_platform();
        let mut key = crossterm::event::KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        // Windows/WSL use Ctrl-F; other platforms use Ctrl-Shift-F.
        if !bindings.matches(&key, "tui.altScreen.search") {
            key.modifiers |= KeyModifiers::SHIFT;
        }
        assert!(bindings.matches(&key, "tui.altScreen.search"));
        Event::Key(key)
    }

    #[tokio::test]
    async fn transcript_search_idle_owner_intercepts_paste_before_composer_admission() {
        clipboard_read::set_test_text(Some("native clipboard must stay out of the draft".into()));
        let (mut shell, idle) = idle_shell_after(vec![
            Event::Paste("preserved draft".into()),
            transcript_search_open_key(),
            Event::Paste("search query /tmp/image.png".into()),
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                if cfg!(windows) {
                    KeyModifiers::ALT
                } else {
                    KeyModifiers::CONTROL
                },
            )),
            ctrl_key('d'),
        ])
        .await;
        clipboard_read::clear_test_text();
        assert!(matches!(idle, Idle::Quit));
        assert!(shell.transcript_search_active());
        assert_eq!(shell.pending(), "preserved draft");
        assert!(shell.drain_composed().attachments.is_empty());
    }

    #[tokio::test]
    async fn transcript_search_active_owner_consumes_query_and_escape_without_interrupting_run() {
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(100)).await;
        let mut shell = InteractiveShell::test_shell();
        shell.extension_set_editor("preserved active draft".into());
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        for event in [
            transcript_search_open_key(),
            Event::Paste("search query /tmp/image.png".into()),
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                if cfg!(windows) {
                    KeyModifiers::ALT
                } else {
                    KeyModifiers::CONTROL
                },
            )),
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )),
        ] {
            sender.send(Ok(event)).await.unwrap();
        }
        let _sender = sender;
        let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut deadline = None;
        clipboard_read::set_test_text(Some("native clipboard must stay out of the draft".into()));
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut extensions,
                &mut false,
                test_run_inspection(),
                &mut deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        clipboard_read::clear_test_text();
        drop(run);
        assert_eq!(ended, HostRunOutcome::Completed);
        assert!(!quit);
        assert!(pending.is_empty());
        assert!(!shell.transcript_search_active());
        assert_eq!(shell.pending(), "preserved active draft");
        assert!(shell.drain_composed().attachments.is_empty());
        assert!(!format!("{:?}", agent.session().context().unwrap()).contains("search query"));
    }

    #[tokio::test]
    async fn configuration_commit_observer_skips_noops_and_unknown_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap().join("config.toml");
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let missing = configuration_snapshot(&path);
        assert_eq!(missing, Some(None));
        assert!(!observe_configuration_commit(&mut extensions, missing.clone(), Some(&path)).await);
        std::fs::write(&path, b"theme = \"dark\"\n").unwrap();
        assert!(observe_configuration_commit(&mut extensions, missing, Some(&path)).await);
        let unchanged = configuration_snapshot(&path);
        assert!(!observe_configuration_commit(&mut extensions, unchanged, Some(&path)).await);
        assert!(!observe_configuration_commit(&mut extensions, None, Some(&path)).await);
        assert!(!observe_configuration_commit(&mut extensions, Some(None), None).await);
        std::fs::write(&path, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert!(configuration_snapshot(&path).is_none());
        #[cfg(unix)]
        {
            let link = path.with_file_name("link.toml");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(configuration_snapshot(&link).is_none());
        }
        let failed: anyhow::Result<()> = persist_configuration(Some(&mut extensions), || {
            anyhow::bail!("failed before commit")
        })
        .await;
        assert!(failed.is_err());
    }

    #[tokio::test]
    async fn scoped_model_cycle_reaches_idle_owner_without_draining_draft() {
        for draft in ["unfinished prompt", "/model second"] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_identity("test", "first", "off");
            shell.set_model_cycle(vec!["first".into(), "second".into()]);
            shell.extension_set_editor(draft.into());
            let mut input = tokio_stream::iter([Ok(ctrl_key('p'))]);
            let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
            let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let (reload_watcher, mut reload, mut reload_tick) = test_reload();
            let idle = wait_for_prompt(
                &mut shell,
                &mut input,
                &mut scroll_tick,
                &mut extension_tick,
                &mut extensions,
                None,
                &mut reload_tick,
                &reload_watcher,
                &mut reload,
            )
            .await
            .unwrap();
            assert!(matches!(idle, Idle::Command(text) if text == "/model second"));
            assert_eq!(shell.pending(), draft);
        }
    }

    #[tokio::test]
    async fn scoped_model_cycle_queues_on_active_owner_without_draining_draft() {
        for draft in ["unfinished active prompt", "/model second"] {
            let (_server, _workspace, mut agent) =
                scripted_agent_with_delay(Duration::from_millis(100)).await;
            let mut shell = InteractiveShell::test_shell();
            shell.set_identity("test", "scripted", "off");
            shell.set_model_cycle(vec!["scripted".into(), "second".into(), "third".into()]);
            shell.extension_set_editor(draft.into());
            let (sender, receiver) = tokio::sync::mpsc::channel(4);
            sender.send(Ok(ctrl_key('p'))).await.unwrap();
            sender.send(Ok(ctrl_key('p'))).await.unwrap();
            let _sender = sender;
            let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
            let mut ticker = tokio::time::interval(Duration::from_millis(1));
            let mut pending = VecDeque::new();
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("initial").await.unwrap();
            shell.set_awaiting_provider(run_id);
            let control = run.control();
            let mut deadline = None;
            let ended = tokio::time::timeout(
                Duration::from_secs(5),
                drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut ticker,
                    &mut pending,
                    &mut quit,
                    None,
                    None,
                    &mut extensions,
                    &mut false,
                    test_run_inspection(),
                    &mut deadline,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            drop(run);
            assert_eq!(ended, HostRunOutcome::Completed);
            assert!(!quit);
            assert_eq!(shell.pending(), draft);
            assert_eq!(
                pending,
                VecDeque::from([PendingIdleAction::ChangeModel(ModelId("third".into()))])
            );
            assert!(!format!("{:?}", agent.session().context().unwrap()).contains("/model"));
        }
    }

    #[tokio::test]
    async fn compact_custom_instructions_queue_and_reach_only_the_summary_wire() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        for queued in [false, true] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/messages"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(text_turn()),
                )
                .mount(&server)
                .await;
            let (_directory, mut app) = fast_test_app(scripted_model(&server.uri()));
            seed_compaction_session(&mut app.agent);
            app.config.compaction.keep_recent_tokens = 1;
            let mut shell = InteractiveShell::test_shell();
            let requested = commands::parse("/compact preserve the API contract");
            let instructions = if queued {
                let (mut queue, quit) =
                    run_active_command(&mut shell, requested, test_run_inspection()).await;
                assert!(!quit);
                assert!(server.received_requests().await.unwrap().is_empty());
                let Some(PendingIdleAction::CompactWithInstructions(value)) = queue.pop_front()
                else {
                    panic!("missing queued instructions")
                };
                assert!(queue.is_empty());
                value
            } else {
                let Command::CompactWithInstructions(value) = requested else {
                    panic!("missing instructions")
                };
                value
            };
            let mut input = futures_util::stream::pending();
            compact_interactively(
                &mut app,
                &mut shell,
                &mut input,
                !queued,
                Some(&instructions),
            )
            .await;
            assert_eq!(shell.debug_error(), None);
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1);
            let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert!(body["system"].to_string().contains(&instructions));
            assert!(!body["messages"].to_string().contains(&instructions));
            assert_eq!(
                app.agent.session().usage_records().len(),
                1,
                "summary accounted exactly once"
            );
            assert!(
                !format!("{:?}", app.agent.session().context().unwrap()).contains(&instructions)
            );
        }
    }

    #[tokio::test]
    async fn compact_custom_instructions_fail_closed_for_native_and_oversize() {
        let server = wiremock::MockServer::start().await;
        let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri()));
        app.config.compaction.mode = CompactionMode::NativeResponses;
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending();
        compact_interactively(
            &mut app,
            &mut shell,
            &mut input,
            true,
            Some("preserve evidence"),
        )
        .await;
        assert!(shell
            .debug_snapshot()
            .contains("custom instructions require local compaction mode"));
        let oversized = "x".repeat(16 * 1024 + 1);
        compact_interactively(&mut app, &mut shell, &mut input, true, Some(&oversized)).await;
        assert!(shell.debug_error().unwrap().contains("at most 16 KiB"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn app_shell_policy_is_explicit_and_checkpoints_survive_rebuilds() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        #[cfg(any(unix, windows))]
        assert!(app.agent.partial_output_checkpoint_stats().is_some());
        assert!(!app.config.tool_available("powershell"));
        app.config.tools = crate::config::ToolPolicy::only(["powershell".into()]).unwrap();
        assert_eq!(app.config.tool_available("powershell"), cfg!(windows));
        app.config.sandbox.allow_process = false;
        assert!(!app.config.tool_available("powershell"));
        app.config.sandbox.allow_process = true;
        app.config.sandbox.allow_shell = false;
        assert!(!app.config.tool_available("powershell"));
        // Restore fixture policy before checking the rebuild, not an unavailable
        // Windows-only explicit request on this platform.
        app.config.tools = Default::default();
        app.config.sandbox.allow_shell = true;
        app = rebuild_app(app, None, None, None, None).unwrap();
        #[cfg(any(unix, windows))]
        assert!(app.agent.partial_output_checkpoint_stats().is_some());
    }

    /// 2d.11 — `!command` results enter model context; `!!command` results are
    /// durably recorded but explicitly excluded from it.
    #[test]
    fn shell_escape_records_are_explicitly_included_or_excluded_from_context() {
        let (_directory, mut app) = fast_test_app(scripted_codex_model("http://127.0.0.1:1"));
        let included = commands::ShellEscapeRecord::new("printf hi", "hi", 0, false);
        record_shell_escape(&mut app, &included).unwrap();
        let context = serde_json::to_string(&app.agent.session().context().unwrap()).unwrap();
        assert!(context.contains("printf hi"), "{context}");
        assert!(context.contains("exit 0"), "{context}");

        let excluded = commands::ShellEscapeRecord::new("printf secret", "secret-value", 1, true);
        record_shell_escape(&mut app, &excluded).unwrap();
        let context = serde_json::to_string(&app.agent.session().context().unwrap()).unwrap();
        assert!(!context.contains("secret-value"), "{context}");
        assert!(!context.contains("printf secret"), "{context}");
        // The excluded execution is still durably accounted for, with an
        // explicit exclusion marker and its exact command and output.
        let head = app.agent.session().head().unwrap();
        let entry = app.agent.session().entry(&head).unwrap();
        let display = entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.display_text.as_deref())
            .expect("the excluded record retains its presentation text");
        assert!(
            display.contains("[excluded from model context]"),
            "{display}"
        );
        assert!(display.contains("printf secret"), "{display}");
        assert!(display.contains("secret-value"), "{display}");
    }

    /// A pending tool call must never be split from its result by a shell
    /// record, even for the context-including `!` form.
    #[test]
    fn an_unresolved_tool_call_keeps_an_included_record_out_of_context() {
        let (_directory, mut app) = fast_test_app(scripted_codex_model("http://127.0.0.1:1"));
        let model = app.model.spec.id.clone();
        app.agent
            .session_mut()
            .append(octet_agent::EntryValue::Message(
                octet_ai::Message::Assistant(octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                        async_execution: false,
                        id: ToolCallId("call-1".into()),
                        name: "bash".into(),
                        arguments_json: "{\"command\":\"ls\"}".into(),
                        argument_error: None,
                    })],
                    model,
                    protocol: octet_ai::Protocol::AnthropicMessages,
                }),
            ))
            .unwrap();
        assert!(session_has_unresolved_tool_calls(app.agent.session()));
        record_shell_escape(
            &mut app,
            &commands::ShellEscapeRecord::new("printf hi", "hi", 0, false),
        )
        .unwrap();
        let context = serde_json::to_string(&app.agent.session().context().unwrap()).unwrap();
        assert!(!context.contains("printf hi"), "{context}");
    }

    /// 2d.2 — the scope mutations materialize deterministically, toggle the
    /// requested provider, reorder by request, and hand back the exact ordered
    /// pattern list (with reasoning suffixes) the writer persists.
    #[test]
    fn scoped_models_mutations_materialize_toggle_reorder_and_persist() {
        let mut model = scripted_codex_model("http://127.0.0.1:1");
        std::sync::Arc::make_mut(&mut model.spec).id = ModelId("first".into());
        std::sync::Arc::make_mut(&mut model.spec).api_name = "first".into();
        let (_directory, mut app) = fast_test_app(model);
        let mut second = (*app.model.spec).clone();
        second.id = ModelId("second".into());
        second.api_name = "second".into();
        app.catalog.register_model(second).unwrap();

        // `all` builds the explicit ordered scope from the complete catalog in
        // a stable order and persists a re-selectable glob.
        let persistence =
            apply_scope_mutation(&mut app, &commands::ScopedModelsCommand::All).unwrap();
        assert_eq!(persistence, Some(Some("*".to_owned())));
        let all = app.model_cycle();
        assert!(all.contains(&"first".to_owned()), "{all:?}");
        assert!(all.contains(&"second".to_owned()), "{all:?}");
        assert!(
            all.windows(2).all(|pair| pair[0] < pair[1]),
            "`all` must materialize a stable order: {all:?}"
        );
        // `clear` removes the restriction and the persisted key; unrestricted
        // cycling follows the same stable catalog order.
        assert_eq!(
            apply_scope_mutation(&mut app, &commands::ScopedModelsCommand::Clear).unwrap(),
            Some(None)
        );
        assert_eq!(app.model_cycle(), all);

        // A narrow ordered scope exercises provider toggle and reorder exactly.
        app.set_model_scope_patterns(Some("first:low,second"))
            .unwrap();
        assert_eq!(app.model_cycle(), vec!["first", "second"]);
        // A provider toggle disables every model of that provider, then
        // restores them in stable order.
        assert_eq!(
            apply_scope_mutation(
                &mut app,
                &commands::ScopedModelsCommand::Toggle("test/*".into())
            )
            .unwrap(),
            Some(None)
        );
        assert!(app.model_cycle().is_empty());
        assert_eq!(
            apply_scope_mutation(
                &mut app,
                &commands::ScopedModelsCommand::Toggle("test/*".into())
            )
            .unwrap(),
            Some(Some("first,second".to_owned()))
        );
        // Reorder follows the requested move; the untouched entry keeps the
        // launch suffix in the persisted list.
        app.set_model_scope_patterns(Some("first:low,second"))
            .unwrap();
        assert_eq!(
            apply_scope_mutation(
                &mut app,
                &commands::ScopedModelsCommand::Move {
                    model: "second".into(),
                    direction: commands::ScopeMove::Top,
                }
            )
            .unwrap(),
            Some(Some("second,first:low".to_owned()))
        );
        assert_eq!(app.model_cycle(), vec!["second", "first"]);
        // Unmatched targets and boundary moves are explicit errors.
        assert!(apply_scope_mutation(
            &mut app,
            &commands::ScopedModelsCommand::Enable("nope/*".into())
        )
        .is_err());
        assert!(apply_scope_mutation(
            &mut app,
            &commands::ScopedModelsCommand::Move {
                model: "second".into(),
                direction: commands::ScopeMove::Up,
            }
        )
        .is_err());
    }

    /// 2d.1 / 2d.2 — the active-run path renders both reports immediately and
    /// queues the mutations it cannot own mid-run.
    #[tokio::test]
    async fn settings_and_scoped_models_render_mid_run_and_queue_mutations() {
        let directory = tempfile::tempdir().unwrap();
        let inspection = test_run_inspection_with_session(directory.path());
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");

        let (queue, quit) = run_active_command(
            &mut shell,
            Command::Settings(commands::SettingsCommand::Show),
            &inspection,
        )
        .await;
        assert!(queue.is_empty());
        assert!(!quit);
        assert!(shell.has_overlay());
        shell.close_overlay();

        let (queue, quit) = run_active_command(
            &mut shell,
            Command::Settings(commands::SettingsCommand::Images(Some(true))),
            &inspection,
        )
        .await;
        assert!(!quit);
        assert!(matches!(
            queue.back(),
            Some(PendingIdleAction::Settings(
                commands::SettingsCommand::Images(Some(true))
            ))
        ));

        let (queue, quit) = run_active_command(
            &mut shell,
            Command::ScopedModels(commands::ScopedModelsCommand::Show),
            &inspection,
        )
        .await;
        assert!(queue.is_empty());
        assert!(!quit);
        assert!(shell.has_overlay());
        shell.close_overlay();

        let (queue, quit) = run_active_command(
            &mut shell,
            Command::ScopedModels(commands::ScopedModelsCommand::All),
            &inspection,
        )
        .await;
        assert!(!quit);
        assert!(matches!(
            queue.back(),
            Some(PendingIdleAction::ScopedModels(
                commands::ScopedModelsCommand::All
            ))
        ));
    }

    /// 2d.11 active path: an approved escape runs immediately under the captured
    /// sandbox, and its explicit context decision is queued for the idle owner
    /// that owns the session writer.
    #[cfg(unix)]
    #[tokio::test]
    async fn active_shell_escape_runs_and_queues_its_context_decision() {
        let directory = tempfile::tempdir().unwrap();
        let inspection = test_run_inspection_with_session(directory.path());
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");
        let (queue, quit) = run_active_command(
            &mut shell,
            Command::Bash(commands::BashEscape {
                command: "printf octet-active-shell".into(),
                excluded: false,
            }),
            &inspection,
        )
        .await;
        assert!(!quit);
        let Some(PendingIdleAction::RecordShellEscape(record)) = queue.back() else {
            panic!("the active escape must queue its durable record: {queue:?}");
        };
        assert!(!record.excluded());
        assert_eq!(record.exit_code(), 0);
        assert!(record.output().contains("octet-active-shell"), "{record:?}");

        // `!!` fixes the opposite decision without touching execution.
        let (queue, quit) = run_active_command(
            &mut shell,
            Command::Bash(commands::BashEscape {
                command: "printf octet-excluded".into(),
                excluded: true,
            }),
            &inspection,
        )
        .await;
        assert!(!quit);
        let Some(PendingIdleAction::RecordShellEscape(record)) = queue.back() else {
            panic!("the excluded escape must still be accounted for: {queue:?}");
        };
        assert!(record.excluded());
    }

    /// The shared local-shell execution path keeps bounded capture, an exit
    /// status, and no refusal for an ordinary command.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_shell_escape_runs_through_the_bounded_capture_path() {
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        let outcome = run_local_shell(
            &mut shell,
            &mut input,
            Path::new("/"),
            &SandboxPolicy::default(),
            "printf octet-shell-test",
        )
        .await
        .unwrap();
        assert_eq!(outcome.exit_code, 0);
        assert!(
            outcome.output.contains("octet-shell-test"),
            "{}",
            outcome.output
        );
        assert!(outcome.refusal.is_none());
        assert!(!outcome.stopped);
        assert!(!outcome.shutting_down);
    }

    #[test]
    fn scoped_model_order_suffixes_and_rebuilds_preserve_launch_defaults() {
        let mut model = scripted_codex_model("http://127.0.0.1:1");
        Arc::make_mut(&mut model.spec).capabilities.reasoning =
            Some(octet_ai::ReasoningCapability {
                options: None,
                control: octet_ai::ReasoningControl::Effort,
                exposes_text: true,
                preserves_state: false,
                effort_budgets: None,
                openai_chat_mode: Default::default(),
                min_effort: octet_ai::ReasoningEffort::Minimal,
                max_effort: octet_ai::ReasoningEffort::High,
            });
        let (_directory, mut app) = fast_test_app(model);
        let mut second = (*app.model.spec).clone();
        second.id = ModelId("second".into());
        app.catalog.register_model(second).unwrap();
        app.set_model_scope_patterns(Some("second:low,scripted:high"))
            .unwrap();
        assert_eq!(app.model_cycle(), vec!["second", "scripted"]);
        assert_eq!(app.model.spec.id.0, "scripted");
        assert_eq!(
            app.reasoning,
            ReasoningConfig::Off,
            "scope handoff must not override launch defaults"
        );
        app = crate::app::apply_reconfig(app, Reconfig::Model(ModelId("second".into()))).unwrap();
        assert_eq!(
            app.reasoning,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low)
        );
        app = crate::app::apply_reconfig(app, Reconfig::Model(ModelId("scripted".into()))).unwrap();
        assert_eq!(
            app.reasoning,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
        );
        app = rebuild_app(app, None, None, None, None).unwrap();
        assert_eq!(app.model_cycle(), vec!["second", "scripted"]);
        // Removing an available route narrows, rather than widens, the scope.
        app.catalog
            .remove_model_if_endpoint(&ModelId("second".into()), &app.model.endpoint.id);
        assert_eq!(app.model_cycle(), vec!["scripted"]);
        app.set_model_scope_patterns(None).unwrap();
        let cycle = app.model_cycle();
        assert!(cycle.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[tokio::test]
    async fn session_and_hotkeys_reports_remain_read_only_during_runs() {
        let directory = tempfile::tempdir().unwrap();
        let inspection = test_run_inspection_with_session(directory.path());
        let before = std::fs::read(&inspection.session_path).unwrap();
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");
        for (command, expected) in [
            ("/session info", "Active-branch messages: 1"),
            ("/hotkeys", "app.model.cycleForward"),
        ] {
            let (queue, quit) =
                run_active_command(&mut shell, commands::parse(command), &inspection).await;
            assert!(queue.is_empty());
            assert!(!quit);
            assert!(shell.has_overlay());
            let text = if command == "/hotkeys" {
                shell.hotkeys_text()
            } else {
                commands::session_text(&inspection.read_only_session().unwrap())
            };
            assert!(text.contains(expected), "{text}");
            shell.close_overlay();
        }
        let (queue, quit) = run_active_command(&mut shell, Command::Copy, &inspection).await;
        assert!(queue.is_empty());
        assert!(!quit);
        assert!(shell
            .debug_error()
            .unwrap()
            .contains("no assistant message"));
        assert_eq!(std::fs::read(&inspection.session_path).unwrap(), before);
    }

    /// A narrowed launch opens `/model` without waiting for fleet discovery.
    /// Cancelling before completion cannot apply a late catalog to the app.
    #[tokio::test]
    async fn model_picker_cancel_keeps_the_narrowed_launch_catalog() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        let active = app.model.spec.id.clone();
        let narrowed = app.catalog.models().count();
        app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
        assert!(ActiveRunInspection::capture(&app).is_narrowed());
        let mut shell = InteractiveShell::test_shell();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
        assert!(open_model_picker(&mut app, &mut shell, &mut input)
            .await
            .unwrap()
            .is_none());
        assert!(!shell.has_panel());
        assert!(!app.readiness.is_fleet());
        assert_eq!(app.catalog.models().count(), narrowed);
        assert_eq!(app.model.spec.id, active);
    }

    #[test]
    fn picker_catalog_completion_keeps_the_active_route() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        let active = app.model.spec.id.clone();
        let narrowed = app.catalog.models().count();
        app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
        let (catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
            app.config.offline,
            &crate::app::bootstrap::CatalogReadiness::Fleet,
        )
        .unwrap();
        assert!(app.apply_picker_catalog(&active, catalog, notes).unwrap());
        assert!(app.readiness.is_fleet());
        assert!(app.catalog.models().count() >= narrowed);
        assert!(app.catalog.resolve(&active).is_ok());
        assert!(!ActiveRunInspection::capture(&app).is_narrowed());
    }

    #[test]
    fn picker_catalog_rejects_an_obsolete_selection() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
        let before = app.catalog.models().count();
        let (catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
            app.config.offline,
            &crate::app::bootstrap::CatalogReadiness::Fleet,
        )
        .unwrap();
        assert!(!app
            .apply_picker_catalog(&ModelId("obsolete".into()), catalog, notes)
            .unwrap());
        assert!(!app.readiness.is_fleet());
        assert_eq!(app.catalog.models().count(), before);
    }

    #[test]
    fn picker_catalog_rejects_a_withdrawn_active_route() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        let active = app.model.spec.id.clone();
        let endpoint = app.model.endpoint.id.clone();
        app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
        let (mut catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
            app.config.offline,
            &crate::app::bootstrap::CatalogReadiness::Fleet,
        )
        .unwrap();
        assert!(catalog.remove_model_if_endpoint(&active, &endpoint));
        assert!(app.apply_picker_catalog(&active, catalog, notes).is_err());
        assert!(!app.readiness.is_fleet());
        assert!(app.catalog.resolve(&active).is_ok());
    }

    fn fast_test_app(model: Model) -> (tempfile::TempDir, App) {
        let (directory, mut app) = crate::compaction::tests::app_for_estimate();
        app.catalog
            .register_endpoint((*model.endpoint).clone())
            .unwrap();
        app.catalog.register_model((*model.spec).clone()).unwrap();
        let app = rebuild_app(app, Some(model), None, None, None).unwrap();
        (directory, app)
    }

    fn fast_response() -> String {
        let output = serde_json::json!([{
            "id": "fast-message", "type": "message", "role": "assistant",
            "content": [{"type": "output_text", "text": "done", "annotations": []}]
        }]);
        [
            serde_json::json!({"type":"response.created", "response":{"id":"fast-response"}}),
            serde_json::json!({"type":"response.output_item.added", "output_index":0,
                "item":{"id":"fast-message", "type":"message"}}),
            serde_json::json!({"type":"response.output_text.delta", "output_index":0, "delta":"done"}),
            serde_json::json!({"type":"response.output_text.done", "output_index":0}),
            serde_json::json!({"type":"response.completed", "response":{"output":output,
                "usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}}}),
        ].into_iter().map(|event| format!("data: {event}\n\n")).collect()
    }

    async fn fast_idle(mut app: App, shell: &mut InteractiveShell, command: &str) -> App {
        let command = commands::parse(command);
        let Command::Fast(requested) = command else {
            panic!("expected fast control")
        };
        apply_fast_command(&mut app, shell, requested);
        schedule_idle_responses_prewarm(&app, &command);
        app
    }

    /// Exercise the slash handler, actual HTTP/SSE requests, status, route
    /// transitions, and rebuilds together; a setter-only assertion is not enough.
    #[tokio::test]
    async fn fast_commands_reach_live_requests_and_fail_closed_on_other_routes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(fast_response()),
            )
            .mount(&server)
            .await;
        let mut model = scripted_codex_model(&server.uri());
        Arc::make_mut(&mut model.spec).limits.context_window = 272_000;
        let (_directory, mut app) = fast_test_app(model.clone());
        let mut shell = InteractiveShell::test_shell();
        let before = std::fs::read(app.agent.session().path()).unwrap();
        app = fast_idle(app, &mut shell, "/fast").await;
        assert!(shell.debug_snapshot().contains("Fast mode: off"));
        assert_eq!(std::fs::read(app.agent.session().path()).unwrap(), before);
        assert!(!app.agent.session().has_uncertain_usage());
        assert!(server.received_requests().await.unwrap().is_empty());

        app = fast_idle(app, &mut shell, "/fast on").await;
        assert!(shell.debug_snapshot().contains("Fast mode: on"));
        assert!(commands::status_text(&app, None).contains("Fast mode: on"));
        assert!(commands::status_text(&app, None).contains("known subtotal"));
        assert!(app.agent.session().has_uncertain_usage());
        assert_eq!(app.model.spec.limits.context_window, 272_000);
        app.agent.complete("priority turn").await.unwrap();
        // Rebuilds for reasoning/reload must retain the real request control.
        app = rebuild_app(app, None, Some(ReasoningConfig::Off), None, None).unwrap();
        app.agent.complete("priority after rebuild").await.unwrap();
        let before = std::fs::read(app.agent.session().path()).unwrap();
        app = fast_idle(app, &mut shell, "/fast status").await;
        assert_eq!(std::fs::read(app.agent.session().path()).unwrap(), before);
        app = fast_idle(app, &mut shell, "/fast off").await;
        app.agent.complete("ordinary turn").await.unwrap();
        assert!(commands::status_text(&app, None).contains("Fast mode: off"));
        assert!(
            commands::cost_text(app.agent.session(), &app.model).contains("Known subtotal only")
        );
        // Clearing a request control does not erase uncertain spend.
        assert!(app.agent.session().has_uncertain_usage());
        assert_eq!(
            app.agent
                .session()
                .usage_uncertainty_records()
                .iter()
                .filter(|record| record.operation == "responses-priority-tier")
                .count(),
            1
        );

        app = fast_idle(app, &mut shell, "/fast on").await;
        let mut ordinary = model.clone();
        Arc::make_mut(&mut ordinary.endpoint)
            .runtime
            .responses_profile = octet_ai::ResponsesRuntimeProfile::Default;
        Arc::make_mut(&mut ordinary.endpoint).id = octet_ai::EndpointId("ordinary".into());
        Arc::make_mut(&mut ordinary.spec).endpoint = ordinary.endpoint.id.clone();
        Arc::make_mut(&mut ordinary.spec).id = ModelId("ordinary".into());
        app.catalog
            .register_endpoint((*ordinary.endpoint).clone())
            .unwrap();
        app.catalog
            .register_model((*ordinary.spec).clone())
            .unwrap();
        app = rebuild_app(app, Some(ordinary), None, None, None).unwrap();
        assert_eq!(
            app.agent.service_tier(),
            None,
            "unsupported model switches clear the tier"
        );
        let before = std::fs::read(app.agent.session().path()).unwrap();
        for command in ["/fast on", "/fast off", "/fast", "/fast status"] {
            app = fast_idle(app, &mut shell, command).await;
            assert!(shell
                .debug_error()
                .unwrap()
                .contains("only available on Codex Responses routes"));
        }
        assert_eq!(std::fs::read(app.agent.session().path()).unwrap(), before);
        app.agent
            .complete("unsupported route stays ordinary")
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            4,
            "status and rejected controls never invoke inference"
        );
        let bodies: Vec<serde_json::Value> = requests
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        assert_eq!(bodies[0]["service_tier"], "priority");
        assert_eq!(bodies[1]["service_tier"], "priority");
        assert!(bodies[2].get("service_tier").is_none());
        assert!(bodies[3].get("service_tier").is_none());
        for body in bodies {
            let body = body.to_string();
            assert!(
                !body.contains("/fast"),
                "local controls must not enter model context"
            );
        }
        app.agent.set_max_session_cost_microdollars(Some(u64::MAX));
        assert!(app
            .agent
            .complete("hard ceilings must fail closed")
            .await
            .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn fast_compaction_preserves_selection_and_restart_keeps_uncertainty_fenced() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(fast_response()),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/v1/responses/compact"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output":[{"type":"compaction", "id":"compact-fast", "encrypted_content":"checkpoint"}],
                "usage":{"input_tokens":5,"output_tokens":2}
            }))).mount(&server).await;
        let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri()));
        let mut shell = InteractiveShell::test_shell();
        app = fast_idle(app, &mut shell, "/fast on").await;
        app.agent.complete("before compaction").await.unwrap();
        app.config.compaction.mode = CompactionMode::NativeResponses;
        assert_eq!(
            attempt_compaction(&mut app).await.unwrap(),
            CompactionOutcome::NativeCompacted
        );
        assert_eq!(
            app.agent.service_tier(),
            Some(octet_ai::ServiceTier::Priority)
        );
        assert!(app.agent.session().has_uncertain_usage());
        app.agent.complete("after compaction").await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        for request in &requests {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            if request.url.path().ends_with("/compact") {
                // Compact has no declared tier field: never invent one or
                // promise that this auxiliary operation used priority.
                assert!(body.get("service_tier").is_none());
            } else {
                assert_eq!(body["service_tier"], "priority");
            }
        }

        // Reconstruct through startup, not a compatible idle rebuild: fast is
        // process-scoped, whereas past uncertain spend is durable and sticky.
        let session_path = app.agent.session().path().to_owned();
        let model = app.model.spec.id.clone();
        let catalog = app.catalog.clone();
        let mut config = app.config.clone();
        config.max_cost_microdollars = Some(u64::MAX);
        drop(app);
        let mut boot = crate::app::bootstrap::bootstrap(config).unwrap();
        boot.catalog = catalog;
        let mut app = build_app(
            boot,
            crate::app::bootstrap::LaunchSelection {
                model,
                session: SessionSelection::OpenExisting(session_path),
                reasoning: ReasoningConfig::Off,
                reasoning_mode: ReasoningMode::Standard,
            },
            "system".into(),
        )
        .unwrap();
        assert_eq!(app.agent.service_tier(), None);
        assert!(app.agent.session().has_uncertain_usage());
        // Match the budget error itself, not a guessed word in its display:
        // UsageUncertain reports "unsettled provider usage".
        assert_eq!(
            attempt_compaction(&mut app).await.unwrap(),
            CompactionOutcome::Skipped {
                reason: octet_agent::AgentError::UsageUncertain.to_string(),
            },
            "compaction must refuse the unsettled ledger before network I/O",
        );
        assert!(app.agent.complete("ceiling after restart").await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[test]
    fn unpriced_usage_marks_local_and_native_compaction_reports_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("compaction-report.jsonl");
        let mut session = Session::create(&path).unwrap();
        let first_kept = session
            .append(EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text("retained turn".into())],
                },
            )))
            .unwrap();
        session
            .record_compaction_usage(
                octet_ai::EndpointId("fixture".into()),
                ModelId("fixture".into()),
                octet_ai::Usage::default(),
                None,
            )
            .unwrap();
        // A later fully priced compaction does not repair the old missing cost.
        session
            .record_compaction_usage(
                octet_ai::EndpointId("fixture".into()),
                ModelId("fixture".into()),
                octet_ai::Usage::default(),
                Some(octet_ai::Cost::default()),
            )
            .unwrap();
        session.compact("summary", first_kept).unwrap();
        drop(session);
        let session = Session::open_read_only(path).unwrap();
        assert!(session.has_unpriced_usage());
        assert!(!session.has_uncertain_usage());
        for outcome in [
            CompactionOutcome::Compacted { elided: 1 },
            CompactionOutcome::NativeCompacted,
        ] {
            let mut shell = InteractiveShell::test_shell();
            report_compaction(&mut shell, &outcome, &session);
            assert!(shell
                .debug_snapshot()
                .contains("session usage or pricing uncertain"));
        }
    }

    #[tokio::test]
    async fn fast_local_summary_cost_ceiling_fails_closed_before_network_io() {
        let server = wiremock::MockServer::start().await;
        let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri()));
        seed_compaction_session(&mut app.agent);
        app.config.compaction.keep_recent_tokens = 1;
        app.set_fast_mode(true).unwrap();
        app.agent.set_max_session_cost_microdollars(Some(u64::MAX));
        // Match the budget error itself, not a guessed word in its display:
        // UsageUncertain reports "unsettled provider usage".
        assert_eq!(
            attempt_compaction(&mut app).await.unwrap(),
            CompactionOutcome::Skipped {
                reason: octet_agent::AgentError::UsageUncertain.to_string(),
            },
            "compaction must refuse the unsettled ledger before network I/O",
        );
        assert_eq!(
            app.agent.service_tier(),
            Some(octet_ai::ServiceTier::Priority)
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fast_unsupported_active_controls_are_not_queued() {
        let mut inspection = test_run_inspection().clone();
        // A profile alone must not lend a Chat route Responses capabilities.
        Arc::make_mut(&mut inspection.model.spec).protocol = octet_ai::Protocol::OpenAiChat;
        Arc::make_mut(&mut inspection.model.endpoint)
            .runtime
            .responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");
        for command in ["/fast", "/fast status", "/fast on", "/fast off"] {
            let (queue, quit) =
                run_active_command(&mut shell, commands::parse(command), &inspection).await;
            assert!(queue.is_empty());
            assert!(!quit);
            assert!(shell
                .debug_error()
                .unwrap()
                .contains("only available on Codex Responses routes"));
        }
    }

    #[test]
    fn fast_rebuild_scope_and_queue_barriers_are_explicit() {
        let model = scripted_codex_model("http://127.0.0.1:1");
        let (directory, mut app) = fast_test_app(model);
        app.set_fast_mode(true).unwrap();
        let path = app.agent.session().path().to_path_buf();
        app = rebuild_app(
            app,
            None,
            None,
            None,
            Some(SessionSelection::OpenExisting(path.clone())),
        )
        .unwrap();
        assert_eq!(
            app.agent.service_tier(),
            Some(octet_ai::ServiceTier::Priority)
        );
        let mut next = app.model.clone();
        Arc::make_mut(&mut next.spec).id = ModelId("another-scripted".into());
        app.catalog.register_model((*next.spec).clone()).unwrap();
        app = rebuild_app(app, Some(next), None, None, None).unwrap();
        assert_eq!(
            app.agent.service_tier(),
            Some(octet_ai::ServiceTier::Priority)
        );
        app = rebuild_app(
            app,
            None,
            None,
            None,
            Some(SessionSelection::CreateNew(
                directory.path().join("new-fast.jsonl"),
            )),
        )
        .unwrap();
        assert_eq!(app.agent.service_tier(), None);
        assert!(!app.agent.session().has_uncertain_usage());
        app = rebuild_app(
            app,
            None,
            None,
            None,
            Some(SessionSelection::OpenExisting(path)),
        )
        .unwrap();
        assert_eq!(
            app.agent.service_tier(),
            None,
            "resuming a different session does not silently opt into billing"
        );
        assert!(
            app.agent.session().has_uncertain_usage(),
            "old accounting remains sticky"
        );

        let mut queue = VecDeque::new();
        for action in [
            PendingIdleAction::Fast(true),
            PendingIdleAction::Fast(false),
            PendingIdleAction::NewSession,
            PendingIdleAction::Fast(true),
        ] {
            push_pending_action(&mut queue, action);
        }
        assert_eq!(
            queue.into_iter().collect::<Vec<_>>(),
            vec![
                PendingIdleAction::Fast(false),
                PendingIdleAction::NewSession,
                PendingIdleAction::Fast(true)
            ]
        );
    }

    #[tokio::test]
    async fn active_changelog_is_read_only_and_does_not_queue_or_interrupt() {
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");
        let before = shell.debug_snapshot();
        let (queue, quit_requested) =
            run_active_command(&mut shell, Command::Changelog, test_run_inspection()).await;
        assert!(shell.has_overlay());
        assert!(queue.is_empty());
        assert!(!quit_requested);
        assert_eq!(
            shell.debug_snapshot(),
            before,
            "release notes are not conversation"
        );
        assert_eq!(
            shell.overlay_input(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ))),
            OverlayInputResult::Closed
        );
        assert_eq!(
            shell.debug_snapshot(),
            before,
            "escape closes the report, not the run"
        );
    }

    #[tokio::test]
    async fn active_inspection_reports_render_without_waiting_for_the_idle_boundary() {
        let session_dir = tempfile::tempdir().expect("inspection fixture");
        let inspection = test_run_inspection_with_session(session_dir.path());
        for command in [
            Command::Help(None),
            Command::Context,
            Command::Cost,
            Command::Cache,
        ] {
            let mut shell = InteractiveShell::test_shell();
            shell.begin_run("test");
            let (queue, quit_requested) =
                run_active_command(&mut shell, command.clone(), &inspection).await;

            assert!(shell.has_overlay(), "{command:?} did not render a report");
            assert!(queue.is_empty(), "{command:?} must not wait for idle");
            assert!(!quit_requested);
            assert_eq!(shell.debug_error(), None, "{command:?} reported an error");
        }
    }

    #[tokio::test]
    async fn active_session_commands_report_through_the_read_only_session() {
        let session_dir = tempfile::tempdir().expect("inspection fixture");
        let inspection = test_run_inspection_with_session(session_dir.path());
        let mut shell = InteractiveShell::test_shell();
        shell.begin_run("test");

        // `/name` without an argument reports the name stored beside the live
        // session file; `/export` writes a portable copy of the same records.
        let (queue, quit_requested) =
            run_active_command(&mut shell, Command::Name(None), &inspection).await;
        assert!(queue.is_empty());
        assert!(!quit_requested);
        assert!(
            shell.debug_snapshot().contains("session name:"),
            "transcript: {}",
            shell.debug_snapshot()
        );

        let output = session_dir.path().join("exported.json");
        let (queue, quit_requested) = run_active_command(
            &mut shell,
            Command::Export(Some(output.display().to_string())),
            &inspection,
        )
        .await;
        assert!(queue.is_empty());
        assert!(!quit_requested);
        assert!(shell.has_overlay(), "export did not render a report");
        assert!(output.exists(), "export did not write {}", output.display());
        assert_eq!(shell.debug_error(), None);
    }

    #[tokio::test]
    async fn model_and_thinking_transitions_update_status_without_success_notices() {
        let (_workspace, mut app) = crate::compaction::tests::app_for_estimate();
        let mut shell = InteractiveShell::test_shell();
        update_status(&mut shell, &app);
        let mut input = futures_util::stream::pending();
        let model = ModelId("claude-sonnet-4-5".into());
        assert_ne!(shell.selected_identity().0, model.0);
        app = transition(app, &mut shell, &mut input, Reconfig::Model(model.clone()))
            .await
            .unwrap();
        assert_eq!(shell.selected_identity().0, model.0);
        assert!(shell.status_detail().contains(&model.0));
        assert!(shell.debug_snapshot().is_empty());

        for level in [ThinkingLevel::High, ThinkingLevel::Low, ThinkingLevel::High] {
            let reasoning =
                requested_thinking_to_reasoning(level, &app.model, app.subagents_available())
                    .unwrap();
            let label = reasoning_label(&reasoning);
            let previous = shell.selected_identity();
            app = transition(app, &mut shell, &mut input, Reconfig::Thinking(reasoning))
                .await
                .unwrap();
            assert_ne!(shell.selected_identity(), previous);
            assert_eq!(shell.selected_identity(), (model.0.clone(), label.clone()));
            assert!(shell.status_detail().contains(&label));
            assert!(
                shell.debug_snapshot().is_empty(),
                "success notices must not accumulate"
            );
            assert_eq!(shell.debug_error(), None);
            assert!(
                app.agent.session().entries().iter().any(|entry| matches!(
                    &entry.value,
                    EntryValue::Config { reasoning: Some(value), .. } if value == &label
                )),
                "configuration provenance must remain durable"
            );
        }
    }

    #[tokio::test]
    async fn failed_model_transition_returns_diagnostic_without_changing_status() {
        let (_workspace, app) = crate::compaction::tests::app_for_estimate();
        let mut shell = InteractiveShell::test_shell();
        update_status(&mut shell, &app);
        let identity = shell.selected_identity();
        let mut input = futures_util::stream::pending();
        let error = transition(
            app,
            &mut shell,
            &mut input,
            Reconfig::Model(ModelId("missing-release-test-model".into())),
        )
        .await
        .err()
        .expect("an unresolved model must remain an error");
        assert!(
            error.to_string().contains("missing-release-test-model"),
            "{error}"
        );
        assert_eq!(shell.selected_identity(), identity);
        assert!(shell.debug_snapshot().is_empty());
    }

    #[tokio::test]
    async fn queued_setting_changes_retain_acknowledgements_and_invalid_values_retain_errors() {
        for command in [
            Command::Model(Some("gpt-4o-mini".into())),
            Command::Thinking(Some("high".into())),
        ] {
            let mut shell = InteractiveShell::test_shell();
            let (queue, _) = run_active_command(&mut shell, command, test_run_inspection()).await;
            assert_eq!(queue.len(), 1);
            assert!(shell
                .debug_snapshot()
                .contains("command queued for the next idle boundary"));
            assert_eq!(shell.debug_error(), None);
        }
        let mut shell = InteractiveShell::test_shell();
        let (queue, _) = run_active_command(
            &mut shell,
            Command::Thinking(Some("invalid-effort".into())),
            test_run_inspection(),
        )
        .await;
        assert!(queue.is_empty());
        assert!(shell.debug_error().is_some());
        assert!(shell.debug_snapshot().is_empty());
    }

    #[test]
    fn starting_a_new_prompt_clears_the_previous_error() {
        let mut shell = InteractiveShell::test_shell();
        shell.error("old failure".to_string());
        assert_eq!(shell.debug_error().as_deref(), Some("old failure"));

        prepare_prompt(&mut shell);

        assert_eq!(shell.debug_error(), None);
    }

    fn text_turn() -> String {
        concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        )
        .to_owned()
    }

    fn scripted_model(uri: &str) -> octet_ai::Model {
        use octet_ai::{
            Auth, Capabilities, Endpoint, EndpointId, Modality, ModalitySet, ModelLimits,
            ModelSpec, Protocol,
        };
        use std::sync::Arc;
        use std::time::Duration;

        octet_ai::Model {
            spec: Arc::new(ModelSpec {
                preset: Default::default(),
                id: ModelId("scripted".into()),
                endpoint: EndpointId("test".into()),
                api_name: "scripted".into(),
                display_name: None,
                protocol: Protocol::AnthropicMessages,
                capabilities: Capabilities {
                    responses_features: Default::default(),
                    input_modalities: ModalitySet::none().with(Modality::Image),
                    output_modalities: ModalitySet::none(),
                    tools: true,
                    parallel_tool_calls: false,
                    reasoning: None,
                    responses_lite: false,
                    agent_delegation: None,
                    structured_output: false,
                    deferred_tool_loading: false,
                },
                limits: ModelLimits {
                    context_window: 16_000,
                    max_output_tokens: 1024,
                },
                pricing: None,
                cache: octet_ai::CacheCompatibility::default(),
            }),
            endpoint: Arc::new(Endpoint {
                id: EndpointId("test".into()),
                base_url: url::Url::parse(&format!("{uri}/v1/")).unwrap(),
                auth: Auth::None,
                default_headers: http::HeaderMap::new(),
                transport: octet_ai::EndpointTransport::Http,
                runtime: octet_ai::RequestRuntime::default(),
                timeout: Duration::from_secs(5),
            }),
        }
    }

    /// The same scripted fixture on the Codex Responses route, the only profile
    /// that declares the `service_tier` capability.
    fn scripted_codex_model(uri: &str) -> octet_ai::Model {
        let mut model = scripted_model(uri);
        std::sync::Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
        std::sync::Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
        model
    }

    /// The effort menu must offer the Codex context-window surface on a Codex
    /// route and nowhere else, and it must report the live effective window.
    #[test]
    fn codex_context_surface_follows_the_declared_route_and_the_effective_window() {
        let plain = scripted_model("http://127.0.0.1:1");
        assert!(
            codex_context_surface(&plain).is_none(),
            "a non-Codex route must not offer a Codex context-window surface"
        );

        let mut codex = scripted_codex_model("http://127.0.0.1:1");
        std::sync::Arc::make_mut(&mut codex.spec).id = octet_ai::ModelId("gpt-6-astra".into());
        std::sync::Arc::make_mut(&mut codex.spec)
            .limits
            .context_window = 272_000;
        let surface = codex_context_surface(&codex).expect("Codex route");
        assert_eq!(surface.effective_window(), 272_000);
        assert!(!surface.has_uncertain_usage());
        let row = pickers::codex_context_menu_row(&surface);
        assert!(row.contains("272000"), "{row}");
        let lines = surface.summary_lines().join("\n");
        assert!(lines.contains("272K"), "{lines}");
        assert!(
            lines.contains("effective 272K"),
            "the surface must label the effective window it reports: {lines}"
        );

        // An above-standard-tier route renders its accounting as uncertain and
        // never as an exact figure.
        std::sync::Arc::make_mut(&mut codex.spec).id = octet_ai::ModelId("gpt-5.6-luna".into());
        std::sync::Arc::make_mut(&mut codex.spec)
            .limits
            .context_window = 372_000;
        let uncertain = codex_context_surface(&codex).expect("Codex route");
        assert!(uncertain.has_uncertain_usage());
        assert!(pickers::codex_context_menu_row(&uncertain).contains("UNCERTAIN"));
        let lines = uncertain.summary_lines().join("\n");
        assert!(lines.contains("UNCERTAIN"), "{lines}");
        assert!(lines.contains("double-priced"), "{lines}");
        assert!(
            !lines.contains(octet_ai_operation_name()),
            "the internal operation id must never be rendered: {lines}"
        );
        assert!(!lines.contains("::"), "{lines}");
        assert!(
            !lines.contains('$'),
            "no exact-looking figure may be rendered above the standard tier: {lines}"
        );
    }

    fn octet_ai_operation_name() -> &'static str {
        crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION
    }

    /// Inspection facts for tests that drive `drive_active_run` directly. The
    /// paths do not exist: only the mechanics tests use this, and none of them
    /// issue an inspection command.
    ///
    /// The durable goal is deliberately unaddressable here, so this fixture is
    /// also the fail-closed case: a mechanics test that issued `/goal` would
    /// have to observe the typed error rather than a silent no-op.
    fn test_run_inspection() -> &'static ActiveRunInspection {
        static INSPECTION: std::sync::OnceLock<ActiveRunInspection> = std::sync::OnceLock::new();
        INSPECTION.get_or_init(|| {
            let missing = PathBuf::from("/nonexistent/octet-run-inspection");
            ActiveRunInspection {
                workspace: missing.clone(),
                invocation_cwd: missing.clone(),
                session_path: missing.join("session.jsonl"),
                model: scripted_model("http://127.0.0.1:1"),
                catalog: octet_ai::ModelCatalog::default(),
                sessions: crate::session_store::SessionStore::new(&missing, &missing),
                sandbox: SandboxPolicy::default(),
                effect_policy: octet_agent::EffectPolicy::UnsafeHost,
                subagents_available: false,
                service_tier: None,
                goal: Err(ActiveGoalError::UnaddressableSession),
                settings: commands::SettingsSurface {
                    default_model: None,
                    reasoning: "off".into(),
                    theme: None,
                    transport: "http",
                    endpoint: "test-endpoint".into(),
                    show_images: false,
                },
                model_scope: None,
                catalog_is_narrowed: false,
            }
        })
    }

    /// Inspection whose session path is a real, empty session file, so
    /// session-scoped reports render instead of failing to open.
    fn test_run_inspection_with_session(dir: &Path) -> ActiveRunInspection {
        let session_path = dir.join("session.jsonl");
        let mut created = octet_agent::Session::create(&session_path).expect("inspection session");
        // `/export` refuses a session with no resumable conversation, so the
        // fixture carries the smallest resumable turn.
        created
            .append(EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text("inspection fixture".into())],
                },
            )))
            .expect("inspection fixture prompt");
        drop(created);
        ActiveRunInspection {
            workspace: dir.to_path_buf(),
            invocation_cwd: dir.to_path_buf(),
            session_path,
            model: scripted_model("http://127.0.0.1:1"),
            catalog: octet_ai::ModelCatalog::default(),
            sessions: crate::session_store::SessionStore::for_directory(dir, dir),
            sandbox: SandboxPolicy::default(),
            effect_policy: octet_agent::EffectPolicy::UnsafeHost,
            subagents_available: false,
            service_tier: None,
            goal: Err(ActiveGoalError::UnaddressableSession),
            settings: commands::SettingsSurface {
                default_model: None,
                reasoning: "off".into(),
                theme: None,
                transport: "http",
                endpoint: "test-endpoint".into(),
                show_images: false,
            },
            model_scope: None,
            catalog_is_narrowed: false,
        }
    }

    /// A run inspection whose durable goal is addressable, exactly as `App`
    /// addresses it: the same store, the same driver state, the same session
    /// key. The caller keeps its own store handle to read the mutation back.
    fn test_run_inspection_with_goal(
        dir: &Path,
        store: Arc<octet_agent::DurableGoalStore>,
        driver: octet_agent::GoalDriver,
        session_id: &str,
    ) -> ActiveRunInspection {
        let mut inspection = test_run_inspection_with_session(dir);
        inspection.goal = GoalAccess::from_parts(store, driver, session_id.to_owned());
        inspection
    }

    /// Drive one active-run slash command with the minimum test scaffolding.
    async fn run_active_command(
        shell: &mut InteractiveShell,
        command: Command,
        inspection: &ActiveRunInspection,
    ) -> (VecDeque<PendingIdleAction>, bool) {
        let (queue, quit_requested, _) =
            run_active_command_observing_deadline(shell, command, inspection).await;
        (queue, quit_requested)
    }

    /// [`run_active_command`] plus the goal deadline the command armed or
    /// cleared, so a mid-run `/goal` is provably applied to the same driver
    /// state an idle `/goal` reaches.
    async fn run_active_command_observing_deadline(
        shell: &mut InteractiveShell,
        command: Command,
        inspection: &ActiveRunInspection,
    ) -> (VecDeque<PendingIdleAction>, bool, Option<Instant>) {
        let mut queue = VecDeque::new();
        let mut quit_requested = false;
        let mut goal_deadline = None;
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let context = octet_agent::ContextSnapshot::default();
        handle_active_command(
            shell,
            command,
            inspection,
            &mut extensions,
            &context,
            &mut goal_deadline,
            |_, _| Ok(None),
            &mut input,
            &mut queue,
            &mut quit_requested,
        )
        .await
        .expect("active command");
        (queue, quit_requested, goal_deadline)
    }

    async fn scripted_agent_with_delay(
        response_delay: Duration,
    ) -> (wiremock::MockServer, tempfile::TempDir, octet_agent::Agent) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_delay(response_delay)
                    .set_body_string(text_turn()),
            )
            .mount(&server)
            .await;

        let (workspace, agent) =
            scripted_agent_for_route(scripted_model(&server.uri()), octet_ai::AiClient::new());
        (server, workspace, agent)
    }

    fn scripted_agent_for_route(
        model: octet_ai::Model,
        client: octet_ai::AiClient,
    ) -> (tempfile::TempDir, octet_agent::Agent) {
        use octet_agent::{
            Agent, AgentConfig, CoreTools, EffectBroker, ExtensionHost, SandboxConfig, Session,
        };
        let workspace = tempfile::tempdir().unwrap();
        let session_path = workspace.path().join("session.jsonl");
        let mut extensions = ExtensionHost::new();
        extensions.load(&CoreTools);
        let mut sandbox = SandboxConfig::new(workspace.path());
        sandbox.allow_edit = true;
        sandbox.allow_process = true;
        let agent = Agent::new(AgentConfig {
            client,
            model,
            session: Session::create(&session_path).unwrap(),
            system: "test".into(),
            sandbox,
            effect_broker: EffectBroker::default(),
            extensions,
            max_turns: Some(4),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
            cache_retention: octet_ai::CacheRetention::default(),
            session_id: None,
        })
        .unwrap();
        (workspace, agent)
    }

    async fn scripted_agent() -> (wiremock::MockServer, tempfile::TempDir, octet_agent::Agent) {
        scripted_agent_with_delay(Duration::ZERO).await
    }

    // A real loopback HTTP response held after headers, independently of tokens.
    // No prompts or response bytes are written to diagnostics.
    struct HeldApi {
        uri: String,
        requests: Arc<std::sync::atomic::AtomicUsize>,
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for HeldApi {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl HeldApi {
        async fn start(
            body: String,
        ) -> (
            Self,
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<bool>,
        ) {
            Self::start_with_repeat(body, false).await
        }

        async fn start_with_repeat(
            body: String,
            repeat: bool,
        ) -> (
            Self,
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<bool>,
        ) {
            use std::sync::atomic::Ordering;
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let uri = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = requests.clone();
            let bodies = Arc::new(Mutex::new(Vec::new()));
            let captured = bodies.clone();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let mut started_tx = Some(started_tx);
                let mut release_rx = Some(release_rx);
                // The held first body must not block admission of a replacement
                // request after its client times out. This set owns that one
                // body task, so aborting the fixture also retires the socket.
                let mut held_responses = tokio::task::JoinSet::new();
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let header_end = loop {
                        let mut bytes = [0; 1024];
                        let n = socket.read(&mut bytes).await.unwrap();
                        assert!(n > 0);
                        request.extend_from_slice(&bytes[..n]);
                        assert!(request.len() < 128 * 1024, "bounded fixture request");
                        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    assert!(length < 128 * 1024);
                    while request.len() < header_end + length {
                        let mut bytes = [0; 1024];
                        let n = socket.read(&mut bytes).await.unwrap();
                        assert!(n > 0);
                        request.extend_from_slice(&bytes[..n]);
                    }
                    if repeat {
                        captured.lock().unwrap().push(
                            serde_json::from_slice(&request[header_end..header_end + length])
                                .unwrap(),
                        );
                    }
                    let attempt = counted.fetch_add(1, Ordering::SeqCst);
                    if attempt != 0 && !repeat {
                        // Bound an existing recovery policy without ever replaying
                        // the fixture's successful result on an unexpected POST.
                        socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
                        continue;
                    }
                    let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    socket.write_all(headers.as_bytes()).await.unwrap();
                    if attempt != 0 {
                        socket.write_all(body.as_bytes()).await.unwrap();
                        continue;
                    }
                    let _ = started_tx.take().unwrap().send(());
                    let release = release_rx.take().unwrap();
                    let body = body.clone();
                    held_responses.spawn(async move {
                        if let Ok(complete) = release.await {
                            let response = if complete {
                                body.as_bytes()
                            } else {
                                &body.as_bytes()[..1]
                            };
                            let _ = socket.write_all(response).await;
                        }
                    });
                }
            });
            (
                Self {
                    uri,
                    requests,
                    bodies,
                    task,
                },
                started_rx,
                release_tx,
            )
        }
    }

    /// Acknowledges only on the poll after the event's handler returned. This
    /// proves input handling while the API gate is still held, not after reply.
    struct ProbedInput {
        input: tokio_stream::wrappers::ReceiverStream<std::io::Result<Event>>,
        remaining: usize,
        handled: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Stream for ProbedInput {
        type Item = std::io::Result<Event>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if self.remaining == 0 {
                if let Some(handled) = self.handled.take() {
                    let _ = handled.send(());
                }
            }
            let event = Pin::new(&mut self.input).poll_next(context);
            if matches!(event, std::task::Poll::Ready(Some(_))) {
                self.remaining = self.remaining.saturating_sub(1);
            }
            event
        }
    }

    /// Input is acknowledged while the first response is held by a gate. The
    /// running request keeps its tier; only a subsequent run sees the change.
    #[tokio::test]
    async fn fast_active_commands_wait_for_ownership_before_changing_wire_requests() {
        use crossterm::event::KeyEvent;
        for initially_on in [false, true] {
            let (server, started, release) =
                HeldApi::start_with_repeat(fast_response(), true).await;
            let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri));
            let mut shell = InteractiveShell::test_shell();
            if initially_on {
                app = fast_idle(app, &mut shell, "/fast on").await;
            }
            update_status(&mut shell, &app);
            let inspection = ActiveRunInspection::capture(&app);
            let command = if initially_on {
                "/fast off"
            } else {
                "/fast on"
            };
            let events: Vec<_> = [command, "/fast"]
                .into_iter()
                .flat_map(|text| {
                    text.chars()
                        .map(|character| {
                            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
                        })
                        .chain(std::iter::once(KeyEvent::new(
                            KeyCode::Enter,
                            KeyModifiers::NONE,
                        )))
                })
                .collect();
            let (sender, receiver) = tokio::sync::mpsc::channel(32);
            let (handled_tx, handled) = tokio::sync::oneshot::channel();
            let mut input = ProbedInput {
                input: tokio_stream::wrappers::ReceiverStream::new(receiver),
                remaining: events.len(),
                handled: Some(handled_tx),
            };
            let mut pending = VecDeque::new();
            let mut ticker = tokio::time::interval(Duration::from_millis(1));
            let mut quit = false;
            let mut made_tool_call = false;
            let mut deadline = None;
            let run_id = shell.begin_run("test");
            let mut run = app.agent.prompt("held first turn").await.unwrap();
            let control = run.control();
            shell.set_awaiting_provider(run_id);
            let driver = drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut app.executable_extensions,
                &mut made_tool_call,
                &inspection,
                &mut deadline,
            );
            let producer = async {
                started.await.unwrap();
                for event in events {
                    sender.send(Ok(Event::Key(event))).await.unwrap();
                }
                handled
                    .await
                    .expect("slash commands handled before releasing response");
                assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
                release.send(true).unwrap();
            };
            let (ended, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(driver, producer)
            })
            .await
            .unwrap();
            drop(run);
            assert_eq!(ended.unwrap(), HostRunOutcome::Completed);
            assert!(!quit);
            assert_eq!(
                app.agent.service_tier(),
                initially_on.then_some(octet_ai::ServiceTier::Priority)
            );
            assert_eq!(
                pending.len(),
                1,
                "read-only status must not enqueue another toggle"
            );
            assert!(shell
                .debug_snapshot()
                .contains("queued for the next idle boundary"));
            let PendingIdleAction::Fast(enabled) = pending.pop_front().unwrap() else {
                panic!("wrong action")
            };
            assert_eq!(enabled, !initially_on);
            // The production idle queue uses this same handler after Run drops.
            apply_fast_command(&mut app, &mut shell, Some(enabled));
            assert!(pending.is_empty());
            app.agent
                .complete("second turn after idle change")
                .await
                .unwrap();
            let bodies = server.bodies.lock().unwrap();
            assert_eq!(bodies.len(), 2);
            for (body, priority) in bodies.iter().zip([initially_on, !initially_on]) {
                if priority {
                    assert_eq!(body["service_tier"], "priority");
                } else {
                    assert!(body.get("service_tier").is_none());
                }
                assert!(!body.to_string().contains("/fast"));
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum HeldOutcome {
        Success,
        Timeout,
        TransportFailure,
        Cancel,
    }

    fn seed_compaction_session(agent: &mut octet_agent::Agent) {
        for index in 0..5 {
            agent
                .session_mut()
                .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
                    octet_ai::UserMessage {
                        content: vec![octet_ai::UserPart::Text(format!("fixture user {index}"))],
                    },
                )))
                .unwrap();
            agent
                .session_mut()
                .append(octet_agent::EntryValue::Message(
                    octet_ai::Message::Assistant(octet_ai::AssistantMessage {
                        content: vec![octet_ai::AssistantPart::Text(format!(
                            "fixture assistant {index}"
                        ))],
                        model: ModelId("scripted".into()),
                        protocol: octet_ai::Protocol::AnthropicMessages,
                    }),
                ))
                .unwrap();
        }
        // Retain a user boundary, so this fixture exercises one summary
        // request rather than the separate split-turn-prefix summary request.
        agent
            .session_mut()
            .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text("retained fixture user".into())],
                },
            )))
            .unwrap();
    }

    async fn held_api_input(
        started: tokio::sync::oneshot::Receiver<()>,
        sender: tokio::sync::mpsc::Sender<std::io::Result<Event>>,
        handled: tokio::sync::oneshot::Receiver<()>,
        release: tokio::sync::oneshot::Sender<bool>,
        outcome: HeldOutcome,
        columns: u16,
    ) {
        use crossterm::event::KeyEvent;
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        sender.send(Ok(Event::Resize(columns, 8))).await.unwrap();
        sender
            .send(Ok(Event::Paste("draft while API waits".into())))
            .await
            .unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(250), handled)
            .await
            .expect("input handling budget while response is held: 250 ms")
            .unwrap();
        match outcome {
            HeldOutcome::Success => {
                release.send(true).unwrap();
            }
            HeldOutcome::TransportFailure => {
                release.send(false).unwrap();
            }
            HeldOutcome::Cancel => {
                sender
                    .send(Ok(Event::Key(KeyEvent::new(
                        KeyCode::Esc,
                        KeyModifiers::NONE,
                    ))))
                    .await
                    .unwrap();
                // Keep both gates alive until the driven operation has settled.
                std::future::pending::<()>().await;
            }
            HeldOutcome::Timeout => std::future::pending::<()>().await,
        }
        // An input EOF would itself abort an ordinary run, masking its result.
        std::future::pending::<()>().await;
    }

    #[tokio::test]
    async fn held_open_manual_compaction_keeps_input_live_and_settles_all_outcomes() {
        for outcome in [
            HeldOutcome::Success,
            HeldOutcome::Timeout,
            HeldOutcome::TransportFailure,
            HeldOutcome::Cancel,
        ] {
            for columns in [46, 80] {
                let (server, started, release) = HeldApi::start(text_turn()).await;
                let (_workspace, mut app) = crate::compaction::tests::app_for_estimate();
                // complete() drives the real streaming path even for a local
                // compaction summary. Only this fixture changes stream limits.
                app.client = octet_ai::AiClient::new()
                    .with_stream_timeouts(Duration::from_millis(750), Duration::from_secs(2));
                // Summaries now use Agent's retry/accounting client. Rebuild it
                // with the fixture's bounded stream timeouts before starting.
                app = rebuild_app(app, None, None, None, None).unwrap();
                app.agent
                    .set_compaction_model(Some(scripted_model(&server.uri)));
                seed_compaction_session(&mut app.agent);
                let force = columns == 46;
                let original_keep = if force { 99 } else { 1 };
                app.config.compaction.keep_recent_tokens = original_keep;
                let before = app.agent.session().entries().len();
                let mut shell = InteractiveShell::test_shell();
                let was_verbose = shell.verbose_tools();
                let (sender, receiver) = tokio::sync::mpsc::channel(8);
                let (handled_tx, handled_rx) = tokio::sync::oneshot::channel();
                let mut input = ProbedInput {
                    input: tokio_stream::wrappers::ReceiverStream::new(receiver),
                    remaining: 3,
                    handled: Some(handled_tx),
                };
                let stimulus =
                    held_api_input(started, sender, handled_rx, release, outcome, columns);
                tokio::pin!(stimulus);
                // 750ms initial body timeout + the shared summary policy's
                // 500ms first backoff + an immediate terminal HTTP 400 on the
                // replacement fits the original bound. Do not hide a blocked
                // fixture accept loop by extending this deadline.
                tokio::time::timeout(Duration::from_secs(3), async {
                    tokio::select! {
                        result = compact_interactively(&mut app, &mut shell, &mut input, force, None) => result,
                        _ = &mut stimulus => unreachable!(),
                    }
                }).await.unwrap_or_else(|error| panic!(
                    "held compaction must settle: outcome={outcome:?}, columns={columns}, requests={}, error={error}",
                    server.requests.load(std::sync::atomic::Ordering::SeqCst),
                ));
                let snapshot = shell.debug_snapshot();
                match outcome {
                    HeldOutcome::Success => {
                        assert!(snapshot.contains("Context compacted"), "{snapshot}")
                    }
                    HeldOutcome::Timeout | HeldOutcome::TransportFailure => {
                        assert!(snapshot.contains("compaction skipped"), "{snapshot}")
                    }
                    HeldOutcome::Cancel => {
                        assert!(snapshot.contains("compaction cancelled"), "{snapshot}")
                    }
                }
                assert_eq!(app.config.compaction.keep_recent_tokens, original_keep);
                assert_eq!(shell.pending(), "draft while API waits");
                assert_ne!(shell.verbose_tools(), was_verbose);
                let expected_requests = match outcome {
                    HeldOutcome::Timeout | HeldOutcome::TransportFailure => 2,
                    HeldOutcome::Success | HeldOutcome::Cancel => 1,
                };
                assert_eq!(
                    server.requests.load(std::sync::atomic::Ordering::SeqCst),
                    expected_requests,
                    "the first summary replacement must hit the fixture's terminal rejection: {outcome:?}",
                );
                if !matches!(outcome, HeldOutcome::Success) {
                    assert!(app.agent.session().has_uncertain_usage(), "{outcome:?}");
                    assert!(app.agent.session().usage_records().is_empty(),
                        "failed/abandoned summaries have unknown usage, not invented successful receipts");
                    assert_eq!(
                        app.agent.session().entries().len(),
                        before,
                        "unsettled compaction must not append a summary"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn held_open_model_request_keeps_input_live_and_settles_once() {
        for outcome in [
            HeldOutcome::Success,
            HeldOutcome::Timeout,
            HeldOutcome::TransportFailure,
            HeldOutcome::Cancel,
        ] {
            let (server, started, release) = HeldApi::start(text_turn()).await;
            let client = octet_ai::AiClient::new()
                .with_stream_timeouts(Duration::from_millis(750), Duration::from_secs(2));
            let (_workspace, mut agent) =
                scripted_agent_for_route(scripted_model(&server.uri), client);
            let mut shell = InteractiveShell::test_shell();
            let was_verbose = shell.verbose_tools();
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("initial").await.unwrap();
            shell.set_awaiting_provider(run_id);
            let control = run.control();
            let (sender, receiver) = tokio::sync::mpsc::channel(8);
            let (handled_tx, handled_rx) = tokio::sync::oneshot::channel();
            let mut input = ProbedInput {
                input: tokio_stream::wrappers::ReceiverStream::new(receiver),
                remaining: 3,
                handled: Some(handled_tx),
            };
            let mut ticker = tokio::time::interval(Duration::from_millis(16));
            let mut pending = VecDeque::new();
            let mut quit = false;
            let mut made_tool_call = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let stimulus = held_api_input(started, sender, handled_rx, release, outcome, 80);
            tokio::pin!(stimulus);
            let ended = tokio::time::timeout(Duration::from_secs(5), async {
                let mut goal_deadline = None;
                tokio::select! {
                    result = drive_active_run(&mut run, &control, &mut shell, &mut input,
                        &mut ticker, &mut pending, &mut quit, None, None, &mut extensions, &mut made_tool_call,
                        test_run_inspection(), &mut goal_deadline) => result.unwrap(),
                    _ = &mut stimulus => unreachable!(),
                }
            }).await.expect("held model request must settle");
            assert!(run.next().await.is_none(), "exactly one terminal event");
            drop(run);
            match outcome {
                HeldOutcome::Success => assert_eq!(ended, HostRunOutcome::Completed),
                HeldOutcome::Cancel => assert_eq!(ended, HostRunOutcome::Aborted),
                HeldOutcome::Timeout | HeldOutcome::TransportFailure => {
                    assert!(matches!(ended, HostRunOutcome::Failed(_)))
                }
            }
            assert_eq!(shell.pending(), "draft while API waits");
            assert_ne!(shell.verbose_tools(), was_verbose);
            assert_eq!(agent.session().checkpoints().len(), 1);
            if !matches!(outcome, HeldOutcome::TransportFailure) {
                assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
            }
            assert!(!quit);
        }
    }

    #[tokio::test]
    async fn caller_driven_run_settles_and_cancels_while_real_renderer_is_gated() {
        // The independent watchdog releases a stalled renderer even when an
        // inline render/held mutex prevents this Tokio thread polling a timeout.
        struct ReleaseGate(Box<dyn Fn()>);
        impl Drop for ReleaseGate {
            fn drop(&mut self) {
                (self.0)();
            }
        }
        for cancel in [false, true] {
            let (server, started, release_response) = HeldApi::start(text_turn()).await;
            let (_workspace, mut agent) =
                scripted_agent_for_route(scripted_model(&server.uri), octet_ai::AiClient::new());
            let (mut shell, gate) = InteractiveShell::test_blocked_renderer();
            // Declared after shell: always releases before shell's join on unwind.
            let release_gate = gate.clone();
            let _release = ReleaseGate(Box::new(move || release_gate.release()));
            let (settled_tx, settled_rx) = std::sync::mpsc::channel();
            let watchdog_gate = gate.clone();
            let watchdog = std::thread::spawn(move || {
                let settled_while_blocked = settled_rx.recv_timeout(Duration::from_secs(5)).is_ok();
                watchdog_gate.release();
                settled_while_blocked
            });
            assert!(gate.wait_until_entered(Duration::from_secs(3)));
            let (sender, receiver) = tokio::sync::mpsc::channel(8);
            let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
            let mut ticker = tokio::time::interval(Duration::from_millis(1));
            let mut pending = VecDeque::new();
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("settle independently of paint").await.unwrap();
            let control = run.control();
            shell.set_awaiting_provider(run_id);
            let stimulus = async move {
                started.await.unwrap();
                if cancel {
                    // Empty-draft Ctrl+C must reach the real Agent control and
                    // the caller must continue polling through RunFinished.
                    sender.send(Ok(ctrl_key('c'))).await.unwrap();
                    let _keep_response_held = release_response;
                    std::future::pending::<()>().await;
                } else {
                    release_response.send(true).unwrap();
                    let _keep_input_open = sender;
                    std::future::pending::<()>().await;
                }
            };
            tokio::pin!(stimulus);
            let mut made_tool_call = false;
            let mut deadline = None;
            let result = tokio::select! {
                result = drive_active_run(
                    &mut run, &control, &mut shell, &mut input, &mut ticker,
                    &mut pending, &mut quit, None, None, &mut extensions,
                    &mut made_tool_call, test_run_inspection(), &mut deadline,
                ) => result.unwrap(),
                _ = &mut stimulus => unreachable!(),
            };
            assert_eq!(
                result,
                if cancel {
                    HostRunOutcome::Aborted
                } else {
                    HostRunOutcome::Completed
                }
            );
            assert!(
                run.next().await.is_none(),
                "driver must consume the terminal outcome"
            );
            drop(run);
            assert_eq!(agent.session().checkpoints().len(), 1);
            assert!(pending.is_empty());
            assert!(!quit);
            settled_tx.send(()).ok();
            assert!(
                watchdog.join().unwrap(),
                "Run only settled after the renderer watchdog released layout: cancel={cancel}"
            );
        }
    }

    fn reasoning_control_model(uri: &str) -> Model {
        let mut model = scripted_model(uri);
        let spec = Arc::make_mut(&mut model.spec);
        spec.protocol = octet_ai::Protocol::OpenAiResponses;
        spec.capabilities
            .responses_features
            .reasoning_effort_updates = true;
        spec.capabilities.reasoning = Some(octet_ai::ReasoningCapability {
            options: Some(octet_ai::types::ReasoningOptions {
                values: vec!["none".into(), "low".into(), "high".into()],
                default: Some("low".into()),
            }),
            control: octet_ai::ReasoningControl::Effort,
            exposes_text: true,
            preserves_state: true,
            effort_budgets: None,
            openai_chat_mode: octet_ai::OpenAiChatReasoningMode::Standard,
            min_effort: octet_ai::ReasoningEffort::Low,
            max_effort: octet_ai::ReasoningEffort::High,
        });
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features
            .reasoning_effort_updates = true;
        model
    }

    #[tokio::test]
    async fn thinking_control_preserves_active_run_and_hands_off_wire_update() {
        // Exercise real preference persistence without modifying the developer HOME.
        const CHILD: &str = "OCTET_TEST_REASONING_CONTROL_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "modes::interactive::tests::thinking_control_preserves_active_run_and_hands_off_wire_update", "--nocapture"])
                .env(CHILD, "1").env("HOME", home.path()).output().unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
            return;
        }
        use crossterm::event::KeyEvent;
        for (requested, qualified) in [
            ("high", true),
            ("medium", true),
            ("ultra", true),
            ("high", false),
        ] {
            let accepted = requested == "high" && qualified;
            let (server, started, release) =
                HeldApi::start_with_repeat(fast_response(), true).await;
            let mut model = reasoning_control_model(&server.uri);
            Arc::make_mut(&mut model.endpoint)
                .runtime
                .responses_features
                .reasoning_effort_updates = qualified;
            let (_workspace, mut agent) =
                scripted_agent_for_route(model.clone(), octet_ai::AiClient::new());
            let mut inspection = test_run_inspection().clone();
            inspection.model = model;
            inspection.session_path = agent.session().path().to_path_buf();
            let mut shell = InteractiveShell::test_shell();
            shell.set_identity("test", "scripted", "off");
            let events: Vec<_> = format!("/thinking {requested}")
                .chars()
                .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
                .chain(std::iter::once(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))))
                .collect();
            let (sender, receiver) = tokio::sync::mpsc::channel(32);
            let (handled_tx, handled) = tokio::sync::oneshot::channel();
            let mut input = ProbedInput {
                input: tokio_stream::wrappers::ReceiverStream::new(receiver),
                remaining: events.len(),
                handled: Some(handled_tx),
            };
            let mut pending = VecDeque::new();
            let mut ticker = tokio::time::interval(Duration::from_millis(1));
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let id = shell.begin_run("test");
            let mut run = agent.prompt("keep the root alive").await.unwrap();
            let control = run.control();
            shell.set_awaiting_provider(id);
            let mut deadline = None;
            let mut made_tool_call = false;
            let driver = drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut extensions,
                &mut made_tool_call,
                &inspection,
                &mut deadline,
            );
            let stimulus = async move {
                started.await.unwrap();
                for event in events {
                    sender.send(Ok(event)).await.unwrap();
                }
                handled.await.unwrap();
                release.send(true).unwrap();
                std::future::pending::<()>().await;
            };
            tokio::pin!(stimulus);
            let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! { result = driver => result.unwrap(), _ = &mut stimulus => unreachable!() }
        }).await.unwrap();
            assert_eq!(outcome, HostRunOutcome::Completed);
            drop(run);
            assert!(!quit);
            if qualified {
                assert!(
                    pending.is_empty(),
                    "qualified control never rebuilds at idle"
                );
            } else {
                assert_eq!(
                    pending.front(),
                    Some(&PendingIdleAction::ChangeThinkingLevel(ThinkingLevel::High))
                );
            }
            assert_eq!(agent.session().checkpoints().len(), 1);
            assert_eq!(
                agent.reasoning(),
                &if accepted {
                    ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
                } else {
                    ReasoningConfig::Off
                }
            );
            assert_eq!(
                shell.selected_identity().1,
                if accepted { "high" } else { "off" }
            );
            let bodies = server.bodies.lock().unwrap();
            if !accepted {
                assert_eq!(
                    bodies.len(),
                    1,
                    "rejected effort must not create a model response"
                );
                assert!(!agent.session().entries().iter().any(|entry| matches!(
                    &entry.value,
                    EntryValue::ResponsesReasoning {
                        update: Some(_),
                        ..
                    }
                )));
                if qualified {
                    assert!(shell.debug_error().unwrap().contains("not supported"));
                } else {
                    assert!(shell.debug_error().is_none());
                    assert!(shell.debug_snapshot().contains("next idle boundary"));
                }
                continue;
            }
            assert!(shell
                .debug_snapshot()
                .contains("not provider acknowledgement"));
            assert_eq!(bodies.len(), 2);
            assert_eq!(bodies[0]["reasoning"]["effort"], "none");
            assert_eq!(
                bodies[1]["reasoning"]["effort"], "none",
                "wire baseline stays pinned"
            );
            assert!(bodies[1]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "configuration_update"
                    && item["reasoning"]["effort"] == "high"));
            assert!(
                std::fs::read_to_string(crate::cli::global_config_path().unwrap())
                    .unwrap()
                    .contains("high")
            );
        }
    }

    #[tokio::test]
    async fn idle_thinking_rejection_preserves_session_and_startup_preference() {
        const CHILD: &str = "OCTET_TEST_IDLE_THINKING_PREFERENCE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "modes::interactive::tests::idle_thinking_rejection_preserves_session_and_startup_preference", "--nocapture"])
                .env(CHILD, "1").env("HOME", home.path()).output().unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
            return;
        }
        let mut model = reasoning_control_model("http://127.0.0.1:1");
        let capabilities = &mut Arc::make_mut(&mut model.spec).capabilities;
        capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
        let capability = capabilities.reasoning.as_mut().unwrap();
        capability.max_effort = octet_ai::ReasoningEffort::Ultra;
        capability
            .options
            .as_mut()
            .unwrap()
            .values
            .push("ultra".into());
        // Capability admission succeeds; only the pinned session makes the
        // requested transition invalid. No live subagent/inference is needed.
        let ultra = requested_thinking_to_reasoning(ThinkingLevel::Ultra, &model, true).unwrap();
        let (_workspace, mut app) = fast_test_app(model);
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending();
        app = select_thinking(
            app,
            &mut shell,
            &mut input,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
            None,
        )
        .await
        .unwrap();
        let preference = crate::cli::global_config_path().unwrap();
        let config_before = std::fs::read(&preference).unwrap();
        assert!(String::from_utf8_lossy(&config_before).contains("high"));
        let session = app.agent.session().path().to_path_buf();
        let session_before = std::fs::read(&session).unwrap();
        let identity = shell.selected_identity();
        // None is the slash/shortcut path; Some(Standard) is picker selection.
        for mode in [None, Some((ReasoningMode::Standard, ThinkingLevel::Ultra))] {
            shell.clear_error();
            app = select_thinking(app, &mut shell, &mut input, ultra.clone(), mode)
                .await
                .unwrap();
            assert_eq!(std::fs::read(&preference).unwrap(), config_before);
            assert_eq!(std::fs::read(&session).unwrap(), session_before);
            assert_eq!(shell.selected_identity(), identity);
            assert_eq!(
                app.reasoning,
                ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
            );
            assert_eq!(app.agent.reasoning(), &app.reasoning);
            let error = shell.debug_error().unwrap();
            assert!(
                error.contains("thinking unchanged") && error.contains("new session"),
                "{error}"
            );
        }
    }

    #[tokio::test]
    async fn thinking_control_idle_is_durable_and_rejected_effort_leaves_session_unchanged() {
        let model = reasoning_control_model("http://127.0.0.1:1");
        let (_workspace, mut app) = fast_test_app(model.clone());
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending();
        let session = app.agent.session().path().to_path_buf();
        app = transition(
            app,
            &mut shell,
            &mut input,
            Reconfig::Thinking(ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)),
        )
        .await
        .unwrap();
        assert_eq!(app.agent.session().path(), session);
        assert_eq!(
            app.reasoning,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
        );
        assert_eq!(shell.selected_identity().1, "high");
        let before = std::fs::read(&session).unwrap();
        for level in [ThinkingLevel::Medium, ThinkingLevel::Ultra] {
            assert!(requested_thinking_to_reasoning(level, &app.model, false).is_err());
        }
        app = transition(
            app,
            &mut shell,
            &mut input,
            Reconfig::Thinking(ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium)),
        )
        .await
        .unwrap();
        assert_eq!(app.model.spec.id, model.spec.id);
        assert_eq!(
            app.reasoning,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
        );
        assert_eq!(std::fs::read(&session).unwrap(), before);
        assert!(shell.debug_error().unwrap().contains("thinking unchanged"));
        let resumed = Session::open_read_only(&session).unwrap();
        assert_eq!(
            resumed
                .responses_reasoning(&model.endpoint.id, &model.spec.id)
                .unwrap()
                .unwrap()
                .1,
            app.reasoning
        );
        let mut codex = model;
        Arc::make_mut(&mut codex.spec)
            .capabilities
            .reasoning
            .as_mut()
            .unwrap()
            .options
            .as_mut()
            .unwrap()
            .values
            .remove(0);
        assert!(requested_thinking_to_reasoning(ThinkingLevel::Off, &codex, false).is_err());
        Arc::make_mut(&mut codex.endpoint)
            .runtime
            .responses_features = Default::default();
        assert!(
            !codex.responses_features().reasoning_effort_updates,
            "unknown routes keep selector fallback"
        );
    }

    #[tokio::test]
    async fn active_model_and_thinking_panels_do_not_suspend_run() {
        use crossterm::event::KeyEvent;
        for command in ["/model", "/thinking"] {
            let (server, started, release) = HeldApi::start(text_turn()).await;
            let (_workspace, mut agent) =
                scripted_agent_for_route(scripted_model(&server.uri), octet_ai::AiClient::new());
            let mut inspection = test_run_inspection().clone();
            inspection
                .catalog
                .register_endpoint((*inspection.model.endpoint).clone())
                .unwrap();
            inspection
                .catalog
                .register_model((*inspection.model.spec).clone())
                .unwrap();
            let mut shell = InteractiveShell::test_shell();
            let events: Vec<_> = command
                .chars()
                .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
                .chain(std::iter::once(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))))
                .collect();
            let (sender, receiver) = tokio::sync::mpsc::channel(32);
            let (handled_tx, handled) = tokio::sync::oneshot::channel();
            let mut input = ProbedInput {
                input: tokio_stream::wrappers::ReceiverStream::new(receiver),
                remaining: events.len(),
                handled: Some(handled_tx),
            };
            let mut pending = VecDeque::new();
            let mut ticker = tokio::time::interval(Duration::from_millis(1));
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let run_id = shell.begin_run("test");
            let mut run = agent
                .prompt("complete while the picker remains open")
                .await
                .unwrap();
            let control = run.control();
            shell.set_awaiting_provider(run_id);
            let mut deadline = None;
            let mut made_tool_call = false;
            let driver = drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut extensions,
                &mut made_tool_call,
                &inspection,
                &mut deadline,
            );
            let stimulus = async move {
                started.await.unwrap();
                for event in events {
                    sender.send(Ok(event)).await.unwrap();
                }
                handled.await.unwrap();
                // No Escape, Enter, or EOF follows opening the panel. This
                // failed previously because the modal stopped polling Run.
                release.send(true).unwrap();
                std::future::pending::<()>().await;
            };
            tokio::pin!(stimulus);
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! { result = driver => result.unwrap(), _ = &mut stimulus => unreachable!() }
            }).await.expect("an open picker must not stall settlement");
            assert_eq!(result, HostRunOutcome::Completed, "{command}");
            assert!(run.next().await.is_none());
            drop(run);
            assert_eq!(agent.session().checkpoints().len(), 1);
            assert!(
                pending.is_empty(),
                "opening or settlement must not imply selection"
            );
            assert!(!quit);
            assert!(
                !shell.has_panel(),
                "settlement cannot leave a driverless modal"
            );
            assert!(shell.debug_snapshot().contains("done"));
        }
    }

    #[tokio::test]
    async fn active_modal_escape_ctrl_c_and_close_keep_their_owners() {
        use crossterm::event::KeyEvent;
        for (draft, keys, expected, closing) in [
            (
                "draft",
                vec![
                    ctrl_key('c'),
                    Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                ],
                HostRunOutcome::Completed,
                false,
            ),
            ("", vec![ctrl_key('c')], HostRunOutcome::Aborted, false),
            ("draft", vec![ctrl_key('d')], HostRunOutcome::Aborted, true),
        ] {
            let (_server, _workspace, mut agent) =
                scripted_agent_with_delay(Duration::from_millis(100)).await;
            let mut shell = InteractiveShell::test_shell();
            shell.extension_set_editor(draft.into());
            let (sender, receiver) = tokio::sync::mpsc::channel(8);
            sender.send(Ok(ctrl_key('l'))).await.unwrap();
            for key in keys {
                sender.send(Ok(key)).await.unwrap();
            }
            let _sender = sender;
            let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
            let mut ticker = tokio::time::interval(Duration::from_millis(1));
            let mut pending = VecDeque::new();
            let mut quit = false;
            let mut extensions = crate::extensions::ExecutableExtensions::default();
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("initial").await.unwrap();
            let control = run.control();
            shell.set_awaiting_provider(run_id);
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut ticker,
                    &mut pending,
                    &mut quit,
                    None,
                    None,
                    &mut extensions,
                    &mut false,
                    test_run_inspection(),
                    &mut None,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(result, expected);
            assert_eq!(quit, closing);
            assert_eq!(shell.pending(), if closing { draft } else { "" });
            assert!(pending.is_empty());
        }
    }

    #[tokio::test]
    async fn active_tool_consent_requires_a_visible_fresh_confirmation_and_drop_denies() {
        use crossterm::event::{KeyEvent, KeyEventKind};
        let (sink, mut progress) = ToolProgressSink::bounded_channel();
        let answer = tokio::spawn(async move {
            sink.confirmation(
                "Approve fixture effect?".into(),
                Some("Consequence retained".into()),
                true,
                true,
            )
            .await
        });
        let ToolProgress::Confirmation(request) = progress.recv().await.unwrap() else {
            panic!("confirmation")
        };
        let mut interaction = ActiveToolInteraction {
            id: ToolCallId("fixture".into()),
            tool: Some("write".into()),
            request: ActiveToolRequest::Confirmation(request),
        };
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        interaction.open(&mut shell);
        assert!(!interaction.input(
            &mut shell,
            &Event::Key(KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Repeat
            ))
        ));
        assert!(!answer.is_finished(), "repeat must not approve");
        shell.set_size(1, 1);
        assert!(!interaction.input(
            &mut shell,
            &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        ));
        assert!(!answer.is_finished(), "invisible action must not approve");
        drop(interaction);
        assert!(
            !answer.await.unwrap(),
            "cancel/settlement/error must deny unanswered requests"
        );
    }

    #[tokio::test]
    async fn cancellation_retains_answer_draft_and_ordered_undelivered_steering() {
        use crossterm::event::KeyEvent;
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_secs(2)).await;
        let mut shell = InteractiveShell::test_shell();
        let events = [
            Event::Paste("first queued".into()),
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Event::Paste("second queued".into()),
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Event::Paste("/answer preserve this instruction".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            // Repeated close/submit keys must not duplicate or drain the draft.
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
        ];
        let mut input =
            tokio_stream::iter(events.into_iter().map(Ok)).chain(futures_util::stream::pending());
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut ticker = tokio::time::interval(Duration::from_millis(16));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut goal_deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(1),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut extensions,
                &mut false,
                test_run_inspection(),
                &mut goal_deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        drop(run);
        assert_eq!(ended, HostRunOutcome::Aborted);
        assert_eq!(
            shell.pending(),
            "first queued\n\nsecond queued\n\n/answer preserve this instruction"
        );
        assert!(!shell.debug_snapshot().contains("Steering:"));
        assert_eq!(agent.session().checkpoints().len(), 1);
        assert_eq!(
            agent.session().context().unwrap().len(),
            1,
            "undelivered input is not durable or replayed"
        );
    }

    #[tokio::test]
    async fn queued_follow_ups_dispatch_only_after_completion_or_escape_settlement() {
        use crossterm::event::KeyEvent;
        for (cancel, dispatch, quit_expected) in [
            (
                Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                true,
                false,
            ),
            (
                Some(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                false,
                false,
            ),
            (
                Some(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                false,
                true,
            ),
            (None, true, false),
        ] {
            let (_server, _workspace, mut agent) =
                scripted_agent_with_delay(Duration::from_millis(10)).await;
            let mut shell = InteractiveShell::test_shell();
            let mut events = vec![
                Event::Paste("queued first".into()),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                Event::Paste("queued second".into()),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)),
                Event::Paste(" edited".into()),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ];
            if let Some(cancel) = cancel {
                events.push(Event::Key(cancel));
                // Neither repeated Escape nor a second fresh Escape can arm a
                // Ctrl+C cancellation after settlement has already started.
                events.push(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                    KeyEventKind::Repeat,
                )));
                events.push(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            }
            let mut input = tokio_stream::iter(events.into_iter().map(Ok))
                .chain(futures_util::stream::pending());
            let run_id = shell.begin_run("test");
            let mut run = agent.prompt("initial").await.unwrap();
            shell.set_awaiting_provider(run_id);
            let control = run.control();
            let mut ticker = tokio::time::interval(Duration::from_millis(16));
            let mut quit = false;
            let mut goal_deadline = None;
            let ended = tokio::time::timeout(
                Duration::from_secs(2),
                drive_active_run(
                    &mut run,
                    &control,
                    &mut shell,
                    &mut input,
                    &mut ticker,
                    &mut VecDeque::new(),
                    &mut quit,
                    None,
                    None,
                    &mut crate::extensions::ExecutableExtensions::default(),
                    &mut false,
                    test_run_inspection(),
                    &mut goal_deadline,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(run.next().await.is_none(), "one terminal event");
            drop(run);
            assert_eq!(quit, quit_expected);
            assert_eq!(
                ended,
                if cancel.is_some() {
                    HostRunOutcome::Aborted
                } else {
                    HostRunOutcome::Completed
                }
            );
            assert_eq!(agent.session().checkpoints().len(), 1);
            assert_eq!(
                shell
                    .take_ready_follow_up()
                    .map(|input| input.transcript_text),
                dispatch.then(|| "queued first".to_owned())
            );
            assert!(shell.take_ready_follow_up().is_none());
            shell.edit_queued_message();
            assert_eq!(shell.pending(), "queued second edited");
        }
    }

    struct EndsThenPanics(bool);

    impl Stream for EndsThenPanics {
        type Item = std::io::Result<Event>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            assert!(!self.0, "a closed input stream was polled more than once");
            self.0 = true;
            std::task::Poll::Ready(None)
        }
    }

    #[tokio::test]
    async fn closed_input_is_disabled_while_the_aborted_run_settles() {
        let (_server, _workspace, mut agent) = scripted_agent().await;
        let mut shell = InteractiveShell::test_shell();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut input = EndsThenPanics(false);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut executable_extensions = crate::extensions::ExecutableExtensions::default();

        let mut goal_deadline = None;
        let ended = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut executable_extensions,
            &mut false,
            test_run_inspection(),
            &mut goal_deadline,
        )
        .await
        .unwrap();
        drop(run);

        assert_eq!(ended, HostRunOutcome::Aborted);
        assert!(quit);
        assert!(shell.debug_snapshot().contains("Interrupted"));
    }

    #[tokio::test]
    async fn changelog_startup_and_idle_skip_responses_context_prewarm() {
        use base64::Engine;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // A real loopback WebSocket records response.create bodies, including
        // generate=false. No provider credentials or external endpoint exist.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let (requests, mut received) = tokio::sync::mpsc::unbounded_channel();
        let mut server = tokio::task::JoinSet::new();
        server.spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
                assert!(head.len() < 16 * 1024);
            }
            let head = String::from_utf8(head).unwrap();
            assert!(head.starts_with("GET /v1/responses HTTP/1.1"));
            assert!(!head.to_ascii_lowercase().contains("authorization:"));
            let key = head.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("sec-websocket-key").then(|| value.trim())
            }).unwrap();
            let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
                format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
            let accept = base64::engine::general_purpose::STANDARD.encode(digest.as_ref());
            socket.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
            loop {
                let opcode = socket.read_u8().await.unwrap();
                assert_eq!(opcode, 0x81, "fixture expects one complete JSON text frame");
                let flags = socket.read_u8().await.unwrap();
                assert_ne!(flags & 0x80, 0, "client frames must be masked");
                let length = match flags & 0x7f {
                    126 => u64::from(socket.read_u16().await.unwrap()),
                    127 => socket.read_u64().await.unwrap(),
                    length => u64::from(length),
                };
                assert!(length <= 128 * 1024);
                let mut mask = [0; 4];
                socket.read_exact(&mut mask).await.unwrap();
                let mut body = vec![0; length as usize];
                socket.read_exact(&mut body).await.unwrap();
                for (index, byte) in body.iter_mut().enumerate() { *byte ^= mask[index % 4]; }
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["type"], "response.create");
                assert_eq!(body["generate"], false);
                let completed = br#"{"type":"response.completed","response":{"id":"fixture-prewarm"}}"#;
                socket.write_all(&[0x81, completed.len() as u8]).await.unwrap();
                socket.write_all(completed).await.unwrap();
                requests.send(body).unwrap();
            }
        });
        let mut model = scripted_model(&uri);
        Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
        Arc::make_mut(&mut model.endpoint).transport =
            octet_ai::EndpointTransport::WebSocketPreferred;
        let (workspace, agent) = scripted_agent_for_route(model.clone(), octet_ai::AiClient::new());
        let (_app_workspace, mut app) = crate::compaction::tests::app_for_estimate();
        app.agent = agent;
        app.model = model;
        let path = workspace.path().join("session.jsonl");
        let before = std::fs::read(&path).unwrap();
        let mut shell = InteractiveShell::test_shell();
        for prompt in ["/changelog", " /chang  "] {
            assert!(prepare_startup_input(&app, &mut shell, Some(prompt.into())).is_none());
            assert!(shell.has_overlay());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            received.try_recv().is_err(),
            "release-note startup sent context"
        );

        // An ordinary startup must still prewarm. This positive control also
        // proves the synthetic model can exercise the transport under test.
        assert!(prepare_startup_input(&app, &mut shell, None).is_none());
        tokio::time::timeout(Duration::from_secs(3), received.recv())
            .await
            .unwrap()
            .unwrap();
        for command in ["/changelog", "/chang"] {
            schedule_idle_responses_prewarm(&app, &commands::parse(command));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            received.try_recv().is_err(),
            "idle release notes sent context"
        );
        schedule_idle_responses_prewarm(&app, &Command::Status);
        tokio::time::timeout(Duration::from_secs(3), received.recv())
            .await
            .unwrap()
            .unwrap();
        // This is not a general startup slash-command dispatcher. Unknown
        // arguments, ordinary text, and explicit templates remain prompts.
        for (template, prompt) in [
            (None, "/changelog extra"),
            (None, "/status"),
            (None, "Explain the changelog"),
            (Some("fixture"), "/changelog"),
            (Some("fixture"), "Expanded template argument: /changelog"),
        ] {
            app.config.prompt_template = template.map(str::to_owned);
            let input = prepare_startup_input(&app, &mut shell, Some(prompt.into())).unwrap();
            assert_eq!(input.display_text, prompt);
            tokio::time::timeout(Duration::from_secs(3), received.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        server.abort_all();
    }

    #[tokio::test]
    async fn scripted_active_changelog_keeps_running_without_provider_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;
        let (_server, _workspace, mut agent) = scripted_agent().await;
        let mut shell = InteractiveShell::test_shell();
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        for character in "/changelog".chars() {
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
        }
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut goal_deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut extensions,
                &mut false,
                test_run_inspection(),
                &mut goal_deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        drop(run);
        assert_eq!(ended, HostRunOutcome::Completed);
        assert!(
            shell.has_overlay(),
            "the report stays open while the run settles"
        );
        assert!(pending.is_empty());
        assert!(!quit);
        assert!(!format!("{:?}", agent.session().context().unwrap()).contains("/changelog"));
    }

    #[tokio::test]
    async fn active_skill_invocations_queue_as_prompts_instead_of_unknown_commands() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (_server, _workspace, mut agent) = scripted_agent().await;
        let mut shell = InteractiveShell::test_shell();
        shell.set_skill_commands(Arc::from([("skill:review".into(), "Review".into())]));
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        for invocation in ["/skill:review inspect", "/skill:rev"] {
            for character in invocation.chars() {
                sender
                    .send(Ok(Event::Key(KeyEvent::new(
                        KeyCode::Char(character),
                        KeyModifiers::NONE,
                    ))))
                    .await
                    .unwrap();
            }
            sender
                .send(Ok(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))))
                .await
                .unwrap();
        }
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut goal_deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut extensions,
                &mut false,
                test_run_inspection(),
                &mut goal_deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        drop(run);
        assert_eq!(ended, HostRunOutcome::Completed);
        assert!(!quit);
        assert!(pending.is_empty());
        assert_eq!(shell.queued_follow_up_len(), 2);
        assert_eq!(
            shell.take_ready_follow_up().unwrap().transcript_text,
            "/skill:review inspect"
        );
        shell.settle_queued_follow_ups(true);
        assert_eq!(
            shell.take_ready_follow_up().unwrap().transcript_text,
            "/skill:review "
        );
        assert!(!format!("{:?}", agent.session().context().unwrap()).contains("/skill:"));
    }

    #[tokio::test]
    async fn scripted_active_loop_queues_controls_and_never_forwards_active_model_command() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (_server, workspace, mut agent) = scripted_agent().await;
        let image = workspace.path().join("shot.png");
        std::fs::write(
            &image,
            include_bytes!("../../tests/fixtures/export_html/one-pixel.png"),
        )
        .unwrap();

        let mut shell = InteractiveShell::test_shell();
        shell.set_input_modalities(octet_ai::ModalitySet::none().with(octet_ai::Modality::Image));
        for character in "steer first".chars() {
            shell.apply_edit(crate::tui::keymap::EditAction::Char(character));
        }
        shell.apply_edit(crate::tui::keymap::EditAction::Paste(
            image.display().to_string(),
        ));
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        for character in "/model gpt-4o-mini".chars() {
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
        }
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        // Keep the sender alive so the receiver remains pending rather than
        // signalling an input close that would abort the real run.
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut goal_deadline = None;
        let ended = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut executable_extensions,
            &mut false,
            test_run_inspection(),
            &mut goal_deadline,
        )
        .await
        .unwrap();
        drop(run);

        assert_eq!(ended, HostRunOutcome::Completed);
        assert!(!quit);
        assert_eq!(
            pending.pop_front(),
            Some(PendingIdleAction::ChangeModel(ModelId(
                "gpt-4o-mini".into()
            )))
        );
        let context = agent.session().context().unwrap();
        let user_text = context
            .iter()
            .filter_map(|message| match message {
                octet_ai::Message::User(user) => user.content.iter().find_map(|part| match part {
                    octet_ai::UserPart::Text(text) => Some(text.as_str()),
                    _ => None,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(user_text.contains(&"steer first"));
        assert!(!user_text.iter().any(|text| text.contains("/model")));
        assert!(context.iter().any(|message| matches!(
            message,
            octet_ai::Message::User(user)
                if user
                    .content
                    .iter()
                    .any(|part| matches!(part, octet_ai::UserPart::Media(_)))
        )));
    }

    #[tokio::test]
    async fn abort_restores_all_undelivered_steering_after_the_final_event() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_secs(2)).await;
        let mut shell = InteractiveShell::test_shell();
        for character in "steer first".chars() {
            shell.apply_edit(crate::tui::keymap::EditAction::Char(character));
        }
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        for character in "steer second".chars() {
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
        }
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        let _sender = sender;

        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut goal_deadline = None;
        let ended = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut executable_extensions,
            &mut false,
            test_run_inspection(),
            &mut goal_deadline,
        )
        .await
        .unwrap();
        drop(run);

        assert_eq!(ended, HostRunOutcome::Aborted);
        assert_eq!(shell.pending(), "steer first\n\nsteer second");
        assert!(shell.debug_snapshot().contains("Interrupted"));
        let context = agent.session().context().unwrap();
        assert!(!context.iter().any(|message| matches!(
            message,
            octet_ai::Message::User(user)
                if user.content.iter().any(|part| matches!(
                    part,
                    octet_ai::UserPart::Text(text) if text.starts_with("steer ")
                ))
        )));
    }

    /// Drive one scripted run with a fixed event script and return its settled
    /// outcome. The sender stays alive so the input stream remains pending
    /// rather than signalling a close that would abort the run.
    async fn drive_scripted_events(
        agent: &mut octet_agent::Agent,
        shell: &mut InteractiveShell,
        events: Vec<Event>,
    ) -> (HostRunOutcome, bool) {
        use tokio_stream::wrappers::ReceiverStream;
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        for event in events {
            sender.send(Ok(event)).await.unwrap();
        }
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut goal_deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            drive_active_run(
                &mut run,
                &control,
                shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut executable_extensions,
                &mut false,
                test_run_inspection(),
                &mut goal_deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        drop(run);
        (ended, quit)
    }

    /// Every durable user-message text in the session, in order.
    fn delivered_user_text(agent: &octet_agent::Agent) -> Vec<String> {
        agent
            .session()
            .context()
            .unwrap()
            .iter()
            .filter_map(|message| match message {
                octet_ai::Message::User(user) => Some(
                    user.content
                        .iter()
                        .filter_map(|part| match part {
                            octet_ai::UserPart::Text(text) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn full_steering_admission_restores_the_draft_without_a_shell_entry() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_secs(2)).await;
        let mut shell = InteractiveShell::test_shell();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let reservations = (0..64)
            .map(|_| control.prepare_steer("occupied").unwrap().0)
            .collect::<Vec<_>>();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender.send(Ok(Event::Paste("refused steering".into()))).await.unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))))
            .await
            .unwrap();
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut goal_deadline = None;

        let ended = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut extensions,
            &mut false,
            test_run_inspection(),
            &mut goal_deadline,
        )
        .await
        .unwrap();
        drop(reservations);
        drop(run);

        assert_eq!(ended, HostRunOutcome::Aborted);
        assert_eq!(shell.pending(), "refused steering");
        assert!(!shell.debug_snapshot().contains("Steering:"));
        assert!(
            !delivered_user_text(&agent)
                .iter()
                .any(|text| text.contains("refused steering")),
        );
    }

    #[tokio::test]
    async fn recalled_live_steering_returns_to_the_editor_without_delivery() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_secs(2)).await;
        let mut shell = InteractiveShell::test_shell();
        let (ended, quit) = drive_scripted_events(
            &mut agent,
            &mut shell,
            vec![
                Event::Paste("steer recalled".into()),
                Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                // Option/Alt+Up recalls the newest retractable live steering
                // before the provider boundary can claim it.
                Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)),
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ],
        )
        .await;
        assert_eq!(ended, HostRunOutcome::Aborted);
        assert!(!quit);
        assert_eq!(shell.pending(), "steer recalled");
        assert!(!shell.debug_snapshot().contains("Steering:"));
        assert!(
            !delivered_user_text(&agent)
                .iter()
                .any(|text| text.contains("steer recalled")),
            "a recalled submission must not also be delivered"
        );
    }

    #[tokio::test]
    async fn requeued_edited_live_steering_is_what_the_next_provider_request_carries() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(200)).await;
        let mut shell = InteractiveShell::test_shell();
        let (ended, quit) = drive_scripted_events(
            &mut agent,
            &mut shell,
            vec![
                Event::Paste("original".into()),
                Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)),
                Event::Paste(" edited".into()),
                Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            ],
        )
        .await;
        assert_eq!(ended, HostRunOutcome::Completed);
        assert!(!quit);
        let delivered = delivered_user_text(&agent);
        assert!(
            delivered.iter().any(|text| text == "original edited"),
            "requeued edited steering is delivered: {delivered:?}"
        );
        assert!(
            !delivered.iter().any(|text| text == "original"),
            "the recalled draft is not delivered twice: {delivered:?}"
        );
        assert!(shell.pending().is_empty());
    }

    #[tokio::test]
    async fn recalled_live_steering_is_not_double_counted_in_the_editor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_secs(2)).await;
        let mut shell = InteractiveShell::test_shell();
        let (ended, _quit) = drive_scripted_events(
            &mut agent,
            &mut shell,
            vec![
                Event::Paste("alpha".into()),
                Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                Event::Paste("beta".into()),
                Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)),
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ],
        )
        .await;
        assert_eq!(ended, HostRunOutcome::Aborted);
        let pending = shell.pending();
        // The recalled entry is restored once and the still-queued entry is
        // restored once, with no duplicate append from either owner.
        assert_eq!(pending, "alpha\n\nbeta");
        assert_eq!(pending.matches("alpha").count(), 1);
        assert_eq!(pending.matches("beta").count(), 1);
        assert!(
            !delivered_user_text(&agent)
                .iter()
                .any(|text| text.contains("alpha") || text.contains("beta")),
            "recalled and aborted steering must not be delivered"
        );
    }

    #[test]
    fn sticky_answer_steering_is_not_retractable_while_live_steering_is() {
        let mut shell = InteractiveShell::test_shell();
        let answer = answer_now_input(Some("keep tools off".into()));
        shell.queue_steering(&answer);
        let (live, receipt) = octet_agent::PreparedSteering::new("live steer");
        shell.queue_retractable_steering(
            receipt,
            "live steer".into(),
            "live steer".into(),
            Vec::new(),
        );
        shell.edit_queued_message();
        // Joint recall takes the newest retractable steering entry only.
        assert_eq!(shell.pending(), "live steer");
        assert!(
            shell.debug_snapshot().contains("Steering: /answer keep tools off"),
            "sticky /answer stays queued: {}",
            shell.debug_snapshot()
        );
        // The recalled live submission is a no-op rather than a second append.
        drop(live);
    }

    #[tokio::test]
    async fn active_run_subagents_without_extension_owner_stays_an_unknown_command() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (_server, _workspace, mut agent) = scripted_agent().await;
        let mut shell = InteractiveShell::test_shell();
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        for character in "/subagents".chars() {
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
        }
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        // Keep the sender alive so the receiver remains pending rather than
        // signalling an input close that would abort the real run.
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut goal_deadline = None;
        let ended = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut executable_extensions,
            &mut false,
            test_run_inspection(),
            &mut goal_deadline,
        )
        .await
        .unwrap();
        drop(run);

        assert_eq!(ended, HostRunOutcome::Completed);
        assert_eq!(
            shell.debug_error().as_deref(),
            Some("unknown command: /subagents"),
            "without a octet-subagents owner the command must not open the live view"
        );
    }

    // -----------------------------------------------------------------------
    // `/goal` applies immediately, mid-run or idle; it is never queued.
    // -----------------------------------------------------------------------

    use crate::commands::GoalCommand;

    /// A real durable goal store, its driver, and an inspection that addresses
    /// it exactly as `App` does: same store, same shared driver state, same
    /// session key. The caller keeps the store and driver to read back the
    /// mutation and to prove driver coherence.
    fn goal_fixture(
        dir: &Path,
        session_id: &str,
    ) -> (
        Arc<octet_agent::DurableGoalStore>,
        octet_agent::GoalDriver,
        ActiveRunInspection,
    ) {
        let store =
            Arc::new(octet_agent::DurableGoalStore::open(dir).expect("durable goal store fixture"));
        let driver = octet_agent::GoalDriver::new(store.clone(), session_id);
        let inspection =
            test_run_inspection_with_goal(dir, store.clone(), driver.clone(), session_id);
        (store, driver, inspection)
    }

    #[test]
    fn goal_commands_have_no_queued_form() {
        let mut queue = VecDeque::new();
        for command in [
            GoalCommand::Help,
            GoalCommand::Status,
            GoalCommand::Set("an objective".into()),
            GoalCommand::Pause,
            GoalCommand::Resume,
            GoalCommand::Clear,
        ] {
            let error = queue_command(Command::Goal(command), &mut queue)
                .expect_err("goal commands are never queued");
            assert!(
                error.to_string().contains("never queued"),
                "the refusal names the defect: {error}"
            );
        }
        assert!(queue.is_empty(), "no goal action was deferred: {queue:?}");
        assert!(matches!(
            commands::parse("/goal close the parity row"),
            Command::Goal(GoalCommand::Set(objective)) if objective == "close the parity row"
        ));
    }

    #[tokio::test]
    async fn active_goal_objective_mutates_the_durable_store_without_queueing() {
        let dir = tempfile::tempdir().unwrap();
        let (store, driver, inspection) = goal_fixture(dir.path(), "goal-session");
        let mut shell = InteractiveShell::test_shell();
        let (queue, quit, deadline) = run_active_command_observing_deadline(
            &mut shell,
            Command::Goal(GoalCommand::Set("close the goal-parity row".into())),
            &inspection,
        )
        .await;
        assert!(queue.is_empty(), "mid-run /goal must not queue: {queue:?}");
        assert!(!quit);
        let goal = store
            .get("goal-session")
            .expect("the fixture store stays readable")
            .expect("the durable goal exists immediately");
        assert_eq!(goal.objective, "close the goal-parity row");
        assert_eq!(goal.status, octet_agent::GoalStatus::Active);
        assert_eq!(goal.turns_used, 0, "setting an objective reserves no turn");
        assert!(
            deadline.is_some(),
            "an active objective arms the same deadline an idle /goal arms"
        );
        let painted = shell.debug_snapshot();
        assert!(painted.contains("goal set"), "{painted}");
        // Driver coherence: the shared driver waits on the new objective exactly
        // as it would after an idle `/goal`, and nothing has been reserved.
        assert!(matches!(
            driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
            Ok(octet_agent::GoalDecision::Wait { .. })
        ));
        assert!(goal.turns_used == 0);
    }

    #[tokio::test]
    async fn active_goal_commands_apply_while_the_run_is_still_streaming() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio_stream::wrappers::ReceiverStream;

        let (server, mut started, release) = HeldApi::start(text_turn()).await;
        let (_workspace, mut agent) =
            scripted_agent_for_route(scripted_model(&server.uri), octet_ai::AiClient::new());
        let dir = tempfile::tempdir().unwrap();
        let (store, _driver, inspection) = goal_fixture(dir.path(), "goal-session");
        store
            .set("goal-session", "keep the stream honest", None)
            .expect("durable goal fixture");

        let mut shell = InteractiveShell::test_shell();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        // The objective change is typed first: it mutates no overlay, so the
        // status report typed second still receives its Enter.
        for command in ["/goal set streamed objective", "/goal status"] {
            for character in command.chars() {
                sender
                    .send(Ok(Event::Key(KeyEvent::new(
                        KeyCode::Char(character),
                        KeyModifiers::NONE,
                    ))))
                    .await
                    .unwrap();
            }
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
        }
        // Keep the sender alive so EOF cannot abort the held run.
        let _sender = sender;
        let mut input = ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut made_tool_call = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut goal_deadline = None;
        // The run borrows the shell, its queue, and the deadline for the whole
        // held turn, so the mid-run observations happen inside this block and
        // every post-settlement assertion happens after its borrows end.
        let ended = {
            let drive = drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut quit,
                None,
                None,
                &mut extensions,
                &mut made_tool_call,
                &inspection,
                &mut goal_deadline,
            );
            tokio::pin!(drive);
            tokio::select! {
                result = &mut drive => {
                    panic!("the held run settled before the provider request started: {result:?}")
                }
                result = &mut started => result.expect("the held provider request starts"),
            }

            // The provider response is still withheld, so a store mutation
            // observed here was applied to the durable goal mid-run, while the
            // stream was live.
            let budget = Instant::now() + Duration::from_secs(2);
            while store.get("goal-session").unwrap().unwrap().objective != "streamed objective" {
                assert!(
                    Instant::now() < budget,
                    "the mid-run objective was never applied while kept alive"
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(10), &mut drive)
                        .await
                        .is_err(),
                    "the run settled before the mid-run /goal was applied"
                );
            }
            let streamed = store.get("goal-session").unwrap().unwrap();
            assert_eq!(streamed.objective, "streamed objective");
            assert_eq!(streamed.status, octet_agent::GoalStatus::Active);
            assert_eq!(
                streamed.turns_used, 0,
                "the mid-run objective reserved no continuation turn"
            );

            release.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(5), &mut drive)
                .await
                .expect("the released run settles")
                .unwrap()
        };
        assert_eq!(ended, HostRunOutcome::Completed);
        drop(run);
        assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            goal_deadline.is_some(),
            "the mid-run objective armed the continuation deadline"
        );
        assert!(
            pending.is_empty(),
            "neither goal command was queued to the idle boundary: {pending:?}"
        );
        assert!(!quit);
        assert!(
            shell.has_overlay(),
            "the /goal status report opened mid-run and stayed open"
        );
        let painted = shell.debug_snapshot();
        assert!(painted.contains("goal set"), "{painted}");
        assert!(
            painted.contains("streamed objective"),
            "the notice names the objective applied mid-run: {painted}"
        );
    }

    #[tokio::test]
    async fn active_goal_status_reports_immediately_without_queueing() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _driver, inspection) = goal_fixture(dir.path(), "goal-session");
        store
            .set("goal-session", "keep the stream honest", None)
            .expect("durable goal fixture");
        let mut shell = InteractiveShell::test_shell();
        let (queue, quit, deadline) = run_active_command_observing_deadline(
            &mut shell,
            Command::Goal(GoalCommand::Status),
            &inspection,
        )
        .await;
        assert!(queue.is_empty(), "status is never queued: {queue:?}");
        assert!(!quit);
        assert!(deadline.is_none(), "status arms no deadline");
        assert!(
            shell.has_overlay(),
            "the active-run status reports in the frame immediately"
        );
        let status = inspection
            .goal_access()
            .expect("addressable fixture")
            .status_text()
            .expect("the report reads the durable store");
        assert!(
            status.contains("Active goal: keep the stream honest"),
            "the open report carries the durable goal: {status}"
        );
        assert_eq!(
            store.get("goal-session").unwrap().unwrap().objective,
            "keep the stream honest",
            "status does not mutate the goal"
        );
    }

    #[tokio::test]
    async fn active_goal_pause_resume_and_clear_are_coherent_mid_run() {
        let dir = tempfile::tempdir().unwrap();
        let (store, driver, inspection) = goal_fixture(dir.path(), "goal-session");
        store
            .set("goal-session", "stay coherent", None)
            .expect("durable goal fixture");
        let mut shell = InteractiveShell::test_shell();

        let (queue, _, paused_deadline) = run_active_command_observing_deadline(
            &mut shell,
            Command::Goal(GoalCommand::Pause),
            &inspection,
        )
        .await;
        assert!(queue.is_empty(), "pause is never queued: {queue:?}");
        assert_eq!(
            store.get("goal-session").unwrap().unwrap().status,
            octet_agent::GoalStatus::Paused
        );
        assert!(
            paused_deadline.is_none(),
            "a paused goal disarms the continuation deadline"
        );
        assert_eq!(
            driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
            Ok(octet_agent::GoalDecision::Paused),
            "a paused goal cannot continue the run that paused it"
        );
        assert!(
            driver.fire_continuation().unwrap().is_none(),
            "a paused goal reserves no continuation turn"
        );

        let (queue, _, resumed_deadline) = run_active_command_observing_deadline(
            &mut shell,
            Command::Goal(GoalCommand::Resume),
            &inspection,
        )
        .await;
        assert!(queue.is_empty(), "resume is never queued: {queue:?}");
        let resumed = store.get("goal-session").unwrap().unwrap();
        assert_eq!(resumed.status, octet_agent::GoalStatus::Active);
        assert_eq!(
            resumed.objective, "stay coherent",
            "resume keeps the objective"
        );
        assert!(resumed_deadline.is_some(), "resume re-arms the deadline");
        assert!(matches!(
            driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
            Ok(octet_agent::GoalDecision::Wait { .. })
        ));

        let (queue, _, cleared_deadline) = run_active_command_observing_deadline(
            &mut shell,
            Command::Goal(GoalCommand::Clear),
            &inspection,
        )
        .await;
        assert!(queue.is_empty(), "clear is never queued: {queue:?}");
        assert!(store.get("goal-session").unwrap().is_none());
        assert!(
            cleared_deadline.is_none(),
            "a cleared goal disarms the continuation deadline"
        );
        assert_eq!(
            driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
            Ok(octet_agent::GoalDecision::Inactive)
        );
        assert!(
            driver.fire_continuation().unwrap().is_none(),
            "a cleared goal reserves no continuation turn"
        );
        let painted = shell.debug_snapshot();
        for expected in ["goal paused", "goal resumed", "goal cleared"] {
            assert!(
                painted.contains(expected),
                "{expected:?} missing: {painted}"
            );
        }
        assert_eq!(paused_deadline.is_none(), true);
    }

    #[tokio::test]
    async fn unaddressable_active_goal_fails_closed_and_is_never_queued() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Arc::new(octet_agent::DurableGoalStore::open(dir.path()).expect("store fixture"));
        let driver = octet_agent::GoalDriver::new(store.clone(), "goal-session");
        assert!(matches!(
            GoalAccess::from_parts(store, driver, String::new()),
            Err(ActiveGoalError::UnaddressableSession)
        ));
        let mut shell = InteractiveShell::test_shell();
        for command in [
            GoalCommand::Status,
            GoalCommand::Set("never queued".into()),
            GoalCommand::Pause,
            GoalCommand::Resume,
            GoalCommand::Clear,
        ] {
            let (queue, quit, deadline) = run_active_command_observing_deadline(
                &mut shell,
                Command::Goal(command),
                test_run_inspection(),
            )
            .await;
            assert!(queue.is_empty(), "the refusal is never queued: {queue:?}");
            assert!(!quit);
            assert!(deadline.is_none(), "no deadline can be armed");
            let error = shell
                .debug_error()
                .expect("the typed reason is rendered, not a silent no-op");
            assert!(error.starts_with("/goal failed:"), "{error}");
            assert!(error.contains("no durable goal identity"), "{error}");
        }
    }

    #[tokio::test]
    async fn a_reachable_goal_store_that_rejects_still_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _driver, inspection) = goal_fixture(dir.path(), "goal-session");
        let mut shell = InteractiveShell::test_shell();
        let oversized = "x".repeat(octet_agent::MAX_GOAL_OBJECTIVE_BYTES + 1);
        let (queue, _, deadline) = run_active_command_observing_deadline(
            &mut shell,
            Command::Goal(GoalCommand::Set(oversized)),
            &inspection,
        )
        .await;
        assert!(
            queue.is_empty(),
            "a rejected goal is never queued: {queue:?}"
        );
        assert!(deadline.is_none(), "a rejected goal arms no deadline");
        assert!(
            store.get("goal-session").unwrap().is_none(),
            "the store rejected the objective and stayed empty"
        );
        let error = shell.debug_error().expect("the store error is rendered");
        assert!(error.starts_with("unable to set goal:"), "{error}");
        assert!(error.contains("invalid goal objective"), "{error}");
    }

    /// `/goal` at idle applies the same durable mutations through the same
    /// producer the idle dispatcher's arm calls, on a real bootstrap-built App
    /// with a real durable store and session key.
    #[tokio::test]
    async fn idle_goal_commands_still_apply_the_same_durable_mutations() {
        let (_workspace, app) = crate::compaction::tests::app_for_estimate();
        let store = app.goal_store.clone();
        let session_id = app.goal_session_id.clone();
        assert!(
            !session_id.is_empty(),
            "the idle fixture carries a durable goal identity"
        );
        let mut shell = InteractiveShell::test_shell();
        let mut goal_deadline = None;

        apply_idle_goal_command(
            &app,
            &mut shell,
            GoalCommand::Set("idle objective".into()),
            &mut goal_deadline,
        )
        .expect("idle /goal set");
        let goal = store
            .get(&session_id)
            .unwrap()
            .expect("idle /goal writes the durable store");
        assert_eq!(goal.objective, "idle objective");
        assert_eq!(goal.status, octet_agent::GoalStatus::Active);
        assert!(goal_deadline.is_some(), "idle /goal arms the deadline");
        assert!(shell.debug_snapshot().contains("goal set"));

        apply_idle_goal_command(&app, &mut shell, GoalCommand::Status, &mut goal_deadline)
            .expect("idle /goal status");
        assert!(
            shell.has_overlay(),
            "idle /goal status opens the same report"
        );
        assert!(store.get(&session_id).unwrap().is_some());

        apply_idle_goal_command(&app, &mut shell, GoalCommand::Pause, &mut goal_deadline)
            .expect("idle /goal pause");
        assert_eq!(
            store.get(&session_id).unwrap().unwrap().status,
            octet_agent::GoalStatus::Paused
        );
        assert!(goal_deadline.is_none());

        apply_idle_goal_command(&app, &mut shell, GoalCommand::Resume, &mut goal_deadline)
            .expect("idle /goal resume");
        assert_eq!(
            store.get(&session_id).unwrap().unwrap().status,
            octet_agent::GoalStatus::Active
        );
        assert!(goal_deadline.is_some());

        apply_idle_goal_command(&app, &mut shell, GoalCommand::Clear, &mut goal_deadline)
            .expect("idle /goal clear");
        assert!(store.get(&session_id).unwrap().is_none());
        assert!(goal_deadline.is_none());
    }

    #[derive(Default)]
    struct RecordingLifecycle {
        events: std::cell::RefCell<Vec<String>>,
    }

    impl RecordingLifecycle {
        fn events(&self) -> Vec<String> {
            self.events.borrow().clone()
        }
    }

    impl MessageLifecycleSink for RecordingLifecycle {
        fn message_started(&mut self, message_id: &str) {
            self.events
                .borrow_mut()
                .push(format!("started:{message_id}"));
        }

        fn message_delta(&mut self, delta: &str) {
            self.events.borrow_mut().push(format!("delta:{delta}"));
        }

        fn message_settled(&mut self, message_id: &str) {
            self.events
                .borrow_mut()
                .push(format!("settled:{message_id}"));
        }
    }

    impl DialogLifecycleSink for RecordingLifecycle {
        fn open_dialog(&self, dialog: &str) {
            self.events.borrow_mut().push(format!("started:{dialog}"));
        }

        fn close_dialog(&self, dialog: &str) {
            self.events.borrow_mut().push(format!("settled:{dialog}"));
        }
    }

    fn text_delta(text: &str) -> AgentEvent {
        AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: text.to_owned(),
        }
    }

    fn completed_run() -> AgentEvent {
        AgentEvent::RunFinished {
            head: EntryId("entry-1".to_owned()),
            reason: octet_agent::FinishReason::Completed,
        }
    }

    #[test]
    fn assistant_message_lifecycle_brackets_one_message_per_text_turn() {
        let mut lifecycle = AssistantMessageLifecycle::default();
        let mut sink = RecordingLifecycle::default();
        lifecycle.observe(&mut sink, &text_delta(""));
        assert!(sink.events().is_empty(), "an empty delta opens nothing");
        lifecycle.observe(&mut sink, &text_delta("Hel"));
        lifecycle.observe(
            &mut sink,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Reasoning,
                text: "hidden reasoning".to_owned(),
            },
        );
        lifecycle.observe(&mut sink, &text_delta("lo"));
        assert_eq!(
            sink.events(),
            ["started:assistant-1", "delta:Hel", "delta:lo"]
        );
        // Every terminal run outcome settles the open boundary exactly once.
        lifecycle.observe(&mut sink, &completed_run());
        lifecycle.observe(&mut sink, &completed_run());
        assert_eq!(sink.events().len(), 4);
        assert_eq!(
            sink.events().last().map(String::as_str),
            Some("settled:assistant-1")
        );
        // A later turn opens a fresh, still bounded identifier.
        lifecycle.observe(&mut sink, &text_delta("again"));
        assert_eq!(
            sink.events().last().map(String::as_str),
            Some("delta:again")
        );
        assert!(sink.events().contains(&"started:assistant-2".to_owned()));
    }

    #[test]
    fn assistant_message_lifecycle_keeps_one_boundary_across_provider_retry() {
        let mut lifecycle = AssistantMessageLifecycle::default();
        let mut sink = RecordingLifecycle::default();
        lifecycle.observe(&mut sink, &text_delta("partial"));
        lifecycle.observe(
            &mut sink,
            &AgentEvent::ProviderRetry {
                attempt: 1,
                max_attempts: 3,
                delay: Duration::from_millis(1),
                error: "stream reset".to_owned(),
            },
        );
        lifecycle.observe(&mut sink, &text_delta("final"));
        assert_eq!(
            sink.events(),
            ["started:assistant-1", "delta:partial", "delta:final"]
        );
        lifecycle.settle(&mut sink);
        assert_eq!(
            sink.events().last().map(String::as_str),
            Some("settled:assistant-1")
        );
    }

    #[test]
    fn assistant_message_lifecycle_forwards_each_delta_verbatim() {
        let mut lifecycle = AssistantMessageLifecycle::default();
        let mut sink = RecordingLifecycle::default();
        for index in 0..64 {
            lifecycle.observe(&mut sink, &text_delta(&format!("t{index}")));
        }
        let events = sink.events();
        assert_eq!(events[0], "started:assistant-1");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("delta:"))
                .count(),
            64,
            "the producer forwards every increment; the host owns coalescing"
        );
        assert!(
            !events.iter().any(|event| event.starts_with("settled:")),
            "no notification per delta is issued before the terminal boundary"
        );
    }

    #[tokio::test]
    async fn present_host_dialog_settles_once_on_every_outcome() {
        let sink = RecordingLifecycle::default();
        let value = present_host_dialog(&sink, "confirm", async { Ok::<_, anyhow::Error>(true) })
            .await
            .expect("approved dialog");
        assert!(value);
        assert_eq!(sink.events(), ["started:confirm", "settled:confirm"]);
        let refused = present_host_dialog(&sink, "input", async {
            Err::<Option<String>, _>(anyhow::anyhow!("dismissed"))
        })
        .await;
        assert!(refused.is_err());
        assert_eq!(
            sink.events(),
            [
                "started:confirm",
                "settled:confirm",
                "started:input",
                "settled:input",
            ]
        );
    }
}
