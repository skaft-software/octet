//! ExecutableExtensions host requests: admission, session and context operations, editor and terminal grants.

use super::*;

impl ExecutableExtensions {
    /// Answer one host-mediated extension request exactly once without blocking
    /// the terminal thread.
    pub(super) fn queue_host_request_response(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        outcome: ExtensionRequestOutcome,
    ) {
        let name = process.descriptor().manifest.name.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {name}: host-owned extension request response requires the Tokio runtime"
            ));
            return;
        };
        handle.spawn(async move {
            let _ = tokio::time::timeout(
                EXTENSION_UI_EDITOR_RESPONSE_DEADLINE,
                process.respond_to_extension_request(request_id, generation, outcome),
            )
            .await;
        });
    }

    /// Refuse one request without changing host state.
    pub(super) fn refuse_host_request(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        failure: ExtensionRequestFailure,
        message: impl Into<String>,
    ) {
        self.queue_host_request_response(
            process,
            request_id,
            generation,
            ExtensionRequestOutcome::Failed(failure, message.into()),
        );
    }

    /// Fence one drained request on foreground resource ownership, negotiated
    /// feature, and payload bounds before it can touch host state. `foreground`
    /// is the authority this drain may exercise: an interactive frontend that
    /// is merely between shell pumps queues an eligible request for that shell,
    /// while a host with no interactive consumer refuses it outright.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_host_request(
        &mut self,
        process: Option<ExtensionProcess>,
        name: &str,
        request_id: ExtensionRequestId,
        generation: u64,
        owner: Option<octet_agent::extension_process::ExtensionResourceOwner>,
        operation: HostRequestOperation,
        foreground: ForegroundAdmission,
    ) {
        let Some(process) = process else {
            self.diagnostics.push(format!(
                "warning: {name}: host-owned extension request has no process"
            ));
            return;
        };
        let owner_is_foreground =
            host_request_owner_is_foreground(owner.as_ref(), self.resource_owner.as_deref());
        let remote_owner_valid = !matches!(&operation, HostRequestOperation::RemoteUi { owner, .. }
            if owner.extension_instance_id != process.extension_instance_id()
                || owner.process_generation != generation
                || !process.is_running()
                || process.health_snapshot().generation != generation);
        if !owner_is_foreground || !remote_owner_valid {
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::NotForegroundOwner,
                format!(
                    "{} is refused: the request owner is not the foreground session",
                    host_request_operation_name(&operation)
                ),
            );
            return;
        }
        let feature = host_request_feature(&operation);
        if !process.supports_feature(feature) {
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::UnsupportedFeature,
                format!("{feature} is not a negotiated extension feature"),
            );
            return;
        }
        if let Err((failure, message)) = validate_host_request(&operation) {
            self.refuse_host_request(process, request_id, generation, failure, message);
            return;
        }
        if !foreground.services_later() {
            // Tool reads and policy-bounded selection need no editor lease.
            // Re-fence the live instance; selection can only narrow the shared
            // host-policed catalog. Other requests stay refused because no
            // foreground consumer exists to service them.
            if matches!(
                &operation,
                HostRequestOperation::ContextSnapshot(ExtensionContextOperation::Tools)
                    | HostRequestOperation::ActiveTools { .. }
            ) {
                let outcome = if process.is_running()
                    && process.health_snapshot().generation == generation
                    && owner.as_ref().is_some_and(|owner| {
                        owner.extension_instance_id == process.extension_instance_id()
                            && owner.process_generation == generation
                    }) {
                    match operation {
                        HostRequestOperation::ActiveTools { names } => match &self.tool_host {
                            Some(host) => {
                                match host.set_active_tools(Some(&names.into_iter().collect())) {
                                    Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
                                    Err(error) => ExtensionRequestOutcome::Failed(
                                        ExtensionRequestFailure::InvalidRequest,
                                        error,
                                    ),
                                }
                            }
                            None => ExtensionRequestOutcome::Failed(
                                ExtensionRequestFailure::UnsupportedFeature,
                                "live tool registry is not bound".into(),
                            ),
                        },
                        _ => self.tool_snapshot_outcome(),
                    }
                } else {
                    ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::NotForegroundOwner,
                        "tool request owner is no longer foreground".into(),
                    )
                };
                self.queue_host_request_response(process, request_id, generation, outcome);
                return;
            }
            let message = match &operation {
                HostRequestOperation::Composer(_) => {
                    "no foreground composer is available in this host mode".to_owned()
                }
                _ => "no foreground session is available in this host mode".to_owned(),
            };
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::InvalidRequest,
                message,
            );
            return;
        }
        if self.pending_host_requests.len() + self.pending_session_requests.len()
            >= HOST_REQUEST_QUEUE_CAPACITY
        {
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::InvalidRequest,
                "the host extension request queue is full".to_owned(),
            );
            return;
        }
        let pending = PendingHostRequest {
            process,
            request_id,
            generation,
            operation,
        };
        if matches!(
            pending.operation,
            HostRequestOperation::SessionEntry(_)
                | HostRequestOperation::ContextSnapshot(ExtensionContextOperation::SystemPrompt)
        ) {
            self.pending_session_requests.push_back(pending);
        } else {
            self.pending_host_requests.push_back(pending);
        }
    }

    /// Maps one active-tool application result to the extension-facing outcome.
    ///
    /// Enforcement lives in the agent: only narrowing inside the host-policed
    /// surface is accepted, and unknown or policy-excluded names are refused with
    /// no state change. This keeps the wire vocabulary honest for either direction.
    pub(super) fn active_tools_outcome(
        result: Result<(), octet_agent::AgentError>,
    ) -> ExtensionRequestOutcome {
        match result {
            Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the active tool set was refused: {error}"),
            ),
        }
    }

    /// Install one runtime shortcut binding for the foreground generation.
    pub(super) fn register_dynamic_shortcut(
        &mut self,
        name: &str,
        process: ExtensionProcess,
        generation: u64,
        shortcut_id: String,
        key: &str,
        description: String,
    ) -> ExtensionRequestOutcome {
        let parsed = match dynamic_shortcut_binding(key) {
            Ok(parsed) => parsed,
            Err((outcome, diagnostic)) => {
                // A host binding always wins. The refusal is typed and the
                // diagnostic is visible in the extension status surface.
                self.diagnostics
                    .push(format!("warning: extension {name:?}: {diagnostic}"));
                return outcome;
            }
        };
        if self.shortcuts.iter().any(|existing| existing.key == parsed) {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is already bound to an extension shortcut"),
            );
        }
        if self
            .dynamic_shortcuts
            .iter()
            .any(|existing| existing.key == parsed)
        {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is already registered"),
            );
        }
        if self
            .dynamic_shortcuts
            .iter()
            .any(|existing| existing.extension == name && existing.shortcut_id == shortcut_id)
        {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut id {shortcut_id:?} is already registered"),
            );
        }
        if self.dynamic_shortcuts.len() >= MAX_EXTENSION_SHORTCUTS {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!("at most {MAX_EXTENSION_SHORTCUTS} runtime shortcuts are supported"),
            );
        }
        self.dynamic_shortcuts.push(RegisteredDynamicShortcut {
            extension: name.to_owned(),
            shortcut_id,
            key: parsed,
            description,
            process,
            generation,
        });
        ExtensionRequestOutcome::Ok(serde_json::json!({}))
    }

    /// Successfully initialized live generations, for detecting which bindings
    /// a resource rebuild actually replaced rather than merely retained.
    pub(crate) fn running_generations(&self) -> BTreeMap<String, (String, u64)> {
        self.processes
            .iter()
            .filter(|process| process.is_running())
            .map(|process| {
                (
                    process.descriptor().manifest.name.clone(),
                    (
                        process.extension_instance_id().to_owned(),
                        process.health_snapshot().generation,
                    ),
                )
            })
            .collect()
    }

    /// Pending host requests are possible interruptions, not proven losses.
    pub(crate) fn pending_host_request_count(&self) -> usize {
        self.pending_host_requests.len()
    }

    pub(crate) fn discard_stale_host_requests(&mut self) -> Vec<String> {
        let mut discarded = Vec::new();
        self.pending_host_requests.retain(|pending| {
            if let Some(notice) = pending.discard_notice() {
                discarded.push(notice);
                false
            } else {
                true
            }
        });
        self.diagnostics.extend(discarded.iter().cloned());
        discarded
    }

    /// Answer the requests the live shell can resolve. Session-entry requests
    /// stay queued for the product loop that owns the session store.
    pub(super) fn drain_host_requests_into_shell(&mut self, shell: &mut InteractiveShell) {
        for _ in 0..self.pending_host_requests.len() {
            let pending = self
                .pending_host_requests
                .pop_front()
                .expect("queued request");
            if let Some(notice) = pending.discard_notice() {
                self.diagnostics.push(notice.clone());
                shell.notice(notice);
                continue;
            }
            let name = pending.process.descriptor().manifest.name.clone();
            let outcome = match pending.operation {
                HostRequestOperation::Composer(operation) => match operation {
                    ExtensionComposerOperation::Get => ExtensionRequestOutcome::Ok(
                        serde_json::json!({ "text": shell.extension_editor_snapshot().text }),
                    ),
                    ExtensionComposerOperation::Set { .. }
                    | ExtensionComposerOperation::Insert { .. }
                        if self.remote_ui.editor_owns_composer() =>
                    {
                        ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::InvalidRequest,
                            "custom-editor composer writes require an editor checkpoint".into(),
                        )
                    }
                    ExtensionComposerOperation::Set { .. }
                    | ExtensionComposerOperation::Insert { .. }
                        if !shell.extension_editor_snapshot().focused =>
                    {
                        ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::InvalidRequest,
                            "native composer does not own input".into(),
                        )
                    }
                    ExtensionComposerOperation::Checkpoint {
                        text,
                        owner,
                        checkpoint,
                    } => {
                        if self.resource_owner.as_deref() != Some(owner.session_id.as_str()) {
                            ExtensionRequestOutcome::Failed(
                                ExtensionRequestFailure::NotForegroundOwner,
                                "editor checkpoint owner is no longer foreground".into(),
                            )
                        } else {
                            match pending.process.commit_editor_checkpoint(
                                &pending.request_id,
                                pending.generation,
                                &owner,
                                &checkpoint,
                                || {
                                    self.remote_ui.checkpoint_editor(
                                        &owner,
                                        &checkpoint,
                                        text,
                                        shell,
                                    )
                                },
                            ) {
                                // The guarded mutation already admitted the exact
                                // ACK. Never enqueue a second asynchronous response.
                                Ok(()) => continue,
                                Err((failure, detail)) => {
                                    ExtensionRequestOutcome::Failed(failure, detail)
                                }
                            }
                        }
                    }
                    ExtensionComposerOperation::Set { text } => {
                        shell.extension_set_editor(text);
                        ExtensionRequestOutcome::Ok(serde_json::json!({}))
                    }
                    ExtensionComposerOperation::Insert { text } => {
                        shell.extension_paste_editor(text);
                        ExtensionRequestOutcome::Ok(serde_json::json!({}))
                    }
                    ExtensionComposerOperation::History { entries } => {
                        shell.seed_prompt_history(entries);
                        ExtensionRequestOutcome::Ok(serde_json::json!({}))
                    }
                },
                HostRequestOperation::Shortcut {
                    shortcut_id,
                    key,
                    description,
                } => self.register_dynamic_shortcut(
                    &name,
                    pending.process.clone(),
                    pending.generation,
                    shortcut_id,
                    &key,
                    description,
                ),
                HostRequestOperation::MessageInjection(injection) => {
                    use octet_agent::extension_process::ExtensionMessageDelivery as Delivery;
                    let message = match injection {
                        ExtensionMessageInjection::User {
                            text,
                            content,
                            deliver_as,
                        } => {
                            let mut input = ComposedInput::from_text(text);
                            if let Some(content) = content {
                                input.parts = content.input_parts();
                                input.display_text = content.text();
                                input.transcript_text = input.display_text.clone();
                            }
                            crate::tui::view::PendingExtensionMessage {
                                input,
                                delivery: deliver_as.unwrap_or(Delivery::FollowUp),
                                wake: true,
                                context_only: false,
                            }
                        }
                        ExtensionMessageInjection::Custom {
                            custom_type,
                            content,
                            display,
                            details,
                            deliver_as,
                            trigger_turn,
                        } => {
                            let custom = octet_agent::session::CustomMessage {
                                custom_type,
                                content,
                                display,
                                details,
                            };
                            let mut input = ComposedInput::from_text(String::new());
                            input.parts.clear();
                            if custom.display {
                                input.transcript_text =
                                    format!("[{}]\n{}", custom.custom_type, custom.text());
                            }
                            input.custom_messages.push(custom);
                            crate::tui::view::PendingExtensionMessage {
                                input,
                                delivery: deliver_as.unwrap_or(Delivery::Steer),
                                wake: trigger_turn == Some(true),
                                context_only: trigger_turn == Some(false),
                            }
                        }
                    };
                    shell.queue_extension_message(message);
                    ExtensionRequestOutcome::Ok(serde_json::json!({}))
                }
                HostRequestOperation::SessionEntry(_) => {
                    // Unreachable: session-entry requests use their own queue.
                    continue;
                }
                HostRequestOperation::ActiveTools { names } => match &self.tool_host {
                    Some(host) => match host.set_active_tools(Some(&names.into_iter().collect())) {
                        Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
                        Err(error) => ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::InvalidRequest,
                            error,
                        ),
                    },
                    None => ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::UnsupportedFeature,
                        "live tool registry is not bound".into(),
                    ),
                },
                HostRequestOperation::Terminal(operation) => self.apply_terminal_host_request(
                    shell,
                    pending.process.clone(),
                    pending.generation,
                    operation,
                ),
                HostRequestOperation::ContextSnapshot(operation) => {
                    self.apply_context_snapshot_in_shell(shell, operation)
                }
                HostRequestOperation::RemoteUi { owner, operation } => {
                    // Re-fence the complete owner immediately before mounting.
                    if self.resource_owner.as_deref() != Some(owner.session_id.as_str())
                        || owner.extension_instance_id != pending.process.extension_instance_id()
                        || owner.process_generation != pending.generation
                    {
                        ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::NotForegroundOwner,
                            "remote UI owner is no longer foreground".into(),
                        )
                    } else {
                        match self.remote_ui.apply(
                            pending.process.clone(),
                            owner,
                            operation,
                            shell,
                            self.terminal_arbiter.active().is_some(),
                        ) {
                            Ok(result) => {
                                // Publish ownership before the next queued request:
                                // close followed by paste can share this drain.
                                // Later sync sees this projection as unchanged, so
                                // wake rendering here even if no more input arrives.
                                if shell.set_remote_ui(self.remote_ui.projection()) {
                                    shell.render();
                                }
                                ExtensionRequestOutcome::Ok(result)
                            }
                            Err((failure, detail)) => {
                                ExtensionRequestOutcome::Failed(failure, detail)
                            }
                        }
                    }
                }
            };
            self.queue_host_request_response(
                pending.process,
                pending.request_id,
                pending.generation,
                outcome,
            );
        }
    }

    /// Answer one read-only foreground context snapshot the live shell can
    /// resolve. `SystemPrompt` is routed to the session owner and can never
    /// reach this path; the two session-context operations resolve against the
    /// cached host state and the shell's admitted follow-up queue.
    pub(super) fn apply_context_snapshot_in_shell(
        &self,
        shell: &InteractiveShell,
        operation: ExtensionContextOperation,
    ) -> ExtensionRequestOutcome {
        match operation {
            ExtensionContextOperation::SessionManager => self.context_session_manager_outcome(),
            ExtensionContextOperation::PendingMessages => {
                Self::context_pending_messages_outcome(shell)
            }
            ExtensionContextOperation::Tools => self.tool_snapshot_outcome(),
            ExtensionContextOperation::SystemPrompt => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "the system prompt is resolved by the session owner".to_owned(),
            ),
        }
    }

    fn tool_snapshot_outcome(&self) -> ExtensionRequestOutcome {
        match &self.tool_host {
            Some(host) => ExtensionRequestOutcome::Ok(host.pi_tool_snapshot()),
            None => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::UnsupportedFeature,
                "live tool registry is not bound".into(),
            ),
        }
    }

    /// Compose the active session-manager snapshot from the cached host state
    /// and the workspace root. A missing session or workspace is refused rather
    /// than answered with a fabricated placeholder.
    pub(super) fn context_session_manager_outcome(&self) -> ExtensionRequestOutcome {
        let Some(session_id) = self.session_id.clone() else {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "no foreground session is available".to_owned(),
            );
        };
        if self.workspace.as_os_str().is_empty() {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "no foreground workspace is available".to_owned(),
            );
        }
        let state = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let active_skills = state
            .active_skills
            .iter()
            .take(MAX_EXTENSION_CONTEXT_ACTIVE_SKILLS)
            .map(|skill| ContextSkillSummary {
                id: skill.id.clone(),
                name: skill.name.clone(),
            })
            .collect();
        let reasoning = state
            .reasoning
            .as_ref()
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let result = ContextSessionManagerResult {
            session_id,
            name: state.session_name,
            model: state.model,
            reasoning,
            active_skills,
            cwd: self.workspace.to_string_lossy().into_owned(),
        };
        match serde_json::to_value(result) {
            Ok(value) => ExtensionRequestOutcome::Ok(value),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the session snapshot is not serializable: {error}"),
            ),
        }
    }

    /// Count the follow-up messages the foreground shell has queued but not yet
    /// admitted to the agent. This is exactly the queue
    /// `session/send_user_message` feeds, so the count is observed, never guessed.
    pub(super) fn context_pending_messages_outcome(
        shell: &InteractiveShell,
    ) -> ExtensionRequestOutcome {
        let pending = u32::try_from(shell.queued_follow_up_len()).unwrap_or(u32::MAX);
        match serde_json::to_value(ContextPendingMessagesResult { pending }) {
            Ok(value) => ExtensionRequestOutcome::Ok(value),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the pending-message count is not serializable: {error}"),
            ),
        }
    }

    /// Compose the bounded system-prompt disclosure from the live agent. The
    /// composed prompt is refused rather than truncated when it exceeds the wire
    /// disclosure bound.
    pub(super) fn context_system_prompt_outcome(agent: &Agent) -> ExtensionRequestOutcome {
        let result = ContextSystemPromptResult {
            text: agent.system_prompt().to_owned(),
        };
        if let Err(error) = result.validate() {
            return ExtensionRequestOutcome::Failed(ExtensionRequestFailure::BoundsExceeded, error);
        }
        match serde_json::to_value(result) {
            Ok(value) => ExtensionRequestOutcome::Ok(value),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the system prompt snapshot is not serializable: {error}"),
            ),
        }
    }

    /// Answers one `session/append_entry` request against the durable
    /// foreground session. The extension manifest name is the entry namespace
    /// and the live process generation is the provenance attestation.
    ///
    /// The wire protocol admits 64 KiB of entry data, but the durable store
    /// retains at most [`MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES`] encoded
    /// envelope bytes, so the smaller effective cap is enforced here and named
    /// in the refusal. A residual `SessionError::Limit` after that pre-check is
    /// a malformed value (unusable namespace, control characters, excessive
    /// nesting), never a size overflow, so it maps to `invalid_request`.
    pub(super) fn apply_extension_entry_append(
        session: &mut Session,
        namespace: &str,
        process_generation: u64,
        entry_type: &str,
        data: Value,
    ) -> ExtensionRequestOutcome {
        if entry_type.is_empty() || entry_type.chars().any(char::is_control) {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "entry_type must be a non-empty string without control characters".to_owned(),
            );
        }
        if entry_type.len() > MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!("entry_type exceeds {MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES} bytes"),
            );
        }
        let envelope = serde_json::json!({ "entry_type": entry_type, "data": &data });
        let encoded = match serde_json::to_vec(&envelope) {
            Ok(encoded) => encoded,
            Err(error) => {
                return ExtensionRequestOutcome::Failed(
                    ExtensionRequestFailure::InvalidRequest,
                    format!("entry data is not serializable: {error}"),
                );
            }
        };
        if encoded.len() > MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!(
                    "entry data exceeds {MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES} stored bytes"
                ),
            );
        }
        match session.append_extension_entry(namespace, Some(process_generation), entry_type, data)
        {
            Ok(entry_id) => {
                ExtensionRequestOutcome::Ok(serde_json::json!({ "entry_id": entry_id.0 }))
            }
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the extension session entry was refused: {error}"),
            ),
        }
    }

    /// Answers one `session/set_label` request against the durable foreground
    /// session. An unknown entry id is refused with `invalid_request` and the
    /// store's refusal path leaves the session file byte-identical; control
    /// characters reach the store and are reported as a malformed request.
    pub(super) fn apply_extension_entry_label(
        session: &mut Session,
        entry_id: &str,
        label: &str,
    ) -> ExtensionRequestOutcome {
        if label.len() > MAX_EXTENSION_SESSION_LABEL_BYTES {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!("entry label exceeds {MAX_EXTENSION_SESSION_LABEL_BYTES} bytes"),
            );
        }
        match session.set_entry_label(&EntryId(entry_id.to_owned()), label) {
            Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
            Err(SessionError::UnknownEntry(id)) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("no session entry {id:?} exists in the foreground session"),
            ),
            Err(SessionError::Limit(message)) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the extension entry label was refused: {message}"),
            ),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the extension entry label could not be stored: {error}"),
            ),
        }
    }

    /// Resolve admitted session-entry and active-tool requests against the
    /// durable foreground session and the live session store. Returns true when
    /// durable session presentation changed; extension entries, their labels,
    /// and the active tool set are not part of the coding-agent shell
    /// presentation.
    ///
    /// Both operations need the live [`Agent`] (the durable session for entries
    /// and labels, the host-policed tool surface for `tools/set_active`), so the
    /// caller hands over the foreground agent instead of a bare session.
    pub fn apply_session_host_requests(
        &mut self,
        agent: &mut Agent,
        sessions: &SessionStore,
    ) -> bool {
        let mut changed = false;
        while let Some(pending) = self.pending_session_requests.pop_front() {
            if !pending.process.is_running()
                || pending.process.health_snapshot().generation != pending.generation
            {
                self.diagnostics.push(format!(
                    "warning: {}: discarded host-owned session request from stale generation {}",
                    pending.process.descriptor().manifest.name,
                    pending.generation
                ));
                continue;
            }
            if let HostRequestOperation::ActiveTools { names } = pending.operation {
                // Narrowing only: the agent refuses unknown or policy-excluded
                // names and can never widen the host-policed tool surface.
                let outcome =
                    Self::active_tools_outcome(agent.set_active_tool_names(Some(
                        names.iter().cloned().collect::<BTreeSet<_>>(),
                    )));
                self.queue_host_request_response(
                    pending.process,
                    pending.request_id,
                    pending.generation,
                    outcome,
                );
                continue;
            }
            if let HostRequestOperation::ContextSnapshot(operation) = pending.operation {
                let outcome = match operation {
                    ExtensionContextOperation::SystemPrompt => {
                        Self::context_system_prompt_outcome(agent)
                    }
                    ExtensionContextOperation::Tools => {
                        ExtensionRequestOutcome::Ok(agent.extension_tool_snapshot())
                    }
                    // The two session-context reads resolve against the shell
                    // drain, never this agent-owning loop.
                    ExtensionContextOperation::SessionManager
                    | ExtensionContextOperation::PendingMessages => continue,
                };
                self.queue_host_request_response(
                    pending.process,
                    pending.request_id,
                    pending.generation,
                    outcome,
                );
                continue;
            }
            let session = agent.session_mut();
            // The extension's own manifest name is the durable metadata
            // namespace; the admitting generation is the provenance value.
            let namespace = pending.process.descriptor().manifest.name.clone();
            let HostRequestOperation::SessionEntry(operation) = pending.operation else {
                continue;
            };
            let outcome = match operation {
                ExtensionSessionEntryOperation::SetName { name } => match self.session_id.clone() {
                    None => ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::InvalidRequest,
                        "no foreground session is available".to_owned(),
                    ),
                    Some(session_id) => match sessions.rename(&session_id, &name) {
                        Ok(_) => {
                            if pending
                                .process
                                .supports_feature(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2)
                            {
                                if let Err(error) = pending.process.notify_session_info_changed() {
                                    self.diagnostics.push(format!(
                                        "warning: {}: session info notification failed: {error}",
                                        pending.process.descriptor().manifest.name
                                    ));
                                }
                            }
                            changed = true;
                            ExtensionRequestOutcome::Ok(serde_json::json!({}))
                        }
                        Err(error) => ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::InvalidRequest,
                            format!("the session name could not be stored: {error}"),
                        ),
                    },
                },
                ExtensionSessionEntryOperation::Append { entry_type, data } => {
                    let outcome = Self::apply_extension_entry_append(
                        session,
                        &namespace,
                        pending.generation,
                        &entry_type,
                        data,
                    );
                    if pending.process.supports_feature(
                        octet_agent::extension_process::EXTENSION_FEATURE_TRANSCRIPT_RENDER,
                    ) {
                        if let ExtensionRequestOutcome::Ok(value) = &outcome {
                            if let Some(entry) = value
                                .get("entry_id")
                                .and_then(Value::as_str)
                                .and_then(|id| session.entry(&EntryId(id.to_owned())))
                            {
                                let owner = extension_execution_context(
                                    &pending.process,
                                    self.resource_owner.as_deref(),
                                )
                                .resource_owner;
                                if let (Some(owner), Some(entry)) = (
                                    owner,
                                    octet_agent::extension_process::transcript_private_entry(
                                        entry, &namespace,
                                    ),
                                ) {
                                    if !self.transcript_renderers.committed(
                                        owner,
                                        namespace.clone(),
                                        entry,
                                    ) {
                                        self.diagnostics.push("warning: transcript entry observation limit reached; resume restores durable entries");
                                    }
                                }
                            }
                        }
                    }
                    outcome
                }
                ExtensionSessionEntryOperation::SetLabel { entry_id, label } => {
                    Self::apply_extension_entry_label(session, &entry_id, &label)
                }
            };
            self.queue_host_request_response(
                pending.process,
                pending.request_id,
                pending.generation,
                outcome,
            );
        }
        changed
    }

    pub(super) fn drain_editor_requests_into_shell(&mut self, shell: &mut InteractiveShell) {
        while let Some(pending) = self.pending_editor_requests.pop_front() {
            if !pending.process.is_running()
                || pending.process.health_snapshot().generation != pending.generation
            {
                self.diagnostics.push(format!(
                    "warning: {}: discarded host-owned editor request from stale generation {}",
                    pending.process.descriptor().manifest.name,
                    pending.generation
                ));
                continue;
            }
            let snapshot = match pending.request {
                ExtensionEditorRequest::Get => shell.extension_editor_snapshot(),
                ExtensionEditorRequest::Set { text } => shell.extension_set_editor(text),
                ExtensionEditorRequest::Paste { text } => shell.extension_paste_editor(text),
                ExtensionEditorRequest::Focus => shell.extension_focus_editor(),
            };
            self.queue_editor_response(
                pending.process,
                pending.request_id,
                pending.generation,
                ExtensionEditorResponse {
                    text: snapshot.text,
                    revision: snapshot.revision,
                    focused: snapshot.focused,
                },
            );
        }
    }

    /// Answers one `terminal/acquire` or `terminal/release` against the live
    /// foreground shell. Every caller receives exactly one typed answer: the
    /// minted grant, an empty release body, or a typed refusal.
    ///
    /// The terminal stays host-owned. The host leaves raw mode and parks its
    /// input before it answers an acquire, and re-enters only after a release
    /// it accepted; a refused caller never changes host terminal state.
    pub(super) fn apply_terminal_host_request(
        &mut self,
        shell: &mut InteractiveShell,
        process: ExtensionProcess,
        generation: u64,
        operation: ExtensionTerminalOperation,
    ) -> ExtensionRequestOutcome {
        // Re-fence the generation even though the drain already discarded stale
        // generations: a reload between admission and drain must never move the
        // tty, and the child must never act on a grant it will not be told about.
        if !process.is_running() || process.health_snapshot().generation != generation {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "the terminal request belongs to a stale process generation".to_owned(),
            );
        }
        let holder = TerminalHolder {
            owner: self.resource_owner.clone(),
            instance_id: process.extension_instance_id().to_owned(),
            generation,
            name: process.descriptor().manifest.name.clone(),
        };
        match operation {
            ExtensionTerminalOperation::Acquire => {
                if !self.remote_ui.is_empty() || shell.remote_ui_input_blocked() {
                    return ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::InvalidRequest,
                        "terminal handoff conflicts with a remote component or host input owner"
                            .into(),
                    );
                }
                // Read the size the host is leaving behind, then hand the tty
                // over: the answer is the last thing the host does here.
                let (columns, rows) = shell.terminal_dimensions();
                let granted = match self.terminal_arbiter.acquire(holder, columns, rows) {
                    Ok(granted) => granted,
                    Err((failure, message)) => {
                        return ExtensionRequestOutcome::Failed(failure, message);
                    }
                };
                shell.cede_terminal_input();
                shell.suspend();
                ExtensionRequestOutcome::Ok(serde_json::json!({
                    "grant_id": granted.grant_id,
                    "columns": granted.columns,
                    "rows": granted.rows,
                }))
            }
            ExtensionTerminalOperation::Release => {
                if let Err((failure, message)) = self.terminal_arbiter.release(&holder) {
                    return ExtensionRequestOutcome::Failed(failure, message);
                }
                // The holder's own release is not a revocation: no
                // `terminal/grant-lost` is fired for it.
                shell.release_terminal_input();
                match shell.resume() {
                    Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
                    Err(error) => ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::InvalidRequest,
                        format!("the host could not re-enter its terminal: {error}"),
                    ),
                }
            }
        }
    }

    /// Restore the host terminal when the live grant stopped being valid: the
    /// holder died or restarted, or the foreground session moved on.
    ///
    /// The host revokes without waiting on the previous holder, so a crash
    /// mid-grant can never wedge the TUI. A holder that outlived its own grant
    /// is told through `terminal/grant-lost`; the holder's own release never
    /// reaches here.
    pub fn reconcile_terminal_grant_for_shell(&mut self, shell: &mut InteractiveShell) {
        let owner = self.resource_owner.clone();
        let live: BTreeSet<(String, u64)> = self
            .processes
            .iter()
            .filter(|process| process.is_running())
            .map(|process| {
                (
                    process.extension_instance_id().to_owned(),
                    process.health_snapshot().generation,
                )
            })
            .collect();
        let Some(revoked) = self.terminal_arbiter.revoke_if(|holder| {
            holder.owner == owner && live.contains(&(holder.instance_id.clone(), holder.generation))
        }) else {
            return;
        };
        let holder_still_live = live.contains(&(
            revoked.holder.instance_id.clone(),
            revoked.holder.generation,
        ));
        let reason = if holder_still_live {
            "the foreground session changed while the terminal was ceded"
        } else {
            "the foreground terminal grant holder is no longer running"
        };
        self.restore_revoked_terminal_grant(shell, revoked, reason);
    }

    /// Revoke before dropping or replacing this binding. Reconciliation alone
    /// cannot find the old grant after a replacement App owns a fresh arbiter.
    pub fn revoke_terminal_grant_for_shell(&mut self, shell: &mut InteractiveShell, reason: &str) {
        for notice in self.remote_ui.revoke(reason) {
            shell.notice(notice);
        }
        shell.set_remote_ui(self.remote_ui.projection());
        if let Some(revoked) = self.terminal_arbiter.revoke_if(|_| false) {
            self.restore_revoked_terminal_grant(shell, revoked, reason);
        }
    }

    pub(super) fn restore_revoked_terminal_grant(
        &mut self,
        shell: &mut InteractiveShell,
        revoked: ActiveTerminalGrant,
        reason: &str,
    ) {
        shell.release_terminal_input();
        if let Err(error) = shell.resume() {
            self.diagnostics.push(format!(
                "warning: {}: the host could not re-enter its terminal after the grant was revoked: {error}",
                revoked.holder.name
            ));
        }
        // Only a holder that is still alive can hear the revocation; a dead
        // process is dropped instead of surfaced as a failed notification.
        if let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                process.is_running()
                    && process.extension_instance_id() == revoked.holder.instance_id
                    && process.health_snapshot().generation == revoked.holder.generation
            })
            .cloned()
        {
            if let Err(error) = process.notify_terminal_grant_lost(reason) {
                self.diagnostics.push(format!(
                    "warning: {}: terminal/grant-lost could not be delivered: {error}",
                    revoked.holder.name
                ));
            }
        }
        self.diagnostics.push(format!(
            "{}: the foreground terminal grant was revoked ({reason})",
            revoked.holder.name
        ));
    }

    /// Whether an extension currently holds the ceded foreground terminal.
    pub fn terminal_grant_is_active(&self) -> bool {
        self.terminal_arbiter.active().is_some()
    }
}
