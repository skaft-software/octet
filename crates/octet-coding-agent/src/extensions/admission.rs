//! Bounded diagnostics, context admission and host request validation.

use super::*;

#[derive(Default)]
pub(super) struct BoundedDiagnostics {
    pub(super) entries: VecDeque<String>,
    pub(super) retained_bytes: usize,
    pub(super) dropped: u64,
}

impl BoundedDiagnostics {
    pub(super) fn push(&mut self, message: impl Into<String>) {
        let message = truncate_diagnostic(message.into());
        while !self.entries.is_empty()
            && (self.entries.len() >= MAX_DIAGNOSTIC_ENTRIES
                || self.retained_bytes.saturating_add(message.len()) > MAX_DIAGNOSTIC_BYTES)
        {
            if let Some(removed) = self.entries.pop_front() {
                self.retained_bytes = self.retained_bytes.saturating_sub(removed.len());
                self.dropped = self.dropped.saturating_add(1);
            }
        }
        self.retained_bytes = self.retained_bytes.saturating_add(message.len());
        self.entries.push_back(message);
    }

    pub(super) fn extend(&mut self, messages: impl IntoIterator<Item = String>) {
        for message in messages {
            self.push(message);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.dropped == 0
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &String> {
        self.entries.iter()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

pub(super) fn truncate_diagnostic(message: String) -> String {
    const MARKER: &str = "\n[… diagnostic truncated …]";
    if message.len() <= MAX_DIAGNOSTIC_ENTRY_BYTES {
        return message.into_boxed_str().into_string();
    }
    let mut keep = MAX_DIAGNOSTIC_ENTRY_BYTES.saturating_sub(MARKER.len());
    while !message.is_char_boundary(keep) {
        keep = keep.saturating_sub(1);
    }
    let mut bounded = String::with_capacity(MAX_DIAGNOSTIC_ENTRY_BYTES);
    bounded.push_str(&message[..keep]);
    bounded.push_str(MARKER);
    bounded
}

#[derive(Default)]
pub(super) struct PendingContext {
    pub(super) entries: VecDeque<ContextContribution>,
    pub(super) retained_bytes: usize,
}

impl PendingContext {
    pub(super) fn try_push(&mut self, mut contribution: ContextContribution) -> Result<(), String> {
        let contribution_bytes = context_contribution_bytes(&contribution)?;
        if self.entries.len() >= MAX_PENDING_CONTEXT_ITEMS {
            return Err(format!(
                "pending context exceeds the {MAX_PENDING_CONTEXT_ITEMS} contribution limit"
            ));
        }
        if self.retained_bytes.saturating_add(contribution_bytes) > MAX_EXTENSION_CONTEXT_BYTES {
            return Err(format!(
                "pending context exceeds the {MAX_EXTENSION_CONTEXT_BYTES} byte aggregate limit"
            ));
        }
        contribution.label = contribution.label.into_boxed_str().into_string();
        contribution.content = contribution.content.into_boxed_str().into_string();
        self.retained_bytes = self.retained_bytes.saturating_add(contribution_bytes);
        self.entries.push_back(contribution);
        Ok(())
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &ContextContribution> {
        self.entries.iter()
    }

    #[cfg(test)]
    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(super) fn commit(&mut self, count: usize) {
        for _ in 0..count.min(self.entries.len()) {
            if let Some(contribution) = self.entries.pop_front() {
                let bytes = context_contribution_bytes(&contribution).unwrap_or_default();
                self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
            }
        }
    }

    pub(super) fn into_vec(self) -> Vec<ContextContribution> {
        self.entries.into_iter().collect()
    }
}

pub(super) fn admit_context(
    pending_context: &mut PendingContext,
    diagnostics: &mut BoundedDiagnostics,
    source: &str,
    contribution: ContextContribution,
) -> bool {
    let label = contribution.label.clone();
    match pending_context.try_push(contribution) {
        Ok(()) => true,
        Err(error) => {
            diagnostics.push(format!(
                "warning: {source}: dropped extension context {label:?}: {error}"
            ));
            false
        }
    }
}

pub(super) fn admit_presentation_owner(
    published: Option<octet_agent::extension_process::ExtensionResourceOwner>,
    active_owner: Option<&str>,
) -> Result<Option<String>, String> {
    let published = published.map(|owner| owner.session_id);
    if published.is_some() && published.as_deref() != active_owner {
        return Err("discarded semantic presentation for another resource owner".into());
    }
    Ok(published)
}

/// Bounded wire name used in typed refusal messages.
pub(super) fn host_request_operation_name(operation: &HostRequestOperation) -> &'static str {
    match operation {
        HostRequestOperation::Composer(_) => "composer",
        HostRequestOperation::SessionEntry(_) => "session_entries",
        HostRequestOperation::MessageInjection(_) => "message_injection",
        HostRequestOperation::Shortcut { .. } => "shortcuts",
        HostRequestOperation::ActiveTools { .. } => "active_tools",
        HostRequestOperation::Terminal(_) => "terminal_handoff",
        HostRequestOperation::RemoteUi { .. } => "remote_ui",
        HostRequestOperation::ContextSnapshot(operation) => match operation {
            ExtensionContextOperation::SessionManager => "session_manager",
            ExtensionContextOperation::PendingMessages => "pending_messages",
            ExtensionContextOperation::SystemPrompt => "system_prompt",
            ExtensionContextOperation::Tools => "active_tools",
        },
    }
}

/// The negotiated feature that gates one host-mediated operation.
pub(super) fn host_request_feature(operation: &HostRequestOperation) -> &'static str {
    match operation {
        HostRequestOperation::Composer(_) => EXTENSION_FEATURE_COMPOSER,
        HostRequestOperation::SessionEntry(_) => EXTENSION_FEATURE_SESSION_ENTRIES,
        HostRequestOperation::MessageInjection(_) => EXTENSION_FEATURE_MESSAGE_INJECTION,
        HostRequestOperation::Shortcut { .. } => EXTENSION_FEATURE_SHORTCUTS,
        HostRequestOperation::ActiveTools { .. } => EXTENSION_FEATURE_ACTIVE_TOOLS,
        HostRequestOperation::Terminal(_) => EXTENSION_FEATURE_TERMINAL_HANDOFF,
        HostRequestOperation::RemoteUi { .. } => EXTENSION_FEATURE_REMOTE_UI,
        HostRequestOperation::ContextSnapshot(ExtensionContextOperation::Tools) => EXTENSION_FEATURE_ACTIVE_TOOLS,
        HostRequestOperation::ContextSnapshot(ExtensionContextOperation::SystemPrompt) => {
            EXTENSION_FEATURE_SYSTEM_PROMPT_READ
        }
        HostRequestOperation::ContextSnapshot(_) => EXTENSION_FEATURE_SESSION_CONTEXT,
    }
}

/// One bounded, char-safe slice of a failure reason for a fixed-shape
/// extension notification.
pub(super) fn bounded_notification_reason(reason: &str) -> &str {
    const MAX_NOTIFICATION_REASON_BYTES: usize = 512;
    if reason.len() <= MAX_NOTIFICATION_REASON_BYTES {
        return reason;
    }
    let mut end = MAX_NOTIFICATION_REASON_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason[..end]
}

/// Bound one header/footer surface text to the semantic-UI disclosure cap,
/// truncating on a UTF-8 boundary rather than refusing the whole contribution.
pub(super) fn bounded_surface_text(text: &str) -> String {
    const MAX_SURFACE_TEXT_BYTES: usize = 8 * 1024;
    if text.len() <= MAX_SURFACE_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_SURFACE_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The host request fence: an owner must exist and it must be the foreground
/// resource owner. A missing owner is a typed refusal, never a silent
/// coercion into the foreground session.
pub(super) fn host_request_owner_is_foreground(
    owner: Option<&octet_agent::extension_process::ExtensionResourceOwner>,
    foreground: Option<&str>,
) -> bool {
    owner.is_some_and(|owner| foreground == Some(owner.session_id.as_str()))
}

/// Resolve one runtime shortcut binding. Host-reserved bindings are refused so
/// the product keymap always wins, and the refusal stays bounded and visible.
pub(super) fn dynamic_shortcut_binding(
    key: &str,
) -> Result<ExtensionShortcutKey, (ExtensionRequestOutcome, String)> {
    let parsed = parse_extension_shortcut(key).map_err(|error| {
        (
            ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is not a supported binding: {error}"),
            ),
            format!("shortcut {key:?} was refused: {error}"),
        )
    })?;
    if is_reserved_extension_shortcut(&parsed) {
        return Err((
            ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is reserved by the host keymap"),
            ),
            format!("shortcut {key:?} was refused: the host keymap reserves this binding"),
        ));
    }
    Ok(parsed)
}

pub(super) fn bounded_host_request_text(
    field: &str,
    text: &str,
    cap: usize,
) -> Result<(), (ExtensionRequestFailure, String)> {
    if text.len() > cap {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            format!("{field} exceeds {cap} bytes"),
        ));
    }
    Ok(())
}

pub(super) fn bounded_host_request_name(
    field: &str,
    value: &str,
    cap: usize,
) -> Result<(), (ExtensionRequestFailure, String)> {
    if value.is_empty() {
        return Err((
            ExtensionRequestFailure::InvalidRequest,
            format!("{field} must not be empty"),
        ));
    }
    if value.len() > cap {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            format!("{field} exceeds {cap} bytes"),
        ));
    }
    Ok(())
}

/// Validate one extension request against the negotiated agent-side caps. No
/// payload reaches host state until this passes.
pub(super) fn validate_host_request(
    operation: &HostRequestOperation,
) -> Result<(), (ExtensionRequestFailure, String)> {
    match operation {
        HostRequestOperation::Composer(operation) => match operation {
            ExtensionComposerOperation::Get => Ok(()),
            ExtensionComposerOperation::Checkpoint {
                text, checkpoint, ..
            } => {
                checkpoint.validate()?;
                bounded_host_request_text("composer text", text, MAX_EXTENSION_COMPOSER_TEXT_BYTES)
            }
            ExtensionComposerOperation::Set { text }
            | ExtensionComposerOperation::Insert { text } => {
                bounded_host_request_text("composer text", text, MAX_EXTENSION_COMPOSER_TEXT_BYTES)
            }
        },
        HostRequestOperation::SessionEntry(operation) => match operation {
            ExtensionSessionEntryOperation::Append { entry_type, data } => {
                bounded_host_request_name(
                    "entry_type",
                    entry_type,
                    MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES,
                )?;
                let encoded = serde_json::to_string(data).map_err(|error| {
                    (
                        ExtensionRequestFailure::InvalidRequest,
                        format!("entry data is not serializable: {error}"),
                    )
                })?;
                if encoded.len() > MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES {
                    return Err((
                        ExtensionRequestFailure::BoundsExceeded,
                        format!(
                            "entry data exceeds {MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES} bytes"
                        ),
                    ));
                }
                Ok(())
            }
            ExtensionSessionEntryOperation::SetName { name } => {
                bounded_host_request_name("session name", name, MAX_EXTENSION_SESSION_NAME_BYTES)
            }
            ExtensionSessionEntryOperation::SetLabel { entry_id, label } => {
                bounded_host_request_name("entry id", entry_id, MAX_EXTENSION_UI_KEY_BYTES)?;
                bounded_host_request_text("entry label", label, MAX_EXTENSION_SESSION_LABEL_BYTES)
            }
        },
        HostRequestOperation::MessageInjection(injection) => match injection {
            ExtensionMessageInjection::User { text, .. } => bounded_host_request_text(
                "injected message text", text, MAX_EXTENSION_INJECTED_MESSAGE_BYTES,
            ),
            ExtensionMessageInjection::Custom { custom_type, content, display, details, .. } => {
                octet_agent::session::CustomMessage { custom_type: custom_type.clone(), content: content.clone(), display: *display, details: details.clone() }
                    .validate().map_err(|error| (ExtensionRequestFailure::InvalidRequest, error.to_string()))
            },
        },
        HostRequestOperation::Shortcut {
            shortcut_id,
            key,
            description,
        } => {
            bounded_host_request_name("shortcut id", shortcut_id, MAX_EXTENSION_SHORTCUT_ID_BYTES)?;
            bounded_host_request_name("shortcut key", key, MAX_EXTENSION_SHORTCUT_KEY_BYTES)?;
            bounded_host_request_text(
                "shortcut description",
                description,
                MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES,
            )
        }
        HostRequestOperation::ActiveTools { names } => {
            if names.len() > MAX_HOST_REQUEST_TOOL_NAMES {
                return Err((
                    ExtensionRequestFailure::BoundsExceeded,
                    format!("tool name list exceeds {MAX_HOST_REQUEST_TOOL_NAMES} entries"),
                ));
            }
            for name in names {
                bounded_host_request_name("tool name", name, MAX_EXTENSION_UI_KEY_BYTES)?;
            }
            Ok(())
        }
        // The handoff carries no payload: the host mints the grant and reports
        // the size it left the terminal in, so there is nothing to bound here.
        HostRequestOperation::Terminal(_) => Ok(()),
        HostRequestOperation::RemoteUi { operation, .. } => operation.validate(),
        // Read-only snapshots take no caller payload; the reply is bounded at
        // the point the host composes it.
        HostRequestOperation::ContextSnapshot(_) => Ok(()),
    }
}

pub(super) fn reduce_presentation_update(
    presentations: &mut BTreeMap<String, ExtensionPresentationView>,
    extension: String,
    active_generation: Option<u64>,
    extension_instance_id: String,
    resource_owner: Option<String>,
    generation: u64,
    snapshot: ExtensionPresentationSnapshot,
) -> Result<String, String> {
    if active_generation != Some(generation) {
        return Err(format!(
            "discarded semantic presentation from stale generation {generation}"
        ));
    }
    if presentations
        .get(&extension)
        .is_some_and(|view| view.extension_instance_id != extension_instance_id)
    {
        presentations.remove(&extension);
    }
    if presentations.get(&extension).is_some_and(|view| {
        view.generation == generation && view.resource_owner.is_some() && resource_owner.is_none()
    }) {
        return Err(
            "discarded process-scoped semantic presentation while owner-scoped state is active"
                .into(),
        );
    }
    if presentations
        .get(&extension)
        .is_some_and(|view| view.resource_owner != resource_owner)
    {
        presentations.remove(&extension);
    }
    if presentations.get(&extension).is_some_and(|view| {
        view.generation > generation
            || (view.generation == generation && view.snapshot.revision >= snapshot.revision)
    }) {
        return Err(format!(
            "discarded stale semantic presentation revision {} for generation {generation}",
            snapshot.revision
        ));
    }
    let compact = snapshot
        .status
        .as_ref()
        .map(|status| status.label.clone())
        .or_else(|| {
            snapshot
                .activities
                .last()
                .map(|activity| activity.summary.clone())
        })
        .unwrap_or_else(|| "presentation updated".to_owned());
    presentations.insert(
        extension.clone(),
        ExtensionPresentationView {
            extension,
            generation,
            extension_instance_id,
            resource_owner,
            snapshot,
        },
    );
    Ok(compact)
}
