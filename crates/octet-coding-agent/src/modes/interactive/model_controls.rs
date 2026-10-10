//! Owner-fenced admission is separate from application at a native safe boundary.
use super::*;
use octet_agent::extension_process::{ExtensionModelControl, ExtensionResourceOwner};

pub(super) fn queue_labels(queue: &VecDeque<PendingIdleAction>) -> Vec<String> {
    queue
        .iter()
        .filter_map(|action| {
            Some(match action {
                PendingIdleAction::ChangeModel(id) => format!("model {} · idle", id.0),
                PendingIdleAction::ExtensionModelControl { selection, .. } => match selection {
                    ExtensionModelControl::Model { provider, id } => {
                        format!("model {provider}/{id} · idle")
                    }
                    ExtensionModelControl::Thinking { level } => format!("thinking {level} · idle"),
                },
                PendingIdleAction::ExtensionActiveThinking { reasoning, .. }
                | PendingIdleAction::ActiveThinkingSelection(reasoning) => {
                    format!("thinking {} · next response", reasoning_label(reasoning))
                }
                PendingIdleAction::ChangeThinking(reasoning) => {
                    format!("thinking {} · idle", reasoning_label(reasoning))
                }
                PendingIdleAction::ChangeThinkingLevel(level) => {
                    format!("thinking {} · idle", level.label())
                }
                PendingIdleAction::PersistThinkingPreference(_)
                | PendingIdleAction::SyncTheme(_)
                | PendingIdleAction::SyncImages(_) => return None,
                PendingIdleAction::NewSession => "new session · idle".into(),
                PendingIdleAction::ResumeSession(_) => "resume session · idle".into(),
                PendingIdleAction::Fork => "fork session · idle".into(),
                PendingIdleAction::Clone => "clone session · idle".into(),
                PendingIdleAction::ReloadResources => "reload · idle".into(),
                PendingIdleAction::Compact | PendingIdleAction::CompactWithInstructions(_) => {
                    "compact · idle".into()
                }
                _ => "command · idle".into(),
            })
        })
        .collect()
}

/// Respond before the hook resumes: waiting for application here would deadlock
/// an awaited hook with its own Run. Only the host-owned queue receives authority.
pub(super) fn admit_busy_control(
    request: ExtensionSessionLifecycleRequest,
    extensions: &crate::extensions::ExecutableExtensions,
    inspection: &ActiveRunInspection,
    queue: &mut VecDeque<PendingIdleAction>,
    shell: &mut InteractiveShell,
    aborting: bool,
) -> Option<ReasoningConfig> {
    let ExtensionSessionLifecycleOperation::ModelControl(selection) = request.operation().clone()
    else {
        unreachable!("model-control receiver filters operations")
    };
    let result = (|| {
        let owner = request
            .resource_owner()
            .filter(|owner| {
                owner.session_id == inspection.resource_owner
                    && extensions.model_control_owner_is_current(owner)
            })
            .ok_or_else(|| "model control owner retired".to_owned())?
            .clone();
        if request.is_cancelled() || aborting {
            return Err("model control cancelled".into());
        }
        if queue.len() >= MAX_PENDING_IDLE_ACTIONS {
            return Err("command queue is full; selection unchanged".into());
        }
        // Validate against a preceding queued model without changing the frozen
        // request or advertising that model as effective to the caller.
        let mut model = inspection.model.clone();
        for action in queue.iter() {
            match action {
                PendingIdleAction::ChangeModel(id) => {
                    if let Ok(target) = inspection.catalog.resolve(id) {
                        model = target;
                    }
                }
                PendingIdleAction::ExtensionModelControl {
                    selection: ExtensionModelControl::Model { provider, id },
                    ..
                } => {
                    if let Some(target) = crate::extensions::model_control::resolve_model(
                        &inspection.catalog,
                        provider,
                        id,
                    ) {
                        model = target;
                    }
                }
                _ => {}
            }
        }
        let reasoning = match &selection {
            ExtensionModelControl::Model { provider, id } => {
                if crate::extensions::model_control::resolve_model(
                    &inspection.catalog,
                    provider,
                    id,
                )
                .is_none()
                {
                    return Ok((serde_json::json!({"selected":false}), None));
                }
                None
            }
            ExtensionModelControl::Thinking { level } => {
                let level = ThinkingLevel::parse(level).map_err(|error| error.to_string())?;
                Some(
                    requested_thinking_to_reasoning(level, &model, inspection.subagents_available)
                        .map_err(|error| error.to_string())?,
                )
            }
        };
        let live = reasoning.filter(|_| {
            can_apply_reasoning_at_response(queue)
                && model.responses_features().reasoning_effort_updates
        });
        if let Some(reasoning) = &live {
            let ExtensionModelControl::Thinking { level } = selection else {
                unreachable!()
            };
            let action = PendingIdleAction::ExtensionActiveThinking {
                owner,
                level,
                reasoning: reasoning.clone(),
            };
            if let Some(pending) = queue.iter_mut().find(|action| is_active_thinking(action)) {
                *pending = action;
            } else {
                queue.push_back(action);
            }
        } else {
            queue.push_back(PendingIdleAction::ExtensionModelControl { owner, selection });
        }
        shell.notice("extension selection queued; effective at the next safe boundary");
        Ok((serde_json::json!({"selected":true,"queued":true}), live))
    })();
    match result {
        Ok((receipt, live)) => {
            request.respond_model_control(Ok(receipt));
            live
        }
        Err(error) => {
            request.respond_model_control(Err(error));
            None
        }
    }
}

pub(super) fn apply_queued_control(
    app: &mut App,
    shell: &mut InteractiveShell,
    owner: &ExtensionResourceOwner,
    selection: &ExtensionModelControl,
) {
    let result = if app
        .executable_extensions
        .model_control_owner_is_current(owner)
        && owner.session_id == app.agent.session().resource_owner_key()
    {
        crate::extensions::model_control::apply_idle_model_control(app, selection)
    } else {
        Err("queued model control owner retired; selection not applied".into())
    };
    match result {
        Ok(receipt) if receipt["selected"] == true => {
            request_extension_ui(shell, app);
            update_status(shell, app);
            shell.notice("extension selection applied at the idle boundary");
        }
        Ok(_) => shell.error("queued model is no longer available; selection not applied".into()),
        Err(error) => shell.error(error),
    }
}

/// The durable response-boundary record, not control-channel admission, decides
/// whether an extension request needs the idle fallback. Never persist it as a
/// user's startup preference or apply an already committed choice twice.
pub(super) fn settle_active_controls(
    queue: &mut VecDeque<PendingIdleAction>,
    effective: &ReasoningConfig,
) {
    queue.retain_mut(|action| {
        if let PendingIdleAction::ExtensionActiveThinking {
            owner,
            level,
            reasoning,
        } = action
        {
            if reasoning == effective {
                return false;
            }
            *action = PendingIdleAction::ExtensionModelControl {
                owner: owner.clone(),
                selection: ExtensionModelControl::Thinking {
                    level: level.clone(),
                },
            };
        }
        true
    });
}

pub(super) fn is_active_thinking(action: &PendingIdleAction) -> bool {
    matches!(
        action,
        PendingIdleAction::ActiveThinkingSelection(_)
            | PendingIdleAction::ExtensionActiveThinking { .. }
    )
}

pub(super) fn can_apply_reasoning_at_response(queue: &VecDeque<PendingIdleAction>) -> bool {
    queue.iter().all(|action| {
        is_active_thinking(action)
            || matches!(
                action,
                PendingIdleAction::SyncTheme(_)
                    | PendingIdleAction::SyncImages(_)
                    | PendingIdleAction::PersistThinkingPreference(_)
            )
    })
}

/// Called only on the Run's request-start signal, after reading its durable
/// selection. Unmatched controls stay pending; a mere timer is not a boundary.
pub(super) fn acknowledge_response_boundary(
    queue: &mut VecDeque<PendingIdleAction>,
    effective: &ReasoningConfig,
) -> bool {
    let mut applied = false;
    queue.retain_mut(|action| {
        match action {
            PendingIdleAction::ExtensionActiveThinking { reasoning, .. }
                if reasoning == effective =>
            {
                applied = true;
                return false;
            }
            PendingIdleAction::ActiveThinkingSelection(reasoning) if reasoning == effective => {
                *action = PendingIdleAction::PersistThinkingPreference(reasoning_label(reasoning));
                applied = true;
            }
            _ => {}
        }
        true
    });
    applied
}

/// Mutable App ownership is the idle safe boundary; original owner and request
/// cancellation are still checked independently of that execution authority.
pub(super) fn apply_idle_request(
    app: &mut App,
    request: &ExtensionSessionLifecycleRequest,
) -> Result<serde_json::Value, String> {
    let ExtensionSessionLifecycleOperation::ModelControl(selection) = request.operation() else {
        unreachable!("model-control receiver filters operations")
    };
    if request.is_cancelled()
        || !request.resource_owner().is_some_and(|owner| {
            owner.session_id == app.agent.session().resource_owner_key()
                && app
                    .executable_extensions
                    .model_control_owner_is_current(owner)
        })
    {
        return Err("model control owner retired or request cancelled".into());
    }
    crate::extensions::model_control::apply_idle_model_control(app, selection)
}
