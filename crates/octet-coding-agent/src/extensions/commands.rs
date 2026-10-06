//! ExecutableExtensions commands, menus, shortcuts and presentation actions.

use super::*;
use octet_agent::extension_process::{
    CommandOutput, ExtensionOperationToken, ExtensionRuntimeError,
};

impl ExecutableExtensions {
    /// Resolve a host-validated extension shortcut from one terminal event.
    /// Static manifest contributions win over runtime registrations.
    pub(super) fn shortcut_for_event(&self, event: &Event) -> Option<ExtensionShortcutInvocation> {
        let key = extension_shortcut_key(event)?;
        if let Some(shortcut) = self.shortcuts.iter().find(|shortcut| shortcut.key == key) {
            return Some(shortcut.invocation.clone());
        }
        let dynamic = self
            .dynamic_shortcuts
            .iter()
            .find(|shortcut| shortcut.key == key)?;
        Some(ExtensionShortcutInvocation {
            extension: dynamic.extension.clone(),
            name: dynamic.shortcut_id.clone(),
            description: dynamic.description.clone(),
        })
    }

    /// Schedule a shortcut without blocking terminal input. Child confirmation
    /// and input requests are denied because a background invocation cannot
    /// safely take ownership of the frontend confirmation surface.
    pub(crate) fn dispatch_shortcut_for_event(
        &mut self,
        event: &Event,
    ) -> Option<ExtensionShortcutInvocation> {
        let invocation = self.shortcut_for_event(event)?;
        if let Some(index) = self.dynamic_shortcuts.iter().position(|dynamic| {
            dynamic.extension == invocation.extension && dynamic.shortcut_id == invocation.name
        }) {
            let dynamic = self.dynamic_shortcuts[index].clone();
            if !dynamic.process.is_running()
                || dynamic.process.health_snapshot().generation != dynamic.generation
            {
                self.dynamic_shortcuts.remove(index);
                self.diagnostics.push(format!(
                    "warning: extension shortcut {:?}/{} was dropped with its process generation",
                    dynamic.extension, dynamic.shortcut_id
                ));
                return None;
            }
            if let Err(error) = dynamic
                .process
                .notify_shortcut_trigger(&dynamic.shortcut_id)
            {
                self.diagnostics.push(format!(
                    "warning: extension shortcut {:?}/{} could not be delivered: {error}",
                    dynamic.extension, dynamic.shortcut_id
                ));
                return None;
            }
            return Some(invocation);
        }
        let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == invocation.extension
                    && process
                        .contributions()
                        .shortcuts
                        .iter()
                        .any(|shortcut| shortcut.name == invocation.name)
            })
            .cloned()
        else {
            self.diagnostics.push(format!(
                "warning: extension shortcut {:?}/{} is no longer available",
                invocation.extension, invocation.name
            ));
            return None;
        };
        if Handle::try_current().is_err() {
            self.diagnostics.push(format!(
                "warning: extension shortcut {:?}/{} could not start outside the octet Tokio runtime",
                invocation.extension, invocation.name
            ));
            return None;
        }
        self.shortcut_tasks.retain(|task| !task.is_finished());
        if self.shortcut_tasks.len() >= SHORTCUT_TASK_CONCURRENCY {
            self.diagnostics.push(format!(
                "warning: extension shortcut {:?}/{} was not started: {SHORTCUT_TASK_CONCURRENCY} invocations are already running",
                invocation.extension, invocation.name
            ));
            return None;
        }
        let sender = self.background_tx.clone();
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        let started = invocation.clone();
        self.shortcut_tasks.push(tokio::spawn(async move {
            let (extension, context, messages) =
                execute_shortcut_headless(process, invocation.name, context).await;
            let _ = sender
                .send(ExtensionBackgroundUpdate::Shortcut {
                    extension,
                    context,
                    messages,
                })
                .await;
        }));
        Some(started)
    }

    pub fn command_suggestions(&self) -> Vec<(String, String)> {
        self.command_suggestions_with_usage()
            .into_iter()
            .map(|(name, description, _)| (name, description))
            .collect()
    }

    /// Returns the executable command metadata needed by non-TUI discovery surfaces.
    pub fn command_suggestions_with_usage(&self) -> Vec<(String, String, Option<String>)> {
        self.command_registry()
            .into_iter()
            .map(|entry| (entry.public_name, entry.description, entry.usage))
            .collect()
    }

    /// The one slash-command registry. Native and product commands keep their
    /// own precedence; every executable extension registration lands here, and
    /// a name claimed twice is namespaced on the later owner rather than
    /// silently shadowed by registration order.
    pub(crate) fn command_registry(&self) -> Vec<RegisteredExtensionCommand> {
        let mut registry: Vec<RegisteredExtensionCommand> = Vec::new();
        for process in &self.processes {
            let owner = process.descriptor().manifest.name.clone();
            for command in &process.contributions().commands {
                let mut public_name = command.name.clone();
                if crate::commands::slash_commands()
                    .iter()
                    .any(|entry| entry.name == public_name)
                    || public_name == "debug"
                    || registry
                        .iter()
                        .any(|entry| entry.public_name == public_name)
                {
                    public_name = format!("{owner}:{}", command.name);
                    // A second extension of the same manifest identity cannot
                    // be told apart by a name; keep the first registration.
                    if registry
                        .iter()
                        .any(|entry| entry.public_name == public_name)
                    {
                        continue;
                    }
                }
                registry.push(RegisteredExtensionCommand {
                    public_name,
                    owner: owner.clone(),
                    registered_name: command.name.clone(),
                    description: command.description.clone(),
                    usage: command.usage.clone(),
                });
            }
        }
        registry
    }

    /// Worker controls remain a runtime slash surface; extension setup stays
    /// in `/extensions`. Only the live first-party owner can advertise it.
    ///
    /// Test-only observation surface: the tests below assert that only the
    /// live first-party owner is advertised, while production slash completion
    /// lists extension commands from `command_suggestions_with_usage`.
    #[cfg(test)]
    pub(crate) fn tui_command_suggestions(&self) -> Vec<(String, String)> {
        self.processes
            .iter()
            .filter(|process| {
                process.descriptor().manifest.name == SUBAGENTS_EXTENSION_NAME
                    && process.is_running()
            })
            .flat_map(|process| {
                process
                    .contributions()
                    .commands
                    .iter()
                    .filter(|command| command.name == "subagents")
                    .map(|command| (command.name.clone(), command.description.clone()))
            })
            .collect()
    }

    /// Returns the manifest identity that owns one registered slash command.
    pub fn command_owner(&self, command: &str) -> Option<String> {
        self.command_registry()
            .into_iter()
            .find(|entry| entry.public_name == command)
            .map(|entry| entry.owner)
    }

    /// The name the owning extension registered for a public registry entry.
    /// Only the dispatcher needs it: a namespaced invocation still reaches the
    /// extension under the name it declared.
    pub(crate) fn registered_command_name(&self, owner: &str, command: &str) -> Option<String> {
        self.command_registry()
            .into_iter()
            .find(|entry| entry.public_name == command && entry.owner == owner)
            .map(|entry| entry.registered_name)
    }

    pub(crate) fn resolve_presentation_action(
        &self,
        extension: &str,
        action_id: &str,
    ) -> anyhow::Result<octet_agent::ExtensionPresentationAction> {
        self.presentation_views()
            .into_iter()
            .find(|view| view.extension == extension)
            .and_then(|view| {
                view.snapshot
                    .actions
                    .into_iter()
                    .find(|action| action.id == action_id)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                "extension presentation action {extension:?}/{action_id:?} is unavailable or stale"
            )
            })
    }

    #[cfg_attr(not(test), allow(dead_code))] // used by tests only
    pub async fn execute_presentation_action_with_confirmation<H>(
        &mut self,
        extension: &str,
        action_id: &str,
        confirmations: &mut H,
    ) -> anyhow::Result<String>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        let action = self.resolve_presentation_action(extension, action_id)?;
        let mut approval_budget = 0;
        if action.destructive {
            let request = ConfirmationRequest {
                parent_request_id: None,
                prompt: format!("Run {}?", action.label),
                detail: Some(format!("Declared by extension {extension:?}")),
                destructive: true,
                default: false,
            };
            if !confirmations.confirm(extension, &request).await? {
                anyhow::bail!("extension presentation action was denied");
            }
            approval_budget = 1;
        }
        let mut command_confirmations = PreapprovedExtensionConfirmation {
            inner: confirmations,
            remaining: approval_budget,
        };
        self.execute_command_with_confirmation_scoped(
            Some(extension),
            &action.command,
            action.arguments,
            &mut command_confirmations,
            None,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("extension presentation action routed to an unavailable command")
        })
    }

    /// The options menu `/extensions` shows for one running extension: its
    /// own `menu/collect` answer, or entries generated from its declared
    /// commands when it offers no menu. `None` when it is not running.
    pub async fn options_menu(
        &mut self,
        extension: &str,
    ) -> anyhow::Result<Option<ExtensionOptions>> {
        let _ = self.drain_events();
        let Some(process) = self
            .processes
            .iter()
            .find(|process| process.descriptor().manifest.name == extension && process.is_running())
            .cloned()
        else {
            return Ok(None);
        };
        if !process.contributions().menu {
            return Ok(Some(generated_options(&process)));
        }
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        let menu = tokio::time::timeout(MENU_COLLECT_DEADLINE, process.collect_menu(context))
            .await
            .map_err(|_| anyhow::anyhow!("timed out after {MENU_COLLECT_DEADLINE:?}"))
            .and_then(|menu| menu.map_err(anyhow::Error::from))
            .with_context(|| format!("{extension} could not build its options menu"))?;
        Ok(Some(ExtensionOptions {
            menu,
            generated: false,
        }))
    }

    /// Entries generated from a running extension's declared commands, used
    /// when it offers no menu or its menu could not be built.
    pub fn generated_options_menu(&self, extension: &str) -> Option<ExtensionOptions> {
        self.processes
            .iter()
            .find(|process| process.descriptor().manifest.name == extension && process.is_running())
            .map(generated_options)
    }

    /// Runs one options-menu action: a declared command of `extension`, behind
    /// a host confirmation when the extension marked it destructive. `place`
    /// names the menu the action was chosen from.
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(not(test), allow(dead_code))] // used by tests only
    pub async fn execute_menu_action_with_confirmation<H>(
        &mut self,
        extension: &str,
        label: &str,
        place: &str,
        command: &str,
        arguments: Vec<String>,
        destructive: bool,
        confirmations: &mut H,
    ) -> anyhow::Result<String>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        let mut approval_budget = 0;
        if destructive {
            let request = ConfirmationRequest {
                parent_request_id: None,
                prompt: format!("{label}?"),
                detail: Some(format!("{place} · offered by {extension}")),
                destructive: true,
                default: false,
            };
            if !confirmations.confirm(extension, &request).await? {
                anyhow::bail!("{label} was cancelled");
            }
            approval_budget = 1;
        }
        let mut command_confirmations = PreapprovedExtensionConfirmation {
            inner: confirmations,
            remaining: approval_budget,
        };
        // The person started this action and watches its progress, and can
        // cancel it, so it may outlast the ordinary request deadline.
        self.execute_command_with_confirmation_scoped(
            Some(extension),
            command,
            arguments,
            &mut command_confirmations,
            Some(MENU_ACTION_DEADLINE),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("{extension} no longer offers {label:?}"))
    }

    #[cfg_attr(not(test), allow(dead_code))] // used by tests only
    pub async fn execute_command_with_confirmation<H>(
        &mut self,
        name: &str,
        arguments: Vec<String>,
        confirmations: &mut H,
    ) -> anyhow::Result<Option<String>>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        self.execute_command_with_confirmation_scoped(None, name, arguments, confirmations, None)
            .await
    }

    /// Prepare an owned command. No fleet or frontend borrow survives this call,
    /// so the interactive owner can apply a real session mutation while it waits.
    pub(crate) fn prepare_owned_command(
        &mut self,
        extension: Option<&str>,
        name: &str,
        arguments: Vec<String>,
        attended: bool,
        approval_budget: usize,
    ) -> Option<OwnedExtensionCommand> {
        let process = self
            .processes
            .iter()
            .find(|process| {
                extension.is_none_or(|extension| process.descriptor().manifest.name == extension)
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == name)
            })?
            .clone();
        let extension_name = process.descriptor().manifest.name.clone();
        let generation = process.health_snapshot().generation;
        let execution_context =
            extension_execution_context(&process, self.resource_owner.as_deref());
        let events = process.subscribe();
        self.command_dialog_process = Some((extension_name.clone(), generation));
        let (request_started, started) = tokio::sync::oneshot::channel();
        let cancellation_token = CancellationToken::default();
        let cancellation = cancellation_token.clone();
        let (progress_sink, progress_rx) = ToolProgressSink::bounded_channel();
        let executing_process = process.clone();
        let command_name = name.to_owned();
        let execution = Box::pin(async move {
            if attended {
                executing_process
                    .execute_attended_command_with_progress(
                        command_name,
                        arguments,
                        execution_context,
                        cancellation,
                        progress_sink,
                        request_started,
                        MENU_ACTION_DEADLINE,
                    )
                    .await
            } else {
                executing_process
                    .execute_command_controlled_with_progress(
                        command_name,
                        arguments,
                        execution_context,
                        cancellation,
                        progress_sink,
                        request_started,
                    )
                    .await
            }
        });
        let mut frontend_tick = tokio::time::interval(Duration::from_millis(50));
        frontend_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Some(OwnedExtensionCommand {
            process,
            name: name.to_owned(),
            extension_name,
            generation,
            owner: self.resource_owner.clone(),
            events,
            events_open: true,
            remote_ui_wake: self.remote_ui_wake(),
            frontend_tick,
            started,
            operation: None,
            cancellation_token,
            execution,
            progress_rx,
            approval_budget,
            output: None,
            headless_messages: BoundedDiagnostics::default(),
            owner_replaced: false,
        })
    }

    #[cfg_attr(not(test), allow(dead_code))] // used by tests only
    pub(super) async fn execute_command_with_confirmation_scoped<H>(
        &mut self,
        extension: Option<&str>,
        name: &str,
        arguments: Vec<String>,
        confirmations: &mut H,
        attended_deadline: Option<Duration>,
    ) -> anyhow::Result<Option<String>>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        let Some(mut command) =
            self.prepare_owned_command(extension, name, arguments, attended_deadline.is_some(), 0)
        else {
            return Ok(None);
        };
        // Frontends without the App owner retain idle barriers only. The
        // interactive runner opts into yielding mutations to its sole owner.
        let failure = command
            .advance(self, confirmations, false, None)
            .await
            .err();
        command.finish(self, confirmations, failure).await.map(Some)
    }

    /// Executes an extension command at a non-interactive boundary.
    ///
    /// Extension confirmation requests are explicitly denied and reported as a
    /// failed invocation because no trusted confirmation surface is available to
    /// the caller. Commands that do not request confirmation retain ordinary
    /// output and queued-context handling.
    pub async fn execute_command_without_confirmation(
        &mut self,
        name: &str,
        arguments: Vec<String>,
    ) -> anyhow::Result<Option<String>> {
        self.execute_command_headless_scoped(None, name, arguments, 0)
            .await
    }

    pub(super) async fn execute_command_headless_scoped(
        &mut self,
        extension: Option<&str>,
        name: &str,
        arguments: Vec<String>,
        approval_budget: usize,
    ) -> anyhow::Result<Option<String>> {
        let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                extension.is_none_or(|extension| process.descriptor().manifest.name == extension)
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == name)
            })
            .cloned()
        else {
            return Ok(None);
        };
        let extension_name = process.descriptor().manifest.name.clone();
        let execution_context =
            extension_execution_context(&process, self.resource_owner.as_deref());
        let output = execute_headless_command(
            &process,
            name,
            arguments,
            execution_context,
            approval_budget,
            &mut self.diagnostics,
        )
        .await?;
        self.enqueue_contexts(&extension_name, output.context);
        let mut blocks = Vec::new();
        if !output.text.trim().is_empty() {
            blocks.push(output.text);
        }
        blocks.extend(
            output
                .notifications
                .iter()
                .map(|notification| format_notification(name, notification)),
        );
        blocks.extend(self.drain_events());
        Ok(Some(blocks.join("\n")))
    }

    /// Start a semantic tool renderer without stalling Agent events or input.
    /// Returns whether a matching renderer was registered.
    pub fn request_tool_render(
        &mut self,
        id: ToolCallId,
        name: &str,
        arguments: serde_json::Value,
        output: Option<String>,
        is_error: bool,
    ) -> bool {
        let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                process
                    .contributions()
                    .tool_renderers
                    .iter()
                    .any(|tool| tool == name)
                    && !process.supports_feature(
                        octet_agent::extension_process::EXTENSION_FEATURE_TRANSCRIPT_RENDER,
                    )
            })
            .cloned()
        else {
            return false;
        };
        self.renderer_tasks.retain(|task| !task.is_finished());
        let sender = self.background_tx.clone();
        let name = name.to_owned();
        let request = ToolRenderRequest {
            name: name.clone(),
            arguments,
            output,
            is_error,
            context: process.current_context(),
        };
        self.renderer_tasks.push(tokio::spawn(async move {
            let (update, diagnostic) =
                match tokio::time::timeout(RENDERER_RPC_DEADLINE, process.render_tool(request))
                    .await
                {
                    Err(_) => (
                        None,
                        Some(format!(
                            "warning: renderer for {name:?} exceeded {RENDERER_RPC_DEADLINE:?}"
                        )),
                    ),
                    Ok(Err(error)) => (
                        None,
                        Some(format!("warning: renderer for {name:?} failed: {error}")),
                    ),
                    Ok(Ok(rendered)) => (
                        Some(ExtensionToolRenderUpdate {
                            id,
                            segments: rendered.segments,
                        }),
                        None,
                    ),
                };
            let _ = sender
                .send(ExtensionBackgroundUpdate::Renderer { update, diagnostic })
                .await;
        }));
        true
    }
}

/// One public slash-command identity in the single registry. A name claimed by
/// more than one owner keeps its plain form for the first owner; later owners
/// are addressed as `<owner>:<name>` so no registration silently shadows
/// another and the owning extension still receives its declared name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegisteredExtensionCommand {
    pub(crate) public_name: String,
    pub(crate) owner: String,
    pub(crate) registered_name: String,
    pub(crate) description: String,
    pub(crate) usage: Option<String>,
}

/// The transport request owns its process and cancellation/accounting lifetime.
/// `advance` borrows the fleet only until completion or a lifecycle handoff.
/// No extension receives a mutable App or a recursive lifecycle dispatcher.
pub(crate) struct OwnedExtensionCommand {
    process: ExtensionProcess,
    name: String,
    extension_name: String,
    generation: u64,
    owner: Option<String>,
    owner_replaced: bool,
    events: broadcast::Receiver<ExtensionEvent>,
    events_open: bool,
    remote_ui_wake: Option<Arc<tokio::sync::Notify>>,
    frontend_tick: tokio::time::Interval,
    started: tokio::sync::oneshot::Receiver<ExtensionOperationToken>,
    operation: Option<ExtensionOperationToken>,
    cancellation_token: CancellationToken,
    execution: Pin<Box<dyn Future<Output = Result<CommandOutput, ExtensionRuntimeError>> + Send>>,
    progress_rx: mpsc::Receiver<ToolProgress>,
    approval_budget: usize,
    output: Option<Result<CommandOutput, ExtensionRuntimeError>>,
    headless_messages: BoundedDiagnostics,
}

impl OwnedExtensionCommand {
    pub(crate) async fn advance<H>(
        &mut self,
        extensions: &mut ExecutableExtensions,
        confirmations: &mut H,
        mutations: bool,
        mut session_owner: Option<(&mut Agent, &SessionStore)>,
    ) -> anyhow::Result<Option<ExtensionSessionLifecycleRequest>>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        if self.output.is_some() {
            return Ok(None);
        }
        let Self {
            process,
            name,
            extension_name,
            events,
            events_open,
            remote_ui_wake,
            frontend_tick,
            started,
            operation,
            cancellation_token,
            execution,
            progress_rx,
            approval_budget,
            headless_messages,
            ..
        } = self;
        let legacy_uncorrelated = process.api_version() == EXTENSION_API_VERSION_0_1;
        let result = loop {
            if let Some(shell) = confirmations.command_shell() {
                if !shell.is_agent_run_active() {
                    if mutations {
                        if let Some(request) = extensions.next_session_lifecycle_request() {
                            return Ok(Some(request));
                        }
                    }
                    if let Some(request) = extensions
                        .session_lifecycle_receiver
                        .as_mut()
                        .and_then(ExtensionSessionLifecycleReceiver::try_next_idle_wait)
                    {
                        request.respond(extensions.session_id.clone().ok_or(
                                octet_agent::extension_process::ExtensionSessionLifecycleError::Unavailable,
                            ));
                    }
                }
                for message in extensions.drain_events_for_shell(shell) {
                    shell.notice(message);
                }
                // A synchronous Pi append blocks its factory thread until the
                // real foreground Agent commits it. Draining only into the
                // session-request queue would deadlock this live command. The
                // caller lends the current Agent on each advance, including
                // after session replacement; retained old contexts stay fenced.
                if let Some((agent, sessions)) = session_owner.as_mut() {
                    extensions.apply_session_host_requests(agent, sessions);
                }
                if extensions.sync_semantic_ui(shell) {
                    shell.render();
                }
            } else {
                // Headless commands can await reverse requests too (e.g. MCP
                // registration). Use the same bounded, owner-fenced drain,
                // without an editor lease, and retain notices until completion.
                headless_messages.extend(extensions.drain_events());
            }
            // The cancellation future and confirmation UI borrow the same
            // frontend. Keep the select in its own scope so cancellation
            // is dropped before a confirmation prompt borrows it again.
            let mut command_progress = None;
            let mut command_input = None;
            let event = {
                let cancellation = confirmations.wait_for_command_event();
                tokio::pin!(cancellation);
                tokio::select! {
                    result = &mut *execution => break result,
                    started = &mut *started, if operation.is_none() => match started {
                        Ok(started) => {
                            *operation = Some(started);
                            None
                        }
                        Err(_) => break execution.await,
                    },
                    progress = progress_rx.recv() => {
                        command_progress = progress;
                        None
                    },
                    event = events.recv(), if *events_open && operation.is_some() => Some(event),
                    incoming = &mut cancellation => {
                        command_input = Some(incoming.with_context(|| format!(
                            "command input failed for extension {extension_name:?}"
                        ))?);
                        None
                    }
                    _ = remote_ui::notified(remote_ui_wake) => None,
                    _ = frontend_tick.tick() => None,
                }
            };
            if let Some(incoming) = command_input {
                let cancelled = match incoming {
                    None => true,
                    Some(event) => {
                        if confirmations.command_cancellation_event(&event) {
                            true
                        } else {
                            let consumed = confirmations.command_shell().is_some_and(|shell| {
                                extensions.route_remote_ui_event(shell, &event, false)
                            });
                            !consumed && confirmations.command_event(event)
                        }
                    }
                };
                if cancelled {
                    cancellation_token.cancel();
                    anyhow::bail!("extension command {name:?} cancelled");
                }
                continue;
            }
            if let Some(progress) = command_progress {
                if let ToolProgress::Confirmation(request) = &progress {
                    let approved = confirmations
                        .confirm_effect(extension_name, request)
                        .await?;
                    request.respond(approved);
                } else {
                    confirmations.progress(extension_name, &progress);
                }
                continue;
            }
            let Some(event) = event else {
                continue;
            };
            match event {
                Ok(ExtensionEvent::ConfirmationRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    request,
                }) if parent_request_id.is_some_and(|parent| {
                    operation.is_some_and(|operation| operation.owns(generation, parent))
                }) || (legacy_uncorrelated
                    && parent_request_id.is_none()
                    && operation.is_some_and(|operation| operation.generation == generation)) =>
                {
                    if process.confirmation_answered(&request_id, generation) {
                        continue;
                    }
                    let confirmed = if *approval_budget > 0 {
                        *approval_budget -= 1;
                        true
                    } else {
                        confirmations
                            .confirm(extension_name, &request)
                            .await
                            .with_context(|| {
                                format!("confirmation UI failed for extension {extension_name:?}")
                            })?
                    };
                    process
                        .respond_to_confirmation(
                            request_id,
                            generation,
                            ConfirmationResponse { confirmed },
                        )
                        .await?;
                }
                Ok(ExtensionEvent::PolicyEvaluationRequested { .. }) => {}
                Ok(ExtensionEvent::InputRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    request,
                }) if operation
                    .is_some_and(|operation| operation.owns(generation, parent_request_id)) =>
                {
                    if process.input_answered(&request_id, generation) {
                        continue;
                    }
                    let value = confirmations
                        .input(extension_name, &request)
                        .await
                        .with_context(|| {
                            format!("input UI failed for extension {extension_name:?}")
                        })?;
                    process
                        .respond_to_input(request_id, generation, ExtensionInputResponse { value })
                        .await?;
                }
                Ok(_) => {
                    // The product's persistent receiver owns ordinary
                    // notifications, status, context, and diagnostics.
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    extensions.diagnostics.push(format!(
                        "warning: {extension_name}: confirmation listener lagged by {count} events"
                    ));
                }
                Err(broadcast::error::RecvError::Closed) => *events_open = false,
            }
        };

        self.output = Some(result);
        Ok(None)
    }

    /// Keep the real command deadline and cancellation path polled while the
    /// application performs a lifecycle operation. Never drop an in-progress
    /// durable mutation just because its requesting command has settled.
    pub(crate) async fn during_lifecycle<F: Future>(&mut self, operation: F) -> F::Output {
        tokio::pin!(operation);
        tokio::select! {
            result = &mut self.execution, if self.output.is_none() => {
                self.output = Some(result);
                operation.await
            }
            result = &mut operation => result,
        }
    }

    /// Reload can replace the owner while retaining its path-derived key.
    pub(crate) fn session_replaced(&mut self) {
        self.owner_replaced = true;
    }

    pub(crate) async fn finish<H>(
        mut self,
        extensions: &mut ExecutableExtensions,
        confirmations: &mut H,
        failure: Option<anyhow::Error>,
    ) -> anyhow::Result<String>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        if self.output.is_none() {
            self.cancellation_token.cancel();
            // Drive native cancellation; its request guard retains permits
            // through remote settlement or the bounded grace/kill boundary.
            self.output = Some((&mut self.execution).await);
        }
        extensions.command_dialog_process = None;
        if let Some(shell) = confirmations.command_shell() {
            for message in extensions.drain_events_for_shell(shell) {
                shell.notice(message);
            }
            extensions.sync_semantic_ui(shell);
            shell.render();
        }
        confirmations.finish_progress(&self.extension_name);
        if let Some(error) = failure {
            return Err(error);
        }
        let output = self
            .output
            .take()
            .expect("command settled before finalization")?;
        // An old command may finish after switching the foreground session.
        // Its text remains a command result; its prompt contributions must not
        // enter the replacement owner, even after a same-path reload.
        let current = !self.owner_replaced
            && self.owner == extensions.resource_owner
            && self.process.health_snapshot().generation == self.generation
            && extensions.processes.iter().any(|process| {
                process.extension_instance_id() == self.process.extension_instance_id()
                    && process.health_snapshot().generation == self.generation
            });
        if current {
            extensions.enqueue_contexts(&self.extension_name, output.context);
        }
        let mut blocks = Vec::new();
        if !output.text.trim().is_empty() {
            blocks.push(output.text);
        }
        blocks.extend(
            output
                .notifications
                .iter()
                .map(|notification| format_notification(&self.name, notification)),
        );
        if let Some(shell) = confirmations.command_shell() {
            blocks.extend(extensions.drain_events_for_shell(shell));
        } else {
            blocks.extend(self.headless_messages.iter().cloned());
            blocks.extend(extensions.drain_events());
        }
        Ok(blocks.join("\n"))
    }
}

impl Drop for OwnedExtensionCommand {
    fn drop(&mut self) {
        self.cancellation_token.cancel();
    }
}
