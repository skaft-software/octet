//! ExecutableExtensions semantic UI, editor state and terminal input.

use super::*;

impl ExecutableExtensions {
    pub(super) fn schedule_confirmation_denials(&mut self) {
        self.confirmation_tasks.retain(|task| !task.is_finished());
        if tokio::runtime::Handle::try_current().is_err() {
            if !self.confirmation_denials.is_empty() {
                self.diagnostics.push(
                    "warning: extension confirmation denials require the octet Tokio runtime",
                );
            }
            return;
        }
        while self.confirmation_tasks.len() < CONFIRMATION_DENIAL_CONCURRENCY {
            let Some(pending) = self.confirmation_denials.pop_front() else {
                break;
            };
            self.confirmation_tasks.push(tokio::spawn(async move {
                let _ = pending
                    .process
                    .respond_to_confirmation(
                        pending.request_id,
                        pending.generation,
                        ConfirmationResponse { confirmed: false },
                    )
                    .await;
            }));
        }
    }

    pub(super) fn schedule_input_cancellations(&mut self) {
        self.input_tasks.retain(|task| !task.is_finished());
        if tokio::runtime::Handle::try_current().is_err() {
            if !self.input_cancellations.is_empty() {
                self.diagnostics
                    .push("warning: extension input cancellation requires the octet Tokio runtime");
            }
            return;
        }
        while self.input_tasks.len() < INPUT_CANCELLATION_CONCURRENCY {
            let Some(pending) = self.input_cancellations.pop_front() else {
                break;
            };
            self.input_tasks.push(tokio::spawn(async move {
                let _ = pending
                    .process
                    .respond_to_input(
                        pending.request_id,
                        pending.generation,
                        ExtensionInputResponse { value: None },
                    )
                    .await;
            }));
        }
    }

    pub(super) fn queue_confirmation_denial(&mut self, pending: PendingConfirmationDenial) {
        if self.confirmation_denials.len() >= CONFIRMATION_DENIAL_QUEUE_CAPACITY {
            self.diagnostics.push(format!(
                "warning: extension confirmation denial queue reached its {CONFIRMATION_DENIAL_QUEUE_CAPACITY}-request limit; newest request was dropped"
            ));
            return;
        }
        self.confirmation_denials.push_back(pending);
        self.schedule_confirmation_denials();
    }

    pub(super) fn queue_input_cancellation(&mut self, pending: PendingInputCancellation) {
        if self.input_cancellations.len() >= INPUT_CANCELLATION_QUEUE_CAPACITY {
            self.diagnostics.push(format!(
                "warning: extension input cancellation queue reached its {INPUT_CANCELLATION_QUEUE_CAPACITY}-request limit; newest request was dropped"
            ));
            return;
        }
        self.input_cancellations.push_back(pending);
        self.schedule_input_cancellations();
    }

    pub(super) fn apply_semantic_ui_contribution(
        &mut self,
        extension: String,
        process: &ExtensionProcess,
        generation: u64,
        contribution: ExtensionUiContribution,
    ) -> Result<(), String> {
        let health = process.health_snapshot();
        if !process.is_running() || health.generation != generation {
            return Err(format!(
                "discarded semantic UI contribution from stale generation {generation}"
            ));
        }
        let instance_id = process.extension_instance_id().to_owned();
        let view = self.semantic_ui.entry(extension).or_default();
        if view.generation != generation || view.extension_instance_id != instance_id {
            *view = SemanticUiView {
                extension_instance_id: instance_id,
                generation,
                ..SemanticUiView::default()
            };
        }
        match contribution {
            ExtensionUiContribution::Status {
                key,
                text,
                style_role,
                priority,
            } => {
                if let Some(text) = text {
                    if !view.statuses.contains_key(&key)
                        && view.statuses.len().saturating_add(view.widgets.len())
                            >= MAX_EXTENSION_UI_ENTRIES
                    {
                        return Err(format!(
                            "semantic UI entry limit {MAX_EXTENSION_UI_ENTRIES} reached"
                        ));
                    }
                    view.statuses.insert(
                        key,
                        SemanticUiStatus {
                            text,
                            style_role,
                            priority,
                        },
                    );
                } else {
                    view.statuses.remove(&key);
                }
            }
            ExtensionUiContribution::Widget {
                key,
                lines,
                placement,
                style_role,
                priority,
            } => {
                if let Some(lines) = lines {
                    if !view.widgets.contains_key(&key)
                        && view.statuses.len().saturating_add(view.widgets.len())
                            >= MAX_EXTENSION_UI_ENTRIES
                    {
                        return Err(format!(
                            "semantic UI entry limit {MAX_EXTENSION_UI_ENTRIES} reached"
                        ));
                    }
                    view.widgets.insert(
                        key,
                        SemanticUiWidget {
                            lines,
                            placement,
                            style_role,
                            priority,
                        },
                    );
                } else {
                    view.widgets.remove(&key);
                }
            }
            ExtensionUiContribution::Working {
                message,
                visible,
                frames,
                interval_ms,
            } => {
                view.working = Some(ShellExtensionWorking {
                    message,
                    visible,
                    frames,
                    interval_ms,
                });
            }
            ExtensionUiContribution::HiddenThinking { label } => {
                view.hidden_thinking_label = label;
            }
        }
        Ok(())
    }

    /// Apply one header/footer status surface against the live process
    /// generation. Keyed `status` surfaces flow through
    /// [`Self::apply_semantic_ui_contribution`] and are ignored here.
    pub(super) fn apply_status_surface(
        &mut self,
        extension: String,
        process: &ExtensionProcess,
        contribution: ExtensionStatusContribution,
    ) {
        self.record_status_surface(
            extension,
            process.extension_instance_id().to_owned(),
            process.health_snapshot().generation,
            contribution,
        );
    }

    /// Store one header/footer surface under its exact instance and generation
    /// fence. Split from [`Self::apply_status_surface`] so the store step is
    /// testable without a live process.
    pub(super) fn record_status_surface(
        &mut self,
        extension: String,
        instance_id: String,
        generation: u64,
        contribution: ExtensionStatusContribution,
    ) {
        let is_header = match contribution.surface {
            ExtensionUiSurface::Header => true,
            ExtensionUiSurface::Footer => false,
            ExtensionUiSurface::Status => return,
        };
        let view = self.semantic_ui.entry(extension).or_default();
        if view.generation != generation || view.extension_instance_id != instance_id {
            *view = SemanticUiView {
                extension_instance_id: instance_id,
                generation,
                ..SemanticUiView::default()
            };
        }
        let slot = (!contribution.text.is_empty()).then(|| SemanticUiStatus {
            text: bounded_surface_text(&contribution.text),
            style_role: contribution.style_role,
            priority: contribution.priority,
        });
        if is_header {
            view.header = slot;
        } else {
            view.footer = slot;
        }
    }

    pub(super) fn prune_semantic_ui(&mut self) {
        self.semantic_ui.retain(|extension, view| {
            self.processes.iter().any(|process| {
                process.descriptor().manifest.name == *extension
                    && process.is_running()
                    && process.extension_instance_id() == view.extension_instance_id
                    && process.health_snapshot().generation == view.generation
            })
        });
        self.autocomplete_registrations
            .retain(|extension, registration| {
                self.processes.iter().any(|process| {
                    process.descriptor().manifest.name == *extension
                        && process.is_running()
                        && process.extension_instance_id() == registration.extension_instance_id
                        && process.health_snapshot().generation == registration.generation
                })
            });
    }

    pub(super) fn semantic_ui_projection(&mut self) -> ShellExtensionUi {
        self.prune_semantic_ui();
        Self::project_semantic_ui(&self.semantic_ui)
    }

    /// Fold every retained semantic-UI view into one shell projection. Split
    /// from [`Self::semantic_ui_projection`] so the fold is testable without a
    /// live process fleet.
    pub(super) fn project_semantic_ui(
        views: &BTreeMap<String, SemanticUiView>,
    ) -> ShellExtensionUi {
        let mut statuses = Vec::new();
        let mut above_editor = Vec::new();
        let mut below_editor = Vec::new();
        let mut header = Vec::new();
        let mut footer = Vec::new();
        let mut working = None;
        let mut hidden_thinking_label = None;
        for view in views.values() {
            for status in view.statuses.values() {
                statuses.push(ShellExtensionUiLine {
                    text: status.text.clone(),
                    style_role: status.style_role.clone(),
                    priority: status.priority,
                });
            }
            for widget in view.widgets.values() {
                let target = match widget.placement {
                    ExtensionWidgetPlacement::AboveEditor => &mut above_editor,
                    ExtensionWidgetPlacement::BelowEditor => &mut below_editor,
                };
                target.extend(
                    widget
                        .lines
                        .iter()
                        .cloned()
                        .map(|text| ShellExtensionUiLine {
                            text,
                            style_role: widget.style_role.clone(),
                            priority: widget.priority,
                        }),
                );
            }
            if working.is_none() {
                working = view.working.clone();
            }
            if hidden_thinking_label.is_none() {
                hidden_thinking_label = view.hidden_thinking_label.clone();
            }
            if let Some(surface) = &view.header {
                header.push(ShellExtensionUiLine {
                    text: surface.text.clone(),
                    style_role: surface.style_role.clone(),
                    priority: surface.priority,
                });
            }
            if let Some(surface) = &view.footer {
                footer.push(ShellExtensionUiLine {
                    text: surface.text.clone(),
                    style_role: surface.style_role.clone(),
                    priority: surface.priority,
                });
            }
        }
        let sort = |left: &ShellExtensionUiLine, right: &ShellExtensionUiLine| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.text.cmp(&right.text))
        };
        statuses.sort_by(sort);
        above_editor.sort_by(sort);
        below_editor.sort_by(sort);
        header.sort_by(sort);
        footer.sort_by(sort);
        statuses.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        above_editor.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        below_editor.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        header.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        footer.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        ShellExtensionUi {
            statuses,
            above_editor,
            below_editor,
            header,
            footer,
            working,
            hidden_thinking_label,
        }
    }

    /// Project validated semantic extension UI into the host-owned interactive
    /// shell. Any stale process generation is discarded before rendering.
    pub fn sync_semantic_ui(&mut self, shell: &mut InteractiveShell) -> bool {
        shell.set_extension_ui(self.semantic_ui_projection())
    }

    /// Notify negotiated editor-handoff extensions whenever the host-owned
    /// editor snapshot changes. The cursor deliberately remains host-local;
    /// autocomplete receives it only in its explicit request payload.
    pub fn sync_editor_state(&mut self, snapshot: ShellEditorSnapshot) {
        let state = ExtensionEditorResponse {
            text: snapshot.text,
            revision: snapshot.revision,
            focused: snapshot.focused,
        };
        let generations = self
            .processes
            .iter()
            .filter(|process| process.is_running())
            .map(|process| {
                (
                    process.descriptor().manifest.name.clone(),
                    (
                        process.health_snapshot().generation,
                        process.extension_instance_id().to_owned(),
                    ),
                )
            })
            .collect();
        let delivery = EditorStateDelivery { state, generations };
        if self.last_editor_state.as_ref() == Some(&delivery) {
            return;
        }
        self.last_editor_state = Some(delivery.clone());
        for process in &self.processes {
            if let Err(error) = process.notify_editor_state(delivery.state.clone()) {
                self.diagnostics.push(format!(
                    "warning: {}: editor state notification failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
    }

    /// Broadcast one already-normalized input observation without granting any
    /// extension an input-consumption path.
    pub fn observe_terminal_input(&mut self, data: String) {
        for process in &self.processes {
            if let Err(error) =
                process.notify_terminal_input(ExtensionTerminalInput { data: data.clone() })
            {
                self.diagnostics.push(format!(
                    "warning: {}: terminal input observation failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
    }

    /// Broadcast one host-observed resize without letting extensions own layout.
    pub fn observe_terminal_resize(&mut self, columns: u16, rows: u16) {
        for process in &self.processes {
            if let Err(error) =
                process.notify_terminal_resize(ExtensionTerminalResize { columns, rows })
            {
                self.diagnostics.push(format!(
                    "warning: {}: terminal resize observation failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
    }

    pub(super) fn queue_editor_response(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        response: ExtensionEditorResponse,
    ) {
        let name = process.descriptor().manifest.name.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {name}: host-owned editor response requires the Tokio runtime"
            ));
            return;
        };
        handle.spawn(async move {
            let _ = tokio::time::timeout(
                EXTENSION_UI_EDITOR_RESPONSE_DEADLINE,
                process.respond_to_editor(request_id, generation, response),
            )
            .await;
        });
    }

    pub(super) fn queue_autocomplete_registration_response(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        accepted: bool,
    ) {
        let name = process.descriptor().manifest.name.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {name}: autocomplete registration response requires the Tokio runtime"
            ));
            return;
        };
        handle.spawn(async move {
            let _ = tokio::time::timeout(
                EXTENSION_UI_EDITOR_RESPONSE_DEADLINE,
                process.respond_to_autocomplete_registration(request_id, generation, accepted),
            )
            .await;
        });
    }
}
