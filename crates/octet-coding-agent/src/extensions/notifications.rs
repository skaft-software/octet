//! ExecutableExtensions lifecycle notifications sent to every extension.

use super::*;

impl ExecutableExtensions {
    /// Fan one host lifecycle notification out to every live process that
    /// negotiated `feature`. Failures stay bounded diagnostics, never panics.
    pub(super) fn notify_lifecycle_v2(
        &mut self,
        feature: &str,
        notify: impl Fn(&ExtensionProcess) -> Result<(), String>,
    ) {
        let mut failures = Vec::new();
        for process in &self.processes {
            if !process.supports_feature(feature) {
                continue;
            }
            if let Err(error) = notify(process) {
                failures.push(format!(
                    "warning: {}: lifecycle notification failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
        for failure in failures {
            self.diagnostics.push(failure);
        }
    }

    /// Announce the start of one host compaction boundary.
    pub fn notify_compaction_started_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_compaction_started()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce the settled state of one host compaction boundary.
    pub fn notify_compaction_settled_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_compaction_settled()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce a failed host compaction boundary with a bounded reason.
    pub fn notify_compaction_failed_all(&mut self, reason: &str) {
        let reason = bounded_notification_reason(reason);
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_compaction_failed(reason)
                .map_err(|error| error.to_string())
        });
    }

    /// Announce that the foreground model selection changed.
    pub fn notify_model_selected_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_model_selected()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce that the foreground reasoning selection changed.
    pub fn notify_reasoning_selected_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_reasoning_selected()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce that the foreground session metadata changed.
    pub fn notify_session_info_changed_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_session_info_changed()
                .map_err(|error| error.to_string())
        });
    }

    /// Fan out a real custom-message commit through the negotiated lifecycle channel.
    pub fn notify_custom_message_committed_all(
        &mut self,
        entry_id: &octet_agent::session::EntryId,
        message: &octet_agent::session::CustomMessage,
        timestamp_unix_ms: u64,
    ) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_custom_message_committed(entry_id, message, timestamp_unix_ms)
                .map_err(|error| error.to_string())
        });
    }

    /// Announce the first streamed increment of one assistant message.
    pub fn notify_message_started_all(&mut self, message_id: &str) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_message_started(message_id)
                .map_err(|error| error.to_string())
        });
    }

    /// Forward one streamed assistant increment to the host coalescer, which
    /// owns every batching and flush decision.
    pub fn push_message_delta_all(&mut self, delta: &str) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .push_message_delta(delta)
                .map_err(|error| error.to_string())
        });
    }

    /// Close one assistant message boundary after flushing coalesced deltas.
    pub fn notify_message_settled_all(&mut self, message_id: &str) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_message_settled(message_id)
                .map_err(|error| error.to_string())
        });
    }

    /// Announce one executed user `!`/`!!` shell escape with bounded text.
    pub fn notify_user_bash_all(&mut self, command: &str) {
        let command = bounded_notification_command(command);
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_user_bash(command)
                .map_err(|error| error.to_string())
        });
    }
}
