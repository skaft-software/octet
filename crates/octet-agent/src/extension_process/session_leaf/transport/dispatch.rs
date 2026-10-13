//! Wire admission owns transfer handles; publication never owns append authority.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareAck {
    accepted: bool,
    transfer_id: String,
    view_revision: u64,
    head: Option<String>,
    sha256: String,
}

fn validate_prepare_ack(value: Value, expected: &Descriptor) -> Result<(), ExtensionRuntimeError> {
    let ack: PrepareAck = serde_json::from_value(value).map_err(|_| unavailable())?;
    if !ack.accepted
        || ack.transfer_id != expected.transfer_id
        || ack.view_revision != expected.view_revision
        || ack.head != expected.head
        || ack.sha256 != expected.sha256
    {
        return Err(unavailable());
    }
    Ok(())
}

pub(in crate::extension_process) struct RequestViews {
    mailbox: Arc<SessionLeafMailbox>,
    parent: u64,
}
impl Drop for RequestViews {
    fn drop(&mut self) {
        lock_std_mutex(&self.mailbox.transport).settle(self.parent);
    }
}

pub(in crate::extension_process) struct Staged {
    mailbox: Arc<SessionLeafMailbox>,
    pub(in crate::extension_process) descriptor: Descriptor,
}
impl Drop for Staged {
    fn drop(&mut self) {
        lock_std_mutex(&self.mailbox.transport)
            .transfers
            .remove(&self.descriptor.transfer_id);
    }
}
pub(in crate::extension_process) fn stage(
    connection: &ProcessConnection,
    view: Arc<View>,
) -> Result<Staged, ExtensionRuntimeError> {
    let profile = connection.session_profile()?;
    let descriptor =
        lock_std_mutex(&connection.session_leaf.transport).transfer(0, view, &profile)?;
    Ok(Staged {
        mailbox: Arc::clone(&connection.session_leaf),
        descriptor,
    })
}

pub(in crate::extension_process) fn attach(
    connection: &ProcessConnection,
    id: u64,
    method: &str,
    owner: Option<&ExtensionResourceOwner>,
    params: &mut Value,
) -> Result<Option<RequestViews>, ExtensionRuntimeError> {
    if !owner_routes_enabled(&read_std_lock(&connection.protocol)) {
        return Ok(None);
    }
    let Some(owner) = owner else {
        return Ok(None);
    };
    let profile = connection.session_profile()?;
    let guard = RequestViews {
        mailbox: Arc::clone(&connection.session_leaf),
        parent: id,
    };
    let mut store = lock_std_mutex(&connection.session_leaf.transport);
    let field = if params.get("snapshot").is_some() {
        "snapshot"
    } else {
        "session_snapshot"
    };
    // History is attached only to dispatches whose callbacks can read session
    // state: hooks (which already carry an invocation snapshot), commands,
    // shortcuts, tool calls and argument preparation. Presentation-only
    // requests (transcript render, UI frames, tool render, provider pipeline)
    // must never pay a document read; their retained facts stay available
    // through the peer's own prepared view exactly like the legacy mirror.
    let wants_session = matches!(
        method,
        methods::HOOK_RUN
            | methods::COMMAND_EXECUTE
            | methods::SHORTCUT_EXECUTE
            | methods::TOOL_CALL
            | "tool/prepare_arguments"
    );
    let view = if let Some(descriptor) = params.get(field) {
        let descriptor: Descriptor =
            serde_json::from_value(descriptor.clone()).map_err(|_| unavailable())?;
        let transfer = store
            .transfers
            .get_mut(&descriptor.transfer_id)
            .ok_or_else(unavailable)?;
        if transfer.parent != 0 || &transfer.view.descriptor.owner != owner {
            return Err(unavailable());
        }
        transfer.parent = id;
        Some(Arc::clone(&transfer.view))
    } else if wants_session {
        match store.current.get(owner) {
            Some(Some(view)) => Some(Arc::clone(view)),
            Some(None) => return Err(unavailable()),
            None => None,
        }
    } else {
        None
    };
    if let Some(view) = view {
        if params.get(field).is_none() {
            params[field] =
                serde_json::to_value(store.transfer(id, Arc::clone(&view), &profile)?)
                    .map_err(|_| unavailable())?;
        }
        let preparation: Option<Preparation> =
            serde_json::from_value(params[field]["preparation"].clone())
                .map_err(|_| unavailable())?;
        store.parents.insert(id, (owner.clone(), preparation));
        if let Some(payload) = params.as_object_mut().and_then(|p| p.remove("payload")) {
            let invocation = store.invocation(&payload, &view, &profile)?;
            let mut descriptor = store.transfer(id, invocation, &profile)?;
            descriptor.preparation = serde_json::from_value(params[field]["preparation"].clone())
                .map_err(|_| unavailable())?;
            params["session_payload"] =
                serde_json::to_value(descriptor).map_err(|_| unavailable())?;
        }
    }
    // Never run Drop while holding the store lock (including error paths).
    drop(store);
    Ok(Some(guard))
}

pub(in crate::extension_process) fn publish(
    process: &ExtensionProcess,
    connection: Arc<ProcessConnection>,
    state: ExtensionHostState,
    session: &crate::Session,
    owner: ExtensionResourceOwner,
) -> Result<(), ExtensionRuntimeError> {
    let publication_id = connection
        .session_leaf
        .begin_snapshot_publication(owner.clone());
    let result = (|| -> Result<(), ExtensionRuntimeError> {
        let profile = connection.session_profile()?;
        let result = {
            let mut store = lock_std_mutex(&connection.session_leaf.transport);
            // Invalidate before preparing, including failure. No head/count reuse.
            if !store.current.contains_key(&owner) && store.current.len() >= profile.owner_views {
                return Err(unavailable());
            }
            // Reserve the exact published revision before advertising anything, so
            // the barrier and the document it announces can never disagree.
            let revision = store.next_revision()?;
            connection.session_leaf.set_snapshot_publication_revision(
                &owner,
                publication_id,
                revision,
            );
            store.current.insert(owner.clone(), None);
            // Publish the newest-wins unavailable barrier even if encoding fails.
            // No UI/foreground authority is needed for read-only owner invalidation.
            if !connection.queue_notification(methods::CONTEXT_UPDATED, json!({"resource_owner":owner,"host":{
                "session_view_revision":revision,"session_entries":null,"session_branch":null,"session_leaf_id":null,"session_file":null,"session_header":null,"session_labels":null
            }})) { return Err(unavailable()); }
            let result = store.history(
                session,
                &process.descriptor().manifest.name,
                owner.clone(),
                &profile,
                None,
                revision,
            );
            if let Ok(view) = &result {
                store.current.insert(owner.clone(), Some(Arc::clone(view)));
            }
            result
        };
        let view = result?;
        // Identity is transport-independent: the paired profile keeps the exact
        // owner fence and the same counted session facts in the mirror slot that
        // admitted setup/replacement receipts read synchronously, while hooks and
        // reads travel through parent-bound chunk handles. No legacy whole-frame
        // legacy snapshot is built for it.
        let facts = super::host_facts(&view)?;
        let (changed, retired_owner) = {
            let mut slot = lock_std_mutex(&connection.session_leaf.mirror);
            let mirror = SessionMirror {
                owner: owner.clone(),
                value: Some(facts),
            };
            let changed = slot.as_ref() != Some(&mirror);
            let retired_owner = slot
                .as_ref()
                .filter(|previous| previous.owner.session_id != owner.session_id)
                .map(|previous| previous.owner.session_id.clone());
            *slot = Some(mirror);
            (changed, retired_owner)
        };
        process.set_host_state_on_connection(
            state.clone(),
            Arc::clone(&connection),
            changed,
            true,
            retired_owner,
        );
        let staged = stage(&connection, Arc::clone(&view))?;
        if !lock_std_mutex(&connection.session_leaf.transport)
            .current
            .get(&owner)
            .and_then(Option::as_ref)
            .is_some_and(|current| Arc::ptr_eq(current, &view))
        {
            // A newer publication superseded this document before it was sent: the
            // peer is already invalidated and the newest publication prepares.
            return Ok(());
        }
        let first =
            lock_std_mutex(&connection.publications).start(&owner, staged, state, publication_id);
        let Some((staged, state, publication_id)) = first else {
            // Both acknowledged-publication lanes are busy; this document is now the
            // newest queued publication instead of another request slot.
            return Ok(());
        };
        let publication_connection = Arc::clone(&connection);
        let publication_owner = owner.clone();
        let publication_process = process.clone();
        tokio::spawn(async move {
            let mut next = Some((staged, state, publication_id));
            while let Some((staged, state, publication_id)) = next {
                let revision = staged.descriptor.view_revision;
                let ack = if !publication_connection.closed.load(Ordering::Acquire)
                    && !publication_connection.draining.load(Ordering::Acquire)
                {
                    let expected = staged.descriptor.clone();
                    let params = json!({"resource_owner":publication_owner,"snapshot":staged.descriptor,"host":state});
                    publication_process
                        .request_typed_on_connection(
                            Arc::clone(&publication_connection),
                            PREPARE,
                            &params,
                            Some(publication_owner.clone()),
                        )
                        .await
                        .and_then(|value| validate_prepare_ack(value, &expected))
                } else {
                    Err(ExtensionRuntimeError::Closed(
                        "session snapshot publication generation retired".into(),
                    ))
                };
                publication_connection
                    .session_leaf
                    .finish_snapshot_publication(
                        &publication_owner,
                        publication_id,
                        revision,
                        ack.is_ok(),
                    );
                next =
                    lock_std_mutex(&publication_connection.publications).finish(&publication_owner);
            }
        });
        Ok(())
    })();
    if result.is_err() {
        connection
            .session_leaf
            .fail_snapshot_publication(&owner, publication_id);
    }
    result
}

/// Bounded host-state publication: a publication burst spends at most two
/// acknowledged `session/snapshot/prepare` requests per owner, and every newer
/// document replaces the queued one instead of adding another request. Request
/// capacity belongs to the peer's own work: an unbounded publication fan-out
/// made an unrelated `hook/run` or `command/execute` fail with
/// `session_request_quota_exceeded` while preparations piled up.
///
/// Two lanes, not one, are deliberate. A superseded preparation can still be
/// reading its document, and the newest document must not wait for it: the peer
/// answers a foreground request from its newest installed view, so a newest
/// preparation delayed behind a superseded one is a real availability window.
/// Ordering stays newest-wins because the peer refuses any preparation at or
/// below the revision its newest barrier already announced, and this host
/// invalidates before it prepares.
#[derive(Default)]
pub(in crate::extension_process) struct PublicationPump {
    lanes: HashMap<ExtensionResourceOwner, usize>,
    queued: HashMap<ExtensionResourceOwner, (Staged, ExtensionHostState, u64)>,
}

/// Simultaneously acknowledged preparations per owner. The peer's own work must
/// keep the rest of its negotiated request quota.
const PUBLICATION_LANES: usize = 2;

impl PublicationPump {
    /// Claims a lane for `owner`. `None` means both lanes are busy and this
    /// publication is now the newest queued document.
    fn start(
        &mut self,
        owner: &ExtensionResourceOwner,
        staged: Staged,
        state: ExtensionHostState,
        publication_id: u64,
    ) -> Option<(Staged, ExtensionHostState, u64)> {
        let lanes = self.lanes.entry(owner.clone()).or_default();
        if *lanes < PUBLICATION_LANES {
            *lanes += 1;
            Some((staged, state, publication_id))
        } else {
            self.queued
                .insert(owner.clone(), (staged, state, publication_id));
            None
        }
    }

    /// Releases a settled lane and hands it the newest queued document, or drops
    /// the owner once the peer has seen the newest publication.
    fn finish(
        &mut self,
        owner: &ExtensionResourceOwner,
    ) -> Option<(Staged, ExtensionHostState, u64)> {
        match self.queued.remove(owner) {
            Some(next) => Some(next),
            None => {
                self.lanes.remove(owner);
                None
            }
        }
    }

    /// A draining generation owns no future publication; the acknowledged
    /// requests settle their own lanes so no owner is left permanently pinned.
    pub(in crate::extension_process) fn clear_queued(&mut self) {
        self.queued.clear();
    }
}
