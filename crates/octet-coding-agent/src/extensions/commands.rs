//! ExecutableExtensions commands, menus, shortcuts and presentation actions.

use super::*;

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
        self.processes
            .iter()
            .flat_map(|process| {
                process.contributions().commands.iter().map(|command| {
                    (
                        command.name.clone(),
                        command.description.clone(),
                        command.usage.clone(),
                    )
                })
            })
            .collect()
    }

    /// Worker controls remain a runtime slash surface; extension setup stays
    /// in `/extensions`. Only the live first-party owner can advertise it.
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
        self.processes.iter().find_map(|process| {
            process
                .contributions()
                .commands
                .iter()
                .any(|definition| definition.name == command)
                .then(|| process.descriptor().manifest.name.clone())
        })
    }

    pub async fn execute_presentation_action_with_confirmation<H>(
        &mut self,
        extension: &str,
        action_id: &str,
        confirmations: &mut H,
    ) -> anyhow::Result<String>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        let action = self
            .presentation_views()
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
            })?;
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

    /// Executes an authenticated Serve action with one command-scoped approval.
    #[cfg(feature = "serve")]
    pub async fn execute_presentation_action_for_serve(
        &mut self,
        extension: &str,
        expected_extension_instance_id: &str,
        expected_generation: u64,
        expected_revision: u64,
        action_id: &str,
        confirmed: bool,
    ) -> anyhow::Result<String> {
        let action = self
            .presentation_views()
            .into_iter()
            .find(|view| {
                view.extension == extension
                    && view.extension_instance_id == expected_extension_instance_id
                    && view.generation == expected_generation
                    && view.snapshot.revision == expected_revision
            })
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
            })?;
        if confirmed && !action.destructive {
            anyhow::bail!("non-destructive extension action cannot carry approval");
        }
        if action.destructive && !confirmed {
            anyhow::bail!("extension presentation action requires explicit confirmation");
        }
        self.execute_command_headless_scoped(
            Some(extension),
            &action.command,
            action.arguments,
            usize::from(action.destructive && confirmed),
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
        let mut events = process.subscribe();
        self.command_dialog_process =
            Some((extension_name.clone(), process.health_snapshot().generation));
        let remote_ui_wake = self.remote_ui_wake();
        let mut frontend_tick = tokio::time::interval(Duration::from_millis(50));
        frontend_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let output: anyhow::Result<_> = async {
            let legacy_uncorrelated = process.api_version() == EXTENSION_API_VERSION_0_1;
            let (request_started, started) = tokio::sync::oneshot::channel();
            let mut started = Box::pin(started);
            let mut operation = None;
            let cancellation_token = CancellationToken::default();
            let (progress_sink, mut progress_rx) = ToolProgressSink::bounded_channel();
            let mut execution: Pin<Box<dyn Future<Output = _> + Send + '_>> =
                match attended_deadline {
                    Some(deadline) => Box::pin(process.execute_attended_command_with_progress(
                        name.to_owned(),
                        arguments,
                        execution_context,
                        cancellation_token.clone(),
                        progress_sink,
                        request_started,
                        deadline,
                    )),
                    None => Box::pin(process.execute_command_controlled_with_progress(
                        name.to_owned(),
                        arguments,
                        execution_context,
                        cancellation_token.clone(),
                        progress_sink,
                        request_started,
                    )),
                };
            let mut events_open = true;
            let result = loop {
                if let Some(shell) = confirmations.command_shell() {
                    for message in self.drain_events_for_shell(shell) { shell.notice(message); }
                    if self.sync_semantic_ui(shell) { shell.render(); }
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
                        result = &mut execution => break result?,
                        started = &mut started, if operation.is_none() => match started {
                            Ok(started) => {
                                operation = Some(started);
                                None
                            }
                            Err(_) => break execution.await?,
                        },
                        progress = progress_rx.recv() => {
                            command_progress = progress;
                            None
                        },
                        event = events.recv(), if events_open && operation.is_some() => Some(event),
                        incoming = &mut cancellation => {
                            command_input = Some(incoming.with_context(|| format!(
                                "command input failed for extension {extension_name:?}"
                            ))?);
                            None
                        }
                        _ = remote_ui::notified(&remote_ui_wake) => None,
                        _ = frontend_tick.tick() => None,
                    }
                };
                if let Some(incoming) = command_input {
                    let cancelled = match incoming {
                        None => true,
                        Some(event) => {
                            let consumed = confirmations.command_shell().is_some_and(|shell| {
                                self.route_remote_ui_event(shell, &event)
                            });
                            !consumed && confirmations.command_event(event)
                        }
                    };
                    if cancelled {
                        cancellation_token.cancel();
                        anyhow::bail!("extension command {name:?} cancelled");
                    }
                    continue;
                }
                if let Some(progress) = command_progress {
                    confirmations.progress(&extension_name, &progress);
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
                        && operation
                            .is_some_and(|operation| operation.generation == generation)) =>
                    {
                        if process.confirmation_answered(&request_id, generation) {
                            continue;
                        }
                        let confirmed = confirmations
                            .confirm(&extension_name, &request)
                            .await
                            .with_context(|| {
                                format!("confirmation UI failed for extension {extension_name:?}")
                            })?;
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
                            .input(&extension_name, &request)
                            .await
                            .with_context(|| {
                                format!("input UI failed for extension {extension_name:?}")
                            })?;
                        process
                            .respond_to_input(
                                request_id,
                                generation,
                                ExtensionInputResponse { value },
                            )
                            .await?;
                    }
                    Ok(_) => {
                        // The product's persistent receiver owns ordinary
                        // notifications, status, context, and diagnostics.
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        self.diagnostics.push(format!(
                                "warning: {extension_name}: confirmation listener lagged by {count} events"
                            ));
                    }
                    Err(broadcast::error::RecvError::Closed) => events_open = false,
                }
            };
            Ok::<_, anyhow::Error>(result)
        }
        .await;
        self.command_dialog_process = None;
        if let Some(shell) = confirmations.command_shell() {
            for message in self.drain_events_for_shell(shell) {
                shell.notice(message);
            }
            self.sync_semantic_ui(shell);
            shell.render();
        }
        confirmations.finish_progress(&extension_name);
        let output = output?;
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
        if let Some(shell) = confirmations.command_shell() {
            blocks.extend(self.drain_events_for_shell(shell));
        } else {
            blocks.extend(self.drain_events());
        }
        Ok(Some(blocks.join("\n")))
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

    /// Start one host-mediated autocomplete request for the active editor
    /// snapshot. A late result is fenced by the shell revision before display.
    pub fn request_editor_autocomplete(&mut self, snapshot: ShellEditorSnapshot) -> bool {
        self.prune_semantic_ui();
        self.autocomplete_tasks.retain(|task| !task.is_finished());
        if self.autocomplete_tasks.len() >= MAX_EXTENSION_AUTOCOMPLETE_TASKS {
            return true;
        }
        let Some(registration) = self.autocomplete_registrations.values().next().cloned() else {
            return false;
        };
        let process = registration.process;
        if !process.is_running()
            || process.health_snapshot().generation != registration.generation
            || process.extension_instance_id() != registration.extension_instance_id
        {
            return false;
        }
        let request = ExtensionAutocompleteRequest {
            text: snapshot.text.clone(),
            cursor: snapshot.cursor,
            revision: snapshot.revision,
        };
        let sender = self.background_tx.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {}: autocomplete requires the Tokio runtime",
                process.descriptor().manifest.name
            ));
            return false;
        };
        self.autocomplete_tasks.push(handle.spawn(async move {
            let (update, diagnostic) = match tokio::time::timeout(
                EXTENSION_AUTOCOMPLETE_DEADLINE,
                process.request_autocomplete(request),
            )
            .await
            {
                Err(_) => (
                    None,
                    Some(format!(
                        "warning: extension autocomplete exceeded {EXTENSION_AUTOCOMPLETE_DEADLINE:?}"
                    )),
                ),
                Ok(Err(error)) => (
                    None,
                    Some(format!("warning: extension autocomplete failed: {error}")),
                ),
                Ok(Ok(response)) => (
                    Some(ExtensionAutocompleteUpdate {
                        snapshot,
                        prefix: response.prefix,
                        items: response
                            .items
                            .into_iter()
                            .map(|item| ShellAutocompleteItem {
                                value: item.value,
                                label: item.label,
                                description: item.description,
                            })
                            .collect(),
                    }),
                    None,
                ),
            };
            let _ = sender
                .send(ExtensionBackgroundUpdate::Autocomplete { update, diagnostic })
                .await;
        }));
        true
    }
}
