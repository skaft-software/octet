//! Turn lifecycle, pending host interactions, terminal grants and semantic UI state.

use super::*;

pub struct ExtensionTurnLifecycle {
    pub(super) processes: Vec<ExtensionProcess>,
    pub(super) resource_owner: String,
    pub(super) session_id: String,
    pub(super) run_id: String,
    pub(super) turn_id: String,
    pub(super) started_at: Instant,
    pub(super) started_delivery: watch::Receiver<bool>,
    pub(super) settled: bool,
    #[cfg(test)]
    pub(super) lifecycle_delivery_test_control:
        Option<std::sync::Arc<LifecycleDeliveryTestControl>>,
}

/// Per-instance cancellation barrier used only by lifecycle ownership tests.
/// Keeping this on the owning `ExecutableExtensions` avoids global hooks and
/// lets the rest of the test suite continue to run in parallel.
#[cfg(test)]
#[derive(Default)]
pub(super) struct LifecycleDeliveryTestControl {
    pub(super) gate_turn_started: std::sync::atomic::AtomicBool,
    pub(super) turn_started_entered: tokio::sync::Notify,
    pub(super) turn_started_release: tokio::sync::Notify,
    pub(super) gate_turn_settled: std::sync::atomic::AtomicBool,
    pub(super) turn_settled_entered: tokio::sync::Notify,
    pub(super) turn_settled_release: tokio::sync::Notify,
}

#[cfg(test)]
impl LifecycleDeliveryTestControl {
    /// Only the lifecycle-delivery suite drives these barriers, and that suite
    /// runs real extension processes, so it exists on unix builds only. The
    /// hook itself stays everywhere because the product code that calls
    /// `wait_before_delivery` is not platform-specific.
    #[cfg(unix)]
    pub(super) fn gate_turn_started(&self) {
        self.gate_turn_started.store(true, Ordering::Release);
    }

    #[cfg(unix)]
    pub(super) fn release_turn_started(&self) {
        self.turn_started_release.notify_one();
    }

    #[cfg(unix)]
    pub(super) async fn turn_started_entered(&self) {
        self.turn_started_entered.notified().await;
    }

    #[cfg(unix)]
    pub(super) fn gate_turn_settled(&self) {
        self.gate_turn_settled.store(true, Ordering::Release);
    }

    #[cfg(unix)]
    pub(super) fn release_turn_settled(&self) {
        self.turn_settled_release.notify_one();
    }

    #[cfg(unix)]
    pub(super) async fn turn_settled_entered(&self) {
        self.turn_settled_entered.notified().await;
    }

    pub(super) async fn wait_before_delivery(&self, event: &ExtensionLifecycleEvent) {
        match event {
            ExtensionLifecycleEvent::TurnStarted { .. }
                if self.gate_turn_started.load(Ordering::Acquire) =>
            {
                self.turn_started_entered.notify_one();
                self.turn_started_release.notified().await;
            }
            ExtensionLifecycleEvent::TurnSettled { .. }
                if self.gate_turn_settled.load(Ordering::Acquire) =>
            {
                self.turn_settled_entered.notify_one();
                self.turn_settled_release.notified().await;
            }
            _ => {}
        }
    }
}

impl ExtensionTurnLifecycle {
    pub(super) async fn settle(
        mut self,
        outcome: ExtensionLifecycleOutcome,
        reason: Option<String>,
    ) -> Vec<String> {
        let processes = self.processes.clone();
        let resource_owner = self.resource_owner.clone();
        let turn_id = self.turn_id.clone();
        let event = ExtensionLifecycleEvent::TurnSettled {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            turn_id: self.turn_id.clone(),
            outcome,
            duration_ms: duration_millis(self.started_at.elapsed()),
            reason,
        };
        #[cfg(test)]
        let lifecycle_delivery_test_control = self.lifecycle_delivery_test_control.clone();
        // Transfer terminal delivery to an owned task before this future can
        // be cancelled. Dropping the JoinHandle detaches rather than aborts
        // the task, so every admitted turn retains one terminal owner.
        let delivery = tokio::spawn(async move {
            #[cfg(test)]
            if let Some(control) = lifecycle_delivery_test_control {
                control.wait_before_delivery(&event).await;
            }
            let diagnostics = notify_lifecycle_all(&processes, event).await;
            for process in &processes {
                process.clear_active_lifecycle_turn(&resource_owner, &turn_id);
            }
            diagnostics
        });
        self.settled = true;
        match delivery.await {
            Ok(diagnostics) => diagnostics,
            Err(error) => vec![format!(
                "warning: extension turn lifecycle task failed: {error}"
            )],
        }
    }
}

impl Drop for ExtensionTurnLifecycle {
    fn drop(&mut self) {
        if self.settled || self.processes.is_empty() {
            return;
        }
        let Ok(handle) = Handle::try_current() else {
            for process in &self.processes {
                process.clear_active_lifecycle_turn(&self.resource_owner, &self.turn_id);
            }
            return;
        };
        let processes = self.processes.clone();
        let resource_owner = self.resource_owner.clone();
        let turn_id = self.turn_id.clone();
        let mut started_delivery = self.started_delivery.clone();
        let event = ExtensionLifecycleEvent::TurnSettled {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            turn_id: self.turn_id.clone(),
            outcome: ExtensionLifecycleOutcome::FrontendDisconnected,
            duration_ms: duration_millis(self.started_at.elapsed()),
            reason: Some("turn owner dropped before explicit settlement".into()),
        };
        #[cfg(test)]
        let lifecycle_delivery_test_control = self.lifecycle_delivery_test_control.clone();
        handle.spawn(async move {
            let _ = started_delivery.wait_for(|started| *started).await;
            #[cfg(test)]
            if let Some(control) = lifecycle_delivery_test_control {
                control.wait_before_delivery(&event).await;
            }
            let _ = notify_lifecycle_all(&processes, event).await;
            for process in &processes {
                process.clear_active_lifecycle_turn(&resource_owner, &turn_id);
            }
        });
    }
}

pub(super) struct PendingConfirmationDenial {
    pub(super) process: ExtensionProcess,
    pub(super) request_id: ExtensionRequestId,
    pub(super) generation: u64,
}

pub(super) struct PendingInputCancellation {
    pub(super) process: ExtensionProcess,
    pub(super) request_id: ExtensionRequestId,
    pub(super) generation: u64,
}

pub(super) struct PendingEditorRequest {
    pub(super) process: ExtensionProcess,
    pub(super) request_id: ExtensionRequestId,
    pub(super) generation: u64,
    pub(super) request: ExtensionEditorRequest,
}

/// One admitted extension request that a host surface still has to answer.
/// Every entry is answered exactly once, or dropped with a bounded diagnostic
/// when its process generation is gone.
pub(super) struct PendingHostRequest {
    pub(super) process: ExtensionProcess,
    pub(super) request_id: ExtensionRequestId,
    pub(super) generation: u64,
    pub(super) operation: HostRequestOperation,
}

impl PendingHostRequest {
    pub(super) fn discard_notice(&self) -> Option<String> {
        (!self.process.is_running() || self.process.health_snapshot().generation != self.generation)
            .then(|| {
                format!(
                    "warning: {}: discarded host-owned extension request from stale generation {}",
                    self.process.descriptor().manifest.name,
                    self.generation
                )
            })
    }
}

/// Which foreground authority one event drain may exercise when it admits a
/// reverse host request.
///
/// Pi resolves `ctx.*` calls against the live session context at any time
/// (`createContext` is gated only by `assertActive()`), so an interactive
/// frontend that owns the session must queue requests its shell will service
/// instead of refusing them as if no foreground session existed. A host with no
/// interactive consumer keeps refusing: nothing would ever answer that queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundAdmission {
    /// No interactive consumer is bound to this session.
    Absent,
    /// An interactive consumer owns the session but is not servicing the shell
    /// in this drain (a hook or tool boundary before the next shell pump).
    Queued,
    /// The live interactive shell is servicing requests in this drain.
    Shell,
}

impl ForegroundAdmission {
    /// True when a foreground frontend exists and will service queued requests.
    pub(super) fn services_later(self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// True when the live interactive shell is pumping in this drain; only then
    /// may a drain grant shell-owned authority such as an editor lease.
    pub(super) fn shell(self) -> bool {
        matches!(self, Self::Shell)
    }
}

/// Host-mediated operations an extension may request. Each one is gated on a
/// negotiated additive feature and on foreground resource ownership.
pub(super) enum HostRequestOperation {
    Composer(ExtensionComposerOperation),
    SessionEntry(ExtensionSessionEntryOperation),
    MessageInjection(ExtensionMessageInjection),
    Shortcut {
        shortcut_id: String,
        key: String,
        description: String,
    },
    ActiveTools {
        names: Vec<String>,
    },
    Terminal(ExtensionTerminalOperation),
    RemoteUi {
        owner: ExtensionResourceOwner,
        operation: ExtensionRemoteUiOperation,
    },
    /// A read-only foreground context snapshot. `SystemPrompt` is resolved by
    /// the product loop that owns the agent; the other operations resolve
    /// against the live shell and the cached host state.
    ContextSnapshot(ExtensionContextOperation),
}

/// Identity of the caller that owns, or wants, the foreground terminal grant.
///
/// The grant is fenced by foreground resource owner, extension instance and
/// process generation, so a reload, a restarted process or a switched session
/// can never inherit or release someone else's granted tty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TerminalHolder {
    /// Host-derived foreground resource owner, or process scope when absent.
    pub(super) owner: Option<String>,
    /// Extension instance that owns the holder process.
    pub(super) instance_id: String,
    /// Process generation admitted for this grant.
    pub(super) generation: u64,
    /// Manifest name, used only for bounded diagnostics.
    pub(super) name: String,
}

/// One live foreground terminal grant. The host keeps the record so it can
/// always recognise the holder, and always take the terminal back.
pub(super) struct ActiveTerminalGrant {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain the exact issued grant ID with its owner for terminal grant diagnostics."
        )
    )]
    pub(super) grant_id: String,
    pub(super) holder: TerminalHolder,
}

/// Single-slot arbiter for the one foreground raw terminal the host can cede.
///
/// The terminal stays host-owned: a second acquire is refused while a grant is
/// live, only the recorded holder can release it, and every refusal is typed so
/// no request is ever silently dropped. Revocation never waits on the previous
/// holder.
#[derive(Default)]
pub(super) struct TerminalGrantArbiter {
    pub(super) active: Option<ActiveTerminalGrant>,
}

impl TerminalGrantArbiter {
    pub(super) fn active(&self) -> Option<&ActiveTerminalGrant> {
        self.active.as_ref()
    }

    /// Admit one acquire. The host mints the grant id and reports the size it
    /// left the terminal in; it never trusts a child-supplied identity.
    pub(super) fn acquire(
        &mut self,
        holder: TerminalHolder,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalAcquireResult, (ExtensionRequestFailure, String)> {
        if let Some(active) = self.active.as_ref() {
            return Err((
                ExtensionRequestFailure::InvalidRequest,
                format!(
                    "the foreground terminal is already granted to {}",
                    active.holder.name
                ),
            ));
        }
        let grant_id = mint_terminal_grant_id(&holder.instance_id);
        self.active = Some(ActiveTerminalGrant {
            grant_id: grant_id.clone(),
            holder,
        });
        Ok(TerminalAcquireResult {
            grant_id,
            columns,
            rows,
        })
    }

    /// Admit one release from the current holder only.
    pub(super) fn release(
        &mut self,
        holder: &TerminalHolder,
    ) -> Result<(), (ExtensionRequestFailure, String)> {
        let Some(active) = self.active.as_ref() else {
            return Err((
                ExtensionRequestFailure::InvalidRequest,
                "no foreground terminal grant is active".to_owned(),
            ));
        };
        if &active.holder != holder {
            return Err((
                ExtensionRequestFailure::InvalidRequest,
                format!(
                    "{} does not hold the active foreground terminal grant",
                    holder.name
                ),
            ));
        }
        self.active = None;
        Ok(())
    }

    /// Take the live grant when its holder stopped being valid.
    pub(super) fn revoke_if(
        &mut self,
        still_valid: impl FnOnce(&TerminalHolder) -> bool,
    ) -> Option<ActiveTerminalGrant> {
        let stale = {
            let active = self.active.as_ref()?;
            !still_valid(&active.holder)
        };
        if stale {
            self.active.take()
        } else {
            None
        }
    }
}

/// Mint a bounded, host-owned grant identifier. The monotonic sequence keeps
/// ids unique across grants handed to one long-lived extension instance.
pub(super) fn mint_terminal_grant_id(instance_id: &str) -> String {
    static TERMINAL_GRANT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = TERMINAL_GRANT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let candidate = format!("terminal-grant-{sequence:016x}-{instance_id}");
    let mut bounded = String::new();
    for character in candidate.chars() {
        if bounded.len() + character.len_utf8() > MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

/// A keymap binding registered at runtime by a live extension generation.
#[derive(Clone)]
pub(super) struct RegisteredDynamicShortcut {
    pub(super) extension: String,
    pub(super) shortcut_id: String,
    pub(super) key: ExtensionShortcutKey,
    pub(super) description: String,
    pub(super) process: ExtensionProcess,
    pub(super) generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditorStateDelivery {
    pub(super) state: ExtensionEditorResponse,
    pub(super) generations: BTreeMap<String, (u64, String)>,
}

#[derive(Clone)]
pub(super) struct SemanticUiStatus {
    pub(super) text: String,
    pub(super) style_role: Option<String>,
    pub(super) priority: i32,
}

#[derive(Clone)]
pub(super) struct SemanticUiWidget {
    pub(super) lines: Vec<String>,
    pub(super) placement: ExtensionWidgetPlacement,
    pub(super) style_role: Option<String>,
    pub(super) priority: i32,
}

#[derive(Default)]
pub(super) struct SemanticUiView {
    pub(super) extension_instance_id: String,
    pub(super) generation: u64,
    pub(super) statuses: BTreeMap<String, SemanticUiStatus>,
    pub(super) widgets: BTreeMap<String, SemanticUiWidget>,
    /// The extension-owned header surface, or `None` while it is cleared.
    pub(super) header: Option<SemanticUiStatus>,
    /// The extension-owned footer surface, or `None` while it is cleared.
    pub(super) footer: Option<SemanticUiStatus>,
    pub(super) working: Option<ShellExtensionWorking>,
    pub(super) hidden_thinking_label: Option<String>,
}

#[derive(Clone)]
pub(super) struct RegisteredAutocomplete {
    pub(super) process: ExtensionProcess,
    pub(super) generation: u64,
    pub(super) extension_instance_id: String,
}

#[derive(Clone)]
pub(super) struct AutocompleteProviderFence {
    pub(super) extension: String,
    pub(super) extension_instance_id: String,
    pub(super) generation: u64,
}

#[derive(Clone)]
pub(super) struct AutocompleteFence {
    pub(super) resource_owner: String,
    pub(super) session_id: Option<String>,
    // One winning provider, or every queried provider for unclaimed fallback.
    /// A host-owned component handle/revision, in addition to the composer
    /// mirror and provider/session fences. None means the ordinary host editor.
    pub(super) composer: Option<(String, crate::native_editor::ComposerEditorIdentity)>,
    pub(super) providers: Vec<AutocompleteProviderFence>,
}

#[derive(Clone)]
pub(crate) struct ExtensionAutocompleteUpdate {
    pub(super) fence: AutocompleteFence,
    pub(crate) snapshot: ShellEditorSnapshot,
    pub(crate) prefix: String,
    pub(crate) items: Vec<ShellAutocompleteItem>,
    /// Only explicit Tab queries may fall through to native path insertion.
    pub(super) path_fallback: bool,
}

pub struct ExtensionToolRenderUpdate {
    pub id: ToolCallId,
    pub segments: Vec<ToolRenderSegment>,
}

#[derive(Default)]
pub struct ExtensionBackgroundUpdates {
    pub rendered_tools: Vec<ExtensionToolRenderUpdate>,
    pub(crate) autocomplete: Vec<ExtensionAutocompleteUpdate>,
    /// Completed shortcut output, ready for the interactive transcript.
    pub shortcut_messages: Vec<String>,
}

pub(super) enum ExtensionBackgroundUpdate {
    Diagnostics(Vec<String>),
    Renderer {
        update: Option<ExtensionToolRenderUpdate>,
        diagnostic: Option<String>,
    },
    Autocomplete {
        update: Option<ExtensionAutocompleteUpdate>,
        diagnostic: Option<String>,
    },
    Shortcut {
        extension: String,
        context: Vec<ContextContribution>,
        messages: Vec<String>,
    },
}

pub(super) fn opaque_extension_resource_id(name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    format!("extension:{digest:x}")
}

/// Keep the experimental transport switch at the process-owner boundary.
///
/// Manifest and MCP configuration are both data loaded before the extension
/// process starts. Strip any copy supplied through that data, then add the
/// argument only when this octet process received its one-shot CLI opt-in.
pub(super) fn apply_experimental_streamable_http_mcp_gate(
    descriptor: &mut DiscoveredExtension,
    enabled: bool,
) {
    if descriptor.manifest.name != MCP_EXTENSION_NAME {
        return;
    }
    descriptor
        .manifest
        .entrypoint
        .args
        .retain(|argument| argument != EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT);
    if enabled {
        descriptor
            .manifest
            .entrypoint
            .args
            .push(EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT.to_owned());
    }
}
