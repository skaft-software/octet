//! Process-scoped completion chains and latest-draft native UI queries.
use super::*;

pub(super) struct AutocompleteQuery {
    snapshot: ShellEditorSnapshot,
    fence: AutocompleteFence,
    task: JoinHandle<()>,
}

/// What the input owner already inspected: the composer change counter and the
/// admitted completion-chain count. A frame that changes neither reuses the live
/// query instead of restarting it, so a slow callback is only cancelled by
/// further typing and idle frames never clone the draft.
#[derive(Default)]
pub(super) struct AutocompleteSync {
    revision: Option<u64>,
    providers: usize,
    composer: Option<(String, crate::native_editor::ComposerEditorIdentity)>,
}

impl ExecutableExtensions {
    pub(super) fn cancel_editor_autocomplete(&mut self) {
        if let Some(query) = self.autocomplete_query.take() {
            // Dropping the in-flight RPC sends ordinary request cancellation.
            query.task.abort();
        }
    }

    /// Observe edits from either renderer without awaiting extension code. Keep
    /// one query per live draft; typing/focus/owner changes retire its RPC first.
    pub(crate) fn sync_editor_autocomplete(&mut self, shell: &InteractiveShell) {
        // Ordinary frames resolve through these two counters alone. A fresh
        // draft is compared in full only after the composer moved or a chain
        // was admitted/retired (a first attempt can precede registration).
        let revision = shell.extension_editor_revision();
        let providers = self.autocomplete_registrations.len();
        let composer = self.remote_ui.composer_editor_identity();
        if self.autocomplete_sync.revision == Some(revision)
            && self.autocomplete_sync.providers == providers
            && self.autocomplete_sync.composer == composer
        {
            return;
        }
        self.autocomplete_sync.revision = Some(revision);
        self.autocomplete_sync.providers = providers;
        self.autocomplete_sync.composer = composer.clone();
        let snapshot = shell.extension_editor_snapshot();
        if self.autocomplete_query.as_ref().is_some_and(|query| {
            query.snapshot == snapshot && self.autocomplete_fence_is_current(&query.fence)
        }) {
            return;
        }
        self.cancel_editor_autocomplete();
        if !snapshot.focused
            || (self.remote_ui.editor_owns_composer()
                && composer
                    .as_ref()
                    .is_none_or(|(_, identity)| !identity.registry_context))
        {
            return;
        }
        let line = snapshot.text[..snapshot.cursor]
            .rsplit('\n')
            .next()
            .unwrap_or_default();
        let command = line
            .trim_start()
            .strip_prefix('/')
            .and_then(|line| line.split_once(' '))
            .map(|(command, _)| command);
        if command.is_some_and(|command| self.command_owner(command).is_some()) {
            self.start_editor_autocomplete(snapshot, false);
        }
    }

    fn autocomplete_fence_is_current(&self, fence: &AutocompleteFence) -> bool {
        self.resource_owner.as_deref() == Some(fence.resource_owner.as_str())
            && self.remote_ui.composer_editor_identity() == fence.composer
            && (!self.remote_ui.editor_owns_composer() || fence.composer.is_some())
            && self.session_id == fence.session_id
            && fence.providers.iter().all(|provider| {
                self.autocomplete_registrations
                    .get(&provider.extension)
                    .is_some_and(|registration| {
                        registration.extension_instance_id == provider.extension_instance_id
                            && registration.generation == provider.generation
                            && registration.process.is_running()
                            && registration.process.extension_instance_id()
                                == provider.extension_instance_id
                            && registration.process.health_snapshot().generation
                                == provider.generation
                    })
            })
    }

    /// Remove a displayed menu when its session or registered process is retired.
    pub(crate) fn reconcile_editor_autocomplete(&mut self, shell: &mut InteractiveShell) -> bool {
        self.prune_semantic_ui();
        if self.displayed_autocomplete.as_ref().is_some_and(|fence| {
            !self.autocomplete_fence_is_current(fence) || !shell.extension_editor_snapshot().focused
        }) {
            self.displayed_autocomplete = None;
            return shell.clear_extension_autocomplete();
        }
        false
    }

    /// Correlate a background result again at display, not just RPC completion.
    pub(crate) fn set_editor_autocomplete(
        &mut self,
        shell: &mut InteractiveShell,
        update: ExtensionAutocompleteUpdate,
    ) -> bool {
        self.reconcile_editor_autocomplete(shell);
        if !self.autocomplete_fence_is_current(&update.fence) {
            return false;
        }
        if update.items.is_empty() && !update.path_fallback {
            if shell.extension_editor_snapshot() != update.snapshot {
                return false;
            }
            self.displayed_autocomplete = None;
            return shell.clear_extension_autocomplete();
        }
        let claimed = !update.items.is_empty();
        if !shell.set_extension_autocomplete(&update.snapshot, update.prefix, update.items) {
            return false;
        }
        self.displayed_autocomplete = claimed.then_some(update.fence);
        true
    }

    /// Explicit user acceptance is independently fenced against the live owner.
    pub(crate) fn accept_editor_autocomplete(&mut self, shell: &mut InteractiveShell) -> bool {
        self.reconcile_editor_autocomplete(shell);
        if self.displayed_autocomplete.take().is_none() {
            shell.clear_extension_autocomplete();
            return false;
        }
        let accepted = shell.accept_extension_autocomplete();
        if accepted {
            if let Some(write) = shell.take_composer_slot_write() {
                self.remote_ui.deliver_editor_text(&write, shell);
            }
            self.autocomplete_sync.composer = self.remote_ui.composer_editor_identity();
            // Accepting is not further typing: do not immediately reopen the menu.
            if let Some(query) = self.autocomplete_query.as_mut() {
                query.snapshot = shell.extension_editor_snapshot();
            }
            self.autocomplete_sync.revision = Some(shell.extension_editor_revision());
        }
        accepted
    }

    /// Start one host-mediated autocomplete request for the active editor
    /// snapshot. Display and acceptance retain session/provider correlation.
    pub fn request_editor_autocomplete(&mut self, snapshot: ShellEditorSnapshot) -> bool {
        self.start_editor_autocomplete(snapshot, true)
    }

    fn start_editor_autocomplete(
        &mut self,
        snapshot: ShellEditorSnapshot,
        path_fallback: bool,
    ) -> bool {
        self.prune_semantic_ui();
        self.cancel_editor_autocomplete();
        let Some(resource_owner) = self.resource_owner.clone() else {
            return false;
        };
        if !snapshot.focused
            || !self.remote_ui.composer_editor_matches(&snapshot)
            || (self.remote_ui.editor_owns_composer()
                && self
                    .remote_ui
                    .composer_editor_identity()
                    .is_none_or(|(_, identity)| !identity.registry_context))
        {
            return false;
        }
        let registrations: Vec<_> = self
            .autocomplete_registrations
            .iter()
            .filter(|(_, registration)| {
                registration.process.is_running()
                    && registration.process.health_snapshot().generation == registration.generation
                    && registration.process.extension_instance_id()
                        == registration.extension_instance_id
            })
            .map(|(extension, registration)| {
                (
                    AutocompleteProviderFence {
                        extension: extension.clone(),
                        extension_instance_id: registration.extension_instance_id.clone(),
                        generation: registration.generation,
                    },
                    registration.clone(),
                )
            })
            .collect();
        if registrations.is_empty() {
            return false;
        }
        let mut fence = AutocompleteFence {
            resource_owner,
            session_id: self.session_id.clone(),
            composer: self.remote_ui.composer_editor_identity(),
            providers: registrations
                .iter()
                .map(|(provider, _)| provider.clone())
                .collect(),
        };
        let request = ExtensionAutocompleteRequest {
            text: snapshot.text.clone(),
            cursor: snapshot.cursor,
            revision: snapshot.revision,
        };
        let sender = self.background_tx.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics
                .push("warning: autocomplete requires the Tokio runtime");
            return false;
        };
        let query_snapshot = snapshot.clone();
        let query_fence = fence.clone();
        let task = handle.spawn(async move {
            // One deadline for the whole chain, not N deadlines for N peers.
            let result = tokio::time::timeout(EXTENSION_AUTOCOMPLETE_DEADLINE, async {
                let mut diagnostic = None;
                for (provider, registration) in registrations {
                    let process = registration.process;
                    if !process.is_running()
                        || process.health_snapshot().generation != registration.generation
                    {
                        continue;
                    }
                    match process.request_autocomplete(request.clone()).await {
                        Ok(response)
                            if process.is_running()
                                && process.health_snapshot().generation == registration.generation
                                && !response.items.is_empty() =>
                        {
                            return (Some((response, provider)), diagnostic);
                        }
                        Ok(_) => {}
                        Err(error) => {
                            diagnostic = Some(format!("warning: extension autocomplete failed: {error}"));
                        }
                    }
                }
                (None, diagnostic)
            })
            .await;
            let (response, diagnostic) = match result {
                Ok(result) => result,
                Err(_) => (
                    None,
                    Some(format!(
                        "warning: extension autocomplete exceeded {EXTENSION_AUTOCOMPLETE_DEADLINE:?}"
                    )),
                ),
            };
            // An unclaimed query must preserve normal path completion. The
            // shell applies this empty response only to the exact saved draft,
            // cursor, revision and focus, just like a positive extension menu.
            let (prefix, items) = response.map_or_else(
                || (String::new(), Vec::new()),
                |(response, provider)| {
                    fence.providers = vec![provider];
                    (response.prefix, response.items)
                },
            );
            let update = Some(ExtensionAutocompleteUpdate {
                fence,
                snapshot,
                prefix,
                items,
                path_fallback,
            });
            let _ = sender
                .send(ExtensionBackgroundUpdate::Autocomplete { update, diagnostic })
                .await;
        });
        self.autocomplete_query = Some(AutocompleteQuery {
            snapshot: query_snapshot,
            fence: query_fence,
            task,
        });
        true
    }
}
