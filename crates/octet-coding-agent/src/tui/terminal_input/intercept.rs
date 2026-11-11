//! One cancellation-safe, bounded pre-native lane shared by every input consumer.
//!
//! Pi 1.0.2 hands each decoded key string to its live input-listener `Set` and
//! only then to the focused component. Octet keeps the host-reserved grammar
//! and open slash popup ahead of that chain; listeners then run in registration
//! order and can consume or replace editor input. One deliberate bound remains:
//! a listener that does not answer inside
//! [`INPUT_BUDGET`] is latched off and its input is delivered unchanged, because
//! Pi would block the input loop indefinitely and octet must never let a stuck
//! extension freeze typing. A listener that *reports* an error is only recorded
//! once and keeps its subscription, matching Pi's habit of letting the chain
//! continue while a broken callback cannot silently rewrite input.

use super::*;
use octet_agent::extension_process::{
    ExtensionProcess, ExtensionResourceOwner, ExtensionRuntimeError,
    MAX_EXTENSION_TERMINAL_INPUT_BYTES,
};
use std::sync::Mutex;

/// Whole chain budget for one input event. Pi has no deadline at all.
const INPUT_BUDGET: Duration = Duration::from_millis(50);

#[derive(Default)]
struct Circuit {
    /// Latched after a timeout or transport/protocol failure: pipelining more
    /// events into a process that already failed to answer would stall typing.
    latched_off: AtomicBool,
    /// One diagnostic per owner, so a listener that always throws cannot spam.
    reported: AtomicBool,
}

impl Circuit {
    fn disable(&self, client: &Client, reason: impl std::fmt::Display) {
        if !self.latched_off.swap(true, Ordering::AcqRel) {
            client.handle.report(format!(
                "warning: {}: terminal input interception disabled until reload: {reason}",
                client.process.descriptor().manifest.name
            ));
        }
    }

    fn report_once(&self, client: &Client, reason: impl std::fmt::Display) {
        if !self.reported.swap(true, Ordering::AcqRel) {
            client.handle.report(format!(
                "warning: {}: terminal input listener failed: {reason}",
                client.process.descriptor().manifest.name
            ));
        }
    }
}

#[derive(Clone)]
struct Client {
    process: ExtensionProcess,
    owner: ExtensionResourceOwner,
    handle: InputInterceptors,
    circuit: Arc<Circuit>,
}

type HostPolicy = Arc<dyn Fn(&Event) -> bool + Send + Sync>;

#[derive(Default)]
struct Bindings {
    revision: u64,
    clients: Vec<Client>,
    diagnostics: Vec<String>,
    host_policy: Option<HostPolicy>,
}

/// Shared binding table. The frontend installs the live extension processes and
/// the active session owner; the input stream consults it for every key.
#[derive(Clone, Default)]
pub(crate) struct InputInterceptors(Arc<Mutex<Bindings>>);

impl InputInterceptors {
    /// Native reserved actions and an open slash popup precede all consumers.
    pub(crate) fn set_host_policy(&self, policy: impl Fn(&Event) -> bool + Send + Sync + 'static) {
        self.0.lock().unwrap().host_policy = Some(Arc::new(policy));
    }

    fn host_reserves(&self, event: &Event) -> bool {
        let policy = self.0.lock().unwrap().host_policy.clone();
        policy.is_some_and(|policy| policy(event))
    }

    /// Rebind the owner and processes, returning any diagnostics accumulated
    /// while typing. Circuits survive rebinding so a latched-off process is not
    /// retried for every keystroke; a changed owner or process set is a new
    /// generation whose held input must never be delivered to the new owner.
    pub(crate) fn bind(&self, processes: &[ExtensionProcess], owner: Option<&str>) -> Vec<String> {
        let mut state = self.0.lock().unwrap();
        let mut clients = Vec::new();
        if let Some(owner) = owner {
            for process in processes
                .iter()
                .filter(|process| process.supports_terminal_input_interception())
            {
                let Some(owner) = process
                    .current_context_for_resource_owner(owner)
                    .resource_owner
                else {
                    continue;
                };
                if !process.resource_owner_is_live(&owner) {
                    continue;
                }
                let circuit = state
                    .clients
                    .iter()
                    .find(|client| client.owner == owner)
                    .map(|client| client.circuit.clone())
                    .unwrap_or_default();
                clients.push(Client {
                    process: process.clone(),
                    owner,
                    handle: self.clone(),
                    circuit,
                });
            }
        }
        let same = state
            .clients
            .iter()
            .map(|client| &client.owner)
            .eq(clients.iter().map(|client| &client.owner));
        if !same {
            state.revision += 1;
        }
        state.clients = clients;
        std::mem::take(&mut state.diagnostics)
    }

    fn report(&self, message: String) {
        self.0.lock().unwrap().diagnostics.push(message);
    }

    /// Clients that still accept input, with the revision they belong to.
    fn active(&self) -> (u64, Vec<Client>) {
        let state = self.0.lock().unwrap();
        let clients = state
            .clients
            .iter()
            .filter(|client| !client.circuit.latched_off.load(Ordering::Acquire))
            .cloned()
            .collect();
        (state.revision, clients)
    }

    fn revision(&self) -> u64 {
        self.0.lock().unwrap().revision
    }

    /// The existing wire bound keeps oversized input out of the lane.
    fn intercepted(raw: Option<&str>) -> Option<&str> {
        raw.filter(|raw| raw.len() <= MAX_EXTENSION_TERMINAL_INPUT_BYTES)
    }
}

fn is_host_reserved(event: &Event) -> bool {
    matches!(event, Event::Key(key) if crate::tui::keymap::is_close_key(key))
}

type InputFuture = Pin<Box<dyn Future<Output = Vec<Event>> + Send>>;

/// The single in-flight input event, retained by the stream so a cancelled
/// `next()` (a `select!` branch losing) can never lose or duplicate a key.
#[derive(Default)]
pub(super) struct PendingInput {
    pub(super) handle: InputInterceptors,
    pending: Option<(u64, InputFuture)>,
    ready: VecDeque<Event>,
    ready_revision: u64,
}

impl PendingInput {
    pub(super) fn start(&mut self, packet: Packet) {
        let (revision, clients) = self.handle.active();
        self.ready_revision = revision;
        let raw = (!(is_host_reserved(&packet.event) || self.handle.host_reserves(&packet.event)))
            .then(|| InputInterceptors::intercepted(packet.raw.as_deref()))
            .flatten()
            .filter(|_| !clients.is_empty())
            .map(str::to_owned);
        let Some(raw) = raw else {
            self.ready.push_back(packet.event);
            return;
        };
        self.pending = Some((
            revision,
            Box::pin(async move {
                let deadline = tokio::time::Instant::now() + INPUT_BUDGET;
                // `None` means no listener answered; the original event is then
                // delivered unchanged instead of an invented one.
                let mut admitted: Option<(String, Vec<Event>)> = None;
                for client in clients {
                    if admitted.as_ref().is_some_and(|(data, _)| data.is_empty()) {
                        break;
                    }
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    let requested = admitted
                        .as_ref()
                        .map_or(raw.as_str(), |(data, _)| data.as_str());
                    match client
                        .process
                        .intercept_terminal_input(requested, &client.owner, remaining)
                        .await
                    {
                        Ok(next) => match codec::replacement(&next) {
                            // Admitting the reply is also the provenance check: a
                            // listener cannot inject a filtered protocol reply or a
                            // half-finished escape as user input.
                            Ok(events) => admitted = Some((next, events)),
                            Err(error) => client.circuit.disable(&client, error),
                        },
                        Err(ExtensionRuntimeError::Remote { code, message, .. }) => client
                            .circuit
                            .report_once(&client, format!("{code}: {message}")),
                        Err(error) => client.circuit.disable(&client, error),
                    }
                }
                match admitted {
                    Some((data, _)) if data == raw => vec![packet.event],
                    Some((_, events)) => events,
                    None => vec![packet.event],
                }
            }),
        ));
    }

    /// `None` means this lane has nothing to deliver; the caller keeps reading.
    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Option<Poll<Event>> {
        if self.ready_revision != self.handle.revision() {
            // Retirement discards held input; it is never replayed to a new owner.
            self.ready.clear();
        }
        if let Some((revision, future)) = &mut self.pending {
            if *revision != self.handle.revision() {
                self.pending = None;
            } else {
                match future.as_mut().poll(cx) {
                    Poll::Pending => return Some(Poll::Pending),
                    Poll::Ready(events) => {
                        self.ready.extend(events);
                        self.pending = None;
                    }
                }
            }
        }
        self.ready.pop_front().map(Poll::Ready)
    }
}
