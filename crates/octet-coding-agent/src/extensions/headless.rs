//! Extension shortcuts and headless command execution.

use super::*;

/// A host-validated terminal shortcut target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtensionShortcutInvocation {
    pub(crate) extension: String,
    pub(crate) name: String,
    pub(crate) description: String,
}

#[derive(Clone, Debug)]
pub(super) struct RegisteredExtensionShortcut {
    pub(super) key: ExtensionShortcutKey,
    pub(super) invocation: ExtensionShortcutInvocation,
}

pub(super) fn register_extension_shortcut(
    registered: &mut Vec<RegisteredExtensionShortcut>,
    diagnostics: &mut Vec<String>,
    extension: &str,
    shortcut: &ShortcutDefinition,
) {
    let key = match parse_extension_shortcut(&shortcut.key) {
        Ok(key) => key,
        Err(error) => {
            diagnostics.push(format!(
                "warning: extension {extension:?} shortcut {:?} was not registered: {error}",
                shortcut.key
            ));
            return;
        }
    };
    if is_reserved_extension_shortcut(&key) {
        diagnostics.push(format!(
            "warning: extension {extension:?} shortcut {:?} was not registered: binding is reserved by octet",
            shortcut.key
        ));
        return;
    }
    if let Some(existing) = registered
        .iter()
        .find(|existing: &&RegisteredExtensionShortcut| existing.key == key)
    {
        diagnostics.push(format!(
            "warning: extension {extension:?} shortcut {:?} conflicts with {}:{}; the first binding remains active",
            shortcut.key, existing.invocation.extension, existing.invocation.name
        ));
        return;
    }
    registered.push(RegisteredExtensionShortcut {
        key,
        invocation: ExtensionShortcutInvocation {
            extension: extension.to_owned(),
            name: shortcut.name.clone(),
            description: shortcut.description.clone(),
        },
    });
}

pub(super) fn register_extension_shortcuts(
    processes: &[ExtensionProcess],
) -> (Vec<RegisteredExtensionShortcut>, Vec<String>) {
    let mut registered = Vec::new();
    let mut diagnostics = Vec::new();
    for process in processes {
        let extension = &process.descriptor().manifest.name;
        for shortcut in &process.contributions().shortcuts {
            register_extension_shortcut(&mut registered, &mut diagnostics, extension, shortcut);
        }
    }
    (registered, diagnostics)
}

pub(super) async fn execute_headless_command(
    process: &ExtensionProcess,
    name: &str,
    arguments: Vec<String>,
    execution_context: octet_agent::extension_process::ExtensionExecutionContext,
    mut approval_budget: usize,
    diagnostics: &mut BoundedDiagnostics,
) -> anyhow::Result<octet_agent::extension_process::CommandOutput> {
    let extension_name = &process.descriptor().manifest.name;
    let mut events = process.subscribe();
    let (output, confirmation_denied) = {
        let mut confirmation_denied = false;
        let output = {
            let legacy_uncorrelated = process.api_version() == EXTENSION_API_VERSION_0_1;
            let (request_started, started) = tokio::sync::oneshot::channel();
            let mut started = Box::pin(started);
            let mut operation = None;
            let cancellation_token = CancellationToken::default();
            let (progress_sink, _progress_rx) = ToolProgressSink::bounded_channel();
            let mut execution = Box::pin(process.execute_command_controlled_with_progress(
                name.to_owned(),
                arguments,
                execution_context,
                cancellation_token,
                progress_sink,
                request_started,
            ));
            let mut events_open = true;
            loop {
                tokio::select! {
                    result = &mut execution => break result?,
                    started = &mut started, if operation.is_none() => match started {
                        Ok(started) => operation = Some(started),
                        Err(_) => break execution.await?,
                    },
                    event = events.recv(), if events_open && operation.is_some() => match event {
                        Ok(ExtensionEvent::ConfirmationRequested {
                            request_id,
                            generation,
                            parent_request_id,
                            ..
                        }) if parent_request_id.is_some_and(|parent| {
                            operation.is_some_and(|operation| operation.owns(generation, parent))
                        }) || (legacy_uncorrelated
                            && parent_request_id.is_none()
                            && operation.is_some_and(|operation| operation.generation == generation)) => {
                            if !process.confirmation_answered(&request_id, generation) {
                                let confirmed = approval_budget > 0;
                                if confirmed {
                                    approval_budget -= 1;
                                } else {
                                    confirmation_denied = true;
                                }
                                process
                                    .respond_to_confirmation(
                                        request_id,
                                        generation,
                                        ConfirmationResponse { confirmed },
                                    )
                                    .await?;
                            }
                        }
                        Ok(ExtensionEvent::PolicyEvaluationRequested { .. }) => {}
                        Ok(ExtensionEvent::InputRequested {
                            request_id,
                            generation,
                            parent_request_id,
                            ..
                        }) if operation.is_some_and(|operation| {
                            operation.owns(generation, parent_request_id)
                        }) => {
                            process
                                .respond_to_input(
                                    request_id,
                                    generation,
                                    ExtensionInputResponse { value: None },
                                )
                                .await?;
                        }
                        Ok(_) => {
                            // The persistent receiver owns ordinary notifications,
                            // status, context, and diagnostics.
                        }
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            diagnostics.push(format!(
                                "warning: {extension_name}: confirmation listener lagged by {count} events"
                            ));
                        }
                        Err(broadcast::error::RecvError::Closed) => events_open = false,
                    },
                }
            }
        };
        (output, confirmation_denied)
    };
    if confirmation_denied {
        anyhow::bail!("extension command {name:?} requires an interactive confirmation surface");
    }
    Ok(output)
}

pub(super) async fn execute_shortcut_headless(
    process: ExtensionProcess,
    name: String,
    execution_context: octet_agent::extension_process::ExtensionExecutionContext,
) -> (String, Vec<ContextContribution>, Vec<String>) {
    let extension = process.descriptor().manifest.name.clone();
    let mut events = process.subscribe();
    let (request_started, started) = tokio::sync::oneshot::channel();
    let mut started = Box::pin(started);
    let mut operation = None;
    let mut execution = Box::pin(process.execute_shortcut_controlled(
        name.clone(),
        execution_context,
        request_started,
    ));
    let mut events_open = true;
    let mut confirmation_denied = false;
    let mut confirmation_state_uncertain = false;
    let output = loop {
        tokio::select! {
            result = &mut execution => break result,
            started = &mut started, if operation.is_none() => match started {
                Ok(started) => operation = Some(started),
                Err(_) => break execution.await,
            },
            event = events.recv(), if events_open && operation.is_some() => match event {
                Ok(ExtensionEvent::ConfirmationRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    ..
                }) if parent_request_id.is_some_and(|parent| {
                    operation.is_some_and(|operation| operation.owns(generation, parent))
                }) => {
                    // The persistent event drain may have denied this first;
                    // either way, a background shortcut never owns an approval UI.
                    confirmation_denied = true;
                    if !process.confirmation_answered(&request_id, generation) {
                        let _ = process.respond_to_confirmation(
                            request_id,
                            generation,
                            ConfirmationResponse { confirmed: false },
                        ).await;
                    }
                }
                Ok(ExtensionEvent::InputRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    ..
                }) if operation.is_some_and(|operation| operation.owns(generation, parent_request_id)) => {
                    if !process.input_answered(&request_id, generation) {
                        let _ = process.respond_to_input(
                            request_id,
                            generation,
                            ExtensionInputResponse { value: None },
                        ).await;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    confirmation_state_uncertain = true;
                }
                Err(broadcast::error::RecvError::Closed) => events_open = false,
            },
        }
    };
    if operation.is_none() {
        if let Ok(started) = started.as_mut().get_mut().try_recv() {
            operation = Some(started);
        }
    }
    if let Some(operation) = operation {
        loop {
            match events.try_recv() {
                Ok(ExtensionEvent::ConfirmationRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    ..
                }) if parent_request_id
                    .is_some_and(|parent| operation.owns(generation, parent)) =>
                {
                    // A response may already have been sent by the persistent
                    // drain. It was necessarily a denial for this background
                    // operation, so never admit its output or context.
                    confirmation_denied = true;
                    if !process.confirmation_answered(&request_id, generation) {
                        let _ = process
                            .respond_to_confirmation(
                                request_id,
                                generation,
                                ConfirmationResponse { confirmed: false },
                            )
                            .await;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty)
                | Err(broadcast::error::TryRecvError::Closed) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    confirmation_state_uncertain = true;
                }
            }
        }
    } else if output.is_ok() {
        // A successful admitted request always reports its operation token. If
        // it did not, do not accept output that cannot be scoped safely.
        confirmation_state_uncertain = true;
    }
    match output {
        Ok(_) if confirmation_denied => {
            let message = format!(
                "extension shortcut {extension:?}/{name:?} requires an interactive confirmation and was denied"
            );
            (extension, Vec::new(), vec![message])
        }
        Ok(_) if confirmation_state_uncertain => (
            extension,
            Vec::new(),
            vec![format!(
                "extension shortcut {name:?} output was discarded because its confirmation state could not be verified"
            )],
        ),
        Ok(output) => {
            let mut messages = Vec::new();
            if !output.text.trim().is_empty() {
                messages.push(output.text);
            }
            messages.extend(
                output
                    .notifications
                    .iter()
                    .map(|notification| format_notification(&name, notification)),
            );
            (extension, output.context, messages)
        }
        Err(error) => (
            extension,
            Vec::new(),
            vec![format!("extension shortcut {name:?} failed: {error}")],
        ),
    }
}
