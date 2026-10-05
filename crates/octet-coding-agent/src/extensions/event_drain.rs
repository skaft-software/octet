//! ExecutableExtensions background updates and draining extension events into the shell.

use super::*;

impl ExecutableExtensions {
    /// Drain completed renderer and autocomplete work without waiting.
    pub fn drain_background_updates(&mut self) -> ExtensionBackgroundUpdates {
        self.poll_telemetry();
        let mut updates = ExtensionBackgroundUpdates::default();
        while let Ok(update) = self.background_rx.try_recv() {
            match update {
                ExtensionBackgroundUpdate::Diagnostics(messages) => {
                    self.diagnostics.extend(messages)
                }
                ExtensionBackgroundUpdate::Renderer { update, diagnostic } => {
                    self.diagnostics.extend(diagnostic);
                    updates.rendered_tools.extend(update);
                }
                ExtensionBackgroundUpdate::Autocomplete { update, diagnostic } => {
                    self.diagnostics.extend(diagnostic);
                    updates.autocomplete.extend(update);
                }
                ExtensionBackgroundUpdate::Shortcut {
                    extension,
                    context,
                    messages,
                } => {
                    self.enqueue_contexts(&extension, context);
                    updates.shortcut_messages.extend(messages);
                }
            }
        }
        updates
    }

    pub(crate) fn remote_ui_wake(&self) -> Option<Arc<tokio::sync::Notify>> {
        self.remote_ui_wake.clone()
    }

    fn sync_remote_ui(&mut self, shell: &mut InteractiveShell) -> bool {
        for error in self
            .remote_ui
            .reconcile(self.resource_owner.as_deref(), shell.terminal_dimensions())
        {
            shell.error(error);
        }
        for process in &self.processes {
            for frame in process.take_remote_ui_frames() {
                self.remote_ui.accept_frame(process, frame);
            }
        }
        let changed = shell.set_remote_ui(self.remote_ui.projection());
        if changed {
            shell.render();
        }
        changed
    }

    /// Focused keys never reach the composer, keymap shortcuts, or passive
    /// terminal-input observers. Reconcile before routing after every wake.
    pub(crate) fn route_remote_ui_event(
        &mut self,
        shell: &mut InteractiveShell,
        event: &Event,
    ) -> bool {
        if let Event::Resize(columns, rows) = event {
            shell.set_size(*columns, *rows);
        }
        self.sync_remote_ui(shell);
        let consumed = self.remote_ui.route_input(shell, event);
        if consumed && shell.set_remote_ui(self.remote_ui.projection()) {
            shell.render();
        }
        consumed
    }

    /// Drain extension events while an interactive shell owns the editor. This
    /// is deliberately separate from the generic event drain so headless hosts
    /// never accidentally grant an editor lease.
    pub fn drain_events_for_shell(&mut self, shell: &mut InteractiveShell) -> Vec<String> {
        self.schedule_session_hook_starts();
        let messages = self.drain_events_inner(true);
        self.drain_editor_requests_into_shell(shell);
        self.drain_host_requests_into_shell(shell);
        self.reconcile_terminal_grant_for_shell(shell);
        self.sync_remote_ui(shell);
        messages
    }

    /// Drain a fixed amount of extension work without letting a continuously
    /// ready process monopolize the input/render task. The start receiver
    /// rotates between calls and each receiver has a smaller per-call quota.
    pub fn drain_events(&mut self) -> Vec<String> {
        self.drain_events_inner(false)
    }

    pub(super) fn drain_events_inner(&mut self, interactive: bool) -> Vec<String> {
        self.poll_telemetry();
        self.schedule_confirmation_denials();
        self.schedule_input_cancellations();
        let receiver_count = self.receivers.len();
        if receiver_count == 0 {
            return Vec::new();
        }

        let start = self.event_drain_cursor % receiver_count;
        let mut remaining = EVENT_DRAIN_BUDGET;
        let mut visited = 0usize;
        let mut messages = Vec::new();
        while visited < receiver_count && remaining > 0 {
            let index = (start + visited) % receiver_count;
            let name = self
                .processes
                .get(index)
                .map(|process| process.descriptor().manifest.name.clone())
                .unwrap_or_else(|| "extension".to_owned());
            let process = self.processes.get(index).cloned();
            let mut receiver_budget = EVENT_DRAIN_PER_RECEIVER_BUDGET.min(remaining);
            while receiver_budget > 0 {
                let event = self.receivers[index].try_recv();
                match event {
                    Ok(ExtensionEvent::ExecRequested { request_id, generation, owner, request }) => {
                        if let Some(process) = process.clone() {
                            if host_request_owner_is_foreground(Some(&owner), self.resource_owner.as_deref()) && process.is_running() && process.health_snapshot().generation == generation && owner.extension_instance_id == process.extension_instance_id() && owner.process_generation == generation && process.supports_feature("process_exec_v1") {
                                self.start_exec_request(process, request_id, generation, owner, request);
                            } else {
                                self.queue_host_request_response(process, request_id, generation, ExtensionRequestOutcome::Failed(
                                    ExtensionRequestFailure::NotForegroundOwner, "exec owner is no longer foreground".into()));
                            }
                        }
                    }
                    Ok(ExtensionEvent::Notification { notification }) => {
                        messages.push(format_notification(&name, &notification));
                    }
                    Ok(ExtensionEvent::ContextContributed { contribution }) => {
                        admit_context(
                            &mut self.pending_context,
                            &mut self.diagnostics,
                            &name,
                            contribution,
                        );
                    }
                    Ok(ExtensionEvent::StatusContributed { contribution }) => {
                        // Header and footer surfaces become bounded chrome; any
                        // other surface stays protocol-only. The source process
                        // generation is the freshness fence.
                        match process.as_ref() {
                            Some(process) => {
                                self.apply_status_surface(name.clone(), process, contribution);
                            }
                            None => self.diagnostics.push(format!(
                                "warning: {name}: status surface source process is unavailable"
                            )),
                        }
                    }
                    Ok(ExtensionEvent::UiContributed {
                        generation,
                        contribution,
                    }) => {
                        let Some(process) = process.as_ref() else {
                            self.diagnostics.push(format!(
                                "warning: {name}: semantic UI source process is unavailable"
                            ));
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        };
                        if let Err(error) = self.apply_semantic_ui_contribution(
                            name.clone(),
                            process,
                            generation,
                            contribution,
                        ) {
                            self.diagnostics.push(format!("warning: {name}: {error}"));
                        }
                    }
                    Ok(ExtensionEvent::EditorRequested {
                        request_id,
                        generation,
                        request,
                    }) => {
                        let Some(process) = process.clone() else {
                            self.diagnostics.push(format!(
                                "warning: {name}: host-owned editor request has no process"
                            ));
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        };
                        if interactive && self.pending_editor_requests.len() < EVENT_DRAIN_BUDGET {
                            self.pending_editor_requests
                                .push_back(PendingEditorRequest {
                                    process,
                                    request_id,
                                    generation,
                                    request,
                                });
                        } else {
                            self.queue_editor_response(
                                process,
                                request_id,
                                generation,
                                ExtensionEditorResponse {
                                    text: String::new(),
                                    revision: 0,
                                    focused: false,
                                },
                            );
                            if interactive {
                                self.diagnostics.push(format!(
                                    "warning: {name}: host-owned editor request queue is full"
                                ));
                            }
                        }
                    }
                    Ok(ExtensionEvent::ComposerRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::Composer(operation),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::SessionEntryRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::SessionEntry(operation),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::MessageInjectionRequested {
                        request_id,
                        generation,
                        owner,
                        injection,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::MessageInjection(injection),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::ShortcutRequested {
                        request_id,
                        generation,
                        owner,
                        shortcut_id,
                        key,
                        description,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::Shortcut {
                                shortcut_id,
                                key,
                                description,
                            },
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::ActiveToolsRequested {
                        request_id,
                        generation,
                        owner,
                        names,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::ActiveTools { names },
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::TerminalRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::Terminal(operation),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::ContextSnapshotRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::ContextSnapshot(operation),
                            interactive,
                        );
                    }
                    // Contract A's product half is still owed: there is no
                    // `HostRequestOperation` for a model view yet, so this
                    // request is not answered here. The Pi bridge does not issue
                    // `context/model` until that lands, so no extension is left
                    // waiting today; the arm keeps the drain exhaustive, matching
                    // the agent crate's own `ModelViewRequested` arm.
                    Ok(ExtensionEvent::RemoteUiRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            Some(owner.clone()),
                            HostRequestOperation::RemoteUi { owner, operation },
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::ModelViewRequested { .. }) => {}
                    Ok(ExtensionEvent::AutocompleteRegistered {
                        request_id,
                        generation,
                        registration: _,
                    }) => {
                        let Some(process) = process.clone() else {
                            self.diagnostics.push(format!(
                                "warning: {name}: autocomplete registration has no process"
                            ));
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        };
                        let accepted = interactive
                            && process.is_running()
                            && process.health_snapshot().generation == generation;
                        if accepted {
                            self.autocomplete_registrations.insert(
                                name.clone(),
                                RegisteredAutocomplete {
                                    extension_instance_id: process
                                        .extension_instance_id()
                                        .to_owned(),
                                    process: process.clone(),
                                    generation,
                                },
                            );
                        }
                        self.queue_autocomplete_registration_response(
                            process, request_id, generation, accepted,
                        );
                    }
                    Ok(ExtensionEvent::PresentationUpdated {
                        generation,
                        resource_owner,
                        snapshot,
                    }) => {
                        let published_owner = match admit_presentation_owner(
                            resource_owner,
                            self.resource_owner.as_deref(),
                        ) {
                            Ok(owner) => owner,
                            Err(error) => {
                                self.diagnostics.push(format!("warning: {name}: {error}"));
                                remaining -= 1;
                                receiver_budget -= 1;
                                continue;
                            }
                        };
                        let active_generation = process
                            .as_ref()
                            .map(ExtensionProcess::health_snapshot)
                            .map(|health| health.generation);
                        let extension_instance_id = process
                            .as_ref()
                            .expect("extension event receivers align with processes")
                            .extension_instance_id()
                            .to_owned();
                        if let Err(error) = reduce_presentation_update(
                            &mut self.presentations,
                            name.clone(),
                            active_generation,
                            extension_instance_id,
                            published_owner,
                            generation,
                            snapshot,
                        ) {
                            self.diagnostics.push(format!("warning: {name}: {error}"));
                        }
                    }
                    Ok(ExtensionEvent::Diagnostic { message }) => {
                        self.diagnostics.push(format!("warning: {name}: {message}"));
                    }
                    Ok(ExtensionEvent::ConfirmationRequested {
                        request_id,
                        generation,
                        request,
                        ..
                    }) => {
                        if self.command_dialog_process.as_ref() == Some(&(name.clone(), generation))
                            || process.as_ref().is_some_and(|process| {
                                process.confirmation_answered(&request_id, generation)
                            })
                        {
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        }
                        // A request outside a frontend-controlled confirmation
                        // boundary is denied through a bounded tracked queue.
                        messages.push(format!(
                            "[{name}] confirmation denied (no active confirmation UI): {}",
                            request.prompt
                        ));
                        if let Some(process) = process.clone() {
                            self.queue_confirmation_denial(PendingConfirmationDenial {
                                process,
                                request_id,
                                generation,
                            });
                        } else {
                            self.diagnostics.push(format!(
                                "warning: {name}: confirmation could not be denied because its process is unavailable"
                            ));
                        }
                    }
                    // The policy supervisor answers independently of frontend
                    // event drains; observing an intent is not a denial.
                    Ok(ExtensionEvent::PolicyEvaluationRequested { .. }) => {}
                    Ok(ExtensionEvent::InputRequested {
                        request_id,
                        generation,
                        request,
                        ..
                    }) => {
                        if self.command_dialog_process.as_ref() == Some(&(name.clone(), generation))
                            || process.as_ref().is_some_and(|process| {
                                process.input_answered(&request_id, generation)
                            })
                        {
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        }
                        messages.push(format!(
                            "[{name}] input cancelled (no active input owner): {}",
                            request.prompt
                        ));
                        if let Some(process) = process.clone() {
                            self.queue_input_cancellation(PendingInputCancellation {
                                process,
                                request_id,
                                generation,
                            });
                        } else {
                            self.diagnostics.push(format!(
                                "warning: {name}: input could not be cancelled because its process is unavailable"
                            ));
                        }
                    }
                    Err(broadcast::error::TryRecvError::Empty)
                    | Err(broadcast::error::TryRecvError::Closed) => break,
                    Err(broadcast::error::TryRecvError::Lagged(count)) => {
                        messages.push(format!(
                            "[{name}] dropped {count} extension events because the consumer lagged"
                        ));
                    }
                }
                remaining -= 1;
                receiver_budget -= 1;
            }
            visited += 1;
        }
        self.event_drain_cursor = (start + visited.max(1)) % receiver_count;
        let active_owner = self.resource_owner.as_deref();
        self.presentations.retain(|name, view| {
            (view.resource_owner.is_none() || view.resource_owner.as_deref() == active_owner)
                && self.processes.iter().any(|process| {
                    process.descriptor().manifest.name == *name
                        && process.is_running()
                        && process.health_snapshot().generation == view.generation
                })
        });
        self.prune_semantic_ui();
        self.dynamic_shortcuts.retain(|shortcut| {
            self.processes.iter().any(|process| {
                process.extension_instance_id() == shortcut.process.extension_instance_id()
                    && process.is_running()
                    && process.health_snapshot().generation == shortcut.generation
            })
        });
        self.schedule_confirmation_denials();
        self.schedule_input_cancellations();
        messages
    }
}
