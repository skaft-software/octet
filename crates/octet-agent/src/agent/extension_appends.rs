//! Command appends cross into the Run that owns the sole mutable Session.
use super::*;
use crate::extension_process::{
    ExtensionProcess, ExtensionRequestFailure, ExtensionRequestId, ExtensionRequestOutcome,
};
use std::future::Future;

struct Request {
    process: ExtensionProcess,
    id: ExtensionRequestId,
    generation: u64,
    entry_type: String,
    data: serde_json::Value,
}

#[derive(Clone)]
pub(crate) struct RunAppends {
    shared: Arc<Mutex<State>>,
    ready: Arc<tokio::sync::Notify>,
    start: usize,
    pub(crate) usage_start: usize,
}

struct State {
    open: bool,
    requests: VecDeque<Request>,
    // A bounded proof of writes made by this lane, not an authority inferred
    // from session length/head alone. Active callbacks may chain past these
    // exact receipts without weakening their original writer/source fence.
    committed: VecDeque<KnownAppend>,
}

struct KnownAppend {
    before: (Option<EntryId>, u64),
    after: (Option<EntryId>, u64),
    entry: crate::session::Entry,
}

fn source(session: &Session) -> Option<(Option<EntryId>, u64)> {
    Some((
        session.head(),
        session.try_clone_file().ok()?.metadata().ok()?.len(),
    ))
}

impl RunAppends {
    pub(super) fn new(start: usize, usage_start: usize) -> Self {
        Self {
            shared: Arc::new(Mutex::new(State {
                open: true,
                requests: VecDeque::new(),
                committed: VecDeque::new(),
            })),
            ready: Arc::new(tokio::sync::Notify::new()),
            start,
            usage_start,
        }
    }

    pub(crate) fn ready(&self) -> impl Future<Output = ()> + Send + 'static {
        let lane = self.clone();
        async move {
            loop {
                let notified = lane.ready.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !lane.shared.lock().unwrap().requests.is_empty() {
                    return;
                }
                notified.await;
            }
        }
    }

    pub(crate) fn service(&self, session: &mut Session) -> bool {
        let request = self.shared.lock().unwrap().requests.pop_front();
        let Some(request) = request else { return false };
        let before = source(session);
        let result = request.process.append_run_session_entry_request(
            &request.id,
            request.generation,
            session,
            &request.entry_type,
            request.data,
            self.start,
        );
        if result.is_ok() {
            if let (Some(before), Some(after)) = (before, source(session)) {
                let entry = session
                    .entry(after.0.as_ref().expect("committed head"))
                    .expect("committed entry")
                    .clone();
                let mut state = self.shared.lock().unwrap();
                if state.committed.len() == 16 {
                    state.committed.pop_front();
                }
                state.committed.push_back(KnownAppend {
                    before,
                    after,
                    entry,
                });
            }
        }
        if let Err((failure, message)) = result {
            respond(
                request.process,
                request.id,
                request.generation,
                failure,
                message,
            );
        }
        true
    }

    fn gap(
        &self,
        session: &Session,
        head: Option<&EntryId>,
        bytes: u64,
    ) -> Option<Vec<crate::session::Entry>> {
        let current = source(session)?;
        let mut previous = (head.cloned(), bytes);
        let state = self.shared.lock().unwrap();
        let first = state
            .committed
            .iter()
            .position(|entry| entry.before == previous)?;
        let mut gap = Vec::new();
        for entry in state.committed.iter().skip(first) {
            if entry.before != previous {
                return None;
            }
            gap.push(entry.entry.clone());
            previous = entry.after.clone();
            if previous == current {
                return Some(gap);
            }
        }
        None
    }

    pub(super) fn close(&self) {
        let requests = {
            let mut state = self.shared.lock().unwrap();
            state.open = false;
            std::mem::take(&mut state.requests)
        };
        for request in requests {
            respond(
                request.process,
                request.id,
                request.generation,
                ExtensionRequestFailure::NotForegroundOwner,
                "response writer settled before append admission".into(),
            );
        }
    }
}

fn respond(
    process: ExtensionProcess,
    id: ExtensionRequestId,
    generation: u64,
    failure: ExtensionRequestFailure,
    message: String,
) {
    // This task delivers only a terminal refusal, never persistence or replay.
    tokio::spawn(async move {
        let _ = process
            .respond_to_extension_request(
                id,
                generation,
                ExtensionRequestOutcome::Failed(failure, message),
            )
            .await;
    });
}

impl Run<'_> {
    /// Route an already admitted command append to this Run's original writer.
    /// Queue admission is not a durable receipt; keep polling the Run to commit.
    pub fn queue_extension_append(
        &self,
        process: ExtensionProcess,
        id: ExtensionRequestId,
        generation: u64,
        entry_type: String,
        data: serde_json::Value,
    ) -> Result<(), (ExtensionRequestFailure, String)> {
        crate::session_leaf::payload_bytes(&entry_type, &data).map_err(|_| {
            (
                ExtensionRequestFailure::BoundsExceeded,
                "response append exceeds private entry bounds".into(),
            )
        })?;
        process.validate_run_session_append(&id, generation, &self.append_owner)?;
        let mut state = self.appends.shared.lock().unwrap();
        if !state.open || state.requests.len() >= 64 {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                "response append lane unavailable or full".into(),
            ));
        }
        state.requests.push_back(Request {
            process,
            id,
            generation,
            entry_type,
            data,
        });
        self.appends.ready.notify_one();
        Ok(())
    }
}

impl Session {
    pub(crate) fn known_run_append_gap(
        &self,
        head: Option<&EntryId>,
        bytes: u64,
    ) -> Option<Vec<crate::session::Entry>> {
        self.run_appends.as_ref()?.gap(self, head, bytes)
    }

    pub(crate) fn run_append_ready(&self) -> impl Future<Output = ()> + Send + 'static {
        let lane = self.run_appends.clone();
        async move {
            match lane {
                Some(lane) => lane.ready().await,
                None => std::future::pending().await,
            }
        }
    }

    pub(crate) fn service_run_append(&mut self) -> bool {
        self.run_appends
            .clone()
            .is_some_and(|lane| lane.service(self))
    }
}
