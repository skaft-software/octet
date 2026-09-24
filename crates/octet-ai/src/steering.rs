//! Explicit, multi-response OpenAI Responses steering sessions.
//!
//! Each response keeps its own guarded stream, output, usage and cost. Acceptance
//! only queues user input. Neither cancellation nor a transport error proves
//! that provider work stopped or that unknown usage is zero. Never automatically
//! replay accepted or ambiguous steering on another connection.

use crate::{AiError, ConfigError, Model, Request, StreamEvent};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

pub(crate) const MAX_STEERS: usize = 64;
const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Outcome of one locally identified user update.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SteeringState {
    /// Locally queued; not evidence of provider acceptance.
    Queued,
    /// Provider accepted the input into its connection-local queue.
    Accepted,
    /// Input was included in a successor (not proof of semantic compliance).
    Applied {
        /// Successor response carrying the update.
        response_id: String,
    },
    /// Provider needs client tool results or approval on the same connection.
    Pending {
        /// Bounded provider description of required input, not execution authority.
        required_input: serde_json::Value,
    },
    /// Provider explicitly rejected the input; it will not apply automatically.
    Failed {
        /// Sanitized provider error code, if supplied.
        code: Option<String>,
    },
    /// Local cancellation/disconnection left the outcome unknown. Do not replay.
    Ambiguous,
}

/// Ordered steering evidence. Persist the corresponding user input separately.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SteeringUpdate {
    /// Monotonically increasing local submission identifier.
    pub local_id: u64,
    /// Provider acceptance identifier, when acknowledged.
    pub steer_id: Option<String>,
    /// Response targeted on the live connection.
    pub previous_response_id: Option<String>,
    /// Latest known state (acceptance is not application).
    pub state: SteeringState,
}

/// One operation event; canonical response boundaries are never flattened.
#[derive(Debug)]
pub enum SteeringEvent {
    /// A canonical event belonging to exactly one independently billed response.
    Response {
        /// Provider response identifier.
        response_id: String,
        /// Ordinary guarded response event. Persist every `Finished` response.
        event: StreamEvent,
    },
    /// A steering state transition.
    Steer(SteeringUpdate),
}

pub(crate) struct Submission {
    pub update: SteeringUpdate,
    pub input: String,
    pub sent: bool,
    pub committed: bool,
}
pub(crate) type Ledger = Arc<Mutex<Vec<Submission>>>;
pub(crate) enum Command {
    Steer(u64),
    Continue {
        body: serde_json::Value,
        request: Box<Request>,
        reply: oneshot::Sender<Result<(), AiError>>,
    },
}
pub(crate) fn invalid(message: &str) -> AiError {
    ConfigError::Parse(message.to_owned()).into()
}
pub(crate) fn unresolved(state: &SteeringState) -> bool {
    matches!(
        state,
        SteeringState::Queued | SteeringState::Accepted | SteeringState::Pending { .. }
    )
}
pub(crate) fn ambiguous(ledger: &Ledger) -> Vec<SteeringUpdate> {
    let mut ledger = ledger.lock().unwrap_or_else(|p| p.into_inner());
    for entry in ledger.iter_mut() {
        if unresolved(&entry.update.state) {
            entry.update.state = if entry.committed {
                SteeringState::Ambiguous
            } else {
                SteeringState::Failed {
                    code: Some("not_submitted".into()),
                }
            };
        }
    }
    ledger.iter().map(|entry| entry.update.clone()).collect()
}

/// A reserved local steering receipt that has NOT been dispatched.
///
/// Persist `local_id()` and the user input, then consume this with
/// [`SteeringControl::commit_steer`]. Dropping it abandons the reservation and
/// never sends `response.steer`. Prepared receipts must be committed in order.
pub struct PreparedSteer {
    local_id: u64,
    control: SteeringControl,
    committed: bool,
}
impl PreparedSteer {
    /// Stable operation-local id available before any socket submission.
    pub fn local_id(&self) -> u64 {
        self.local_id
    }
}
impl Drop for PreparedSteer {
    fn drop(&mut self) {
        if !self.committed {
            let mut ledger = self
                .control
                .ledger
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let entry = &mut ledger[self.local_id as usize];
            if !entry.committed && unresolved(&entry.update.state) {
                entry.update.state = SteeringState::Failed {
                    code: Some("not_submitted".into()),
                };
            }
            let _ = self.control.sender.try_send(Command::Steer(self.local_id));
        }
    }
}

/// Cloneable bounded command port. Commands are selected alongside socket reads.
#[derive(Clone)]
pub struct SteeringControl {
    pub(crate) sender: mpsc::Sender<Command>,
    pub(crate) ledger: Ledger,
    pub(crate) model: Model,
    pub(crate) completed: Arc<Mutex<Option<crate::AssistantMessage>>>,
}
impl SteeringControl {
    /// Queues user text and returns its local id, not provider acceptance.
    ///
    /// Persist the user input durably BEFORE calling this method. The returned
    /// local id can then be attached to that existing durable record. Admission
    /// may dispatch before this future returns; it is not a persistence barrier.
    /// The actor waits for `response.created` and uses the current response id.
    /// At most 64 submissions and 64 KiB per input are admitted per operation.
    pub async fn steer(&self, input: String) -> Result<u64, AiError> {
        let prepared = self.prepare_steer(input)?;
        self.commit_steer(prepared).await
    }

    /// Reserves a receipt without dispatch. Persist input and `local_id` before
    /// calling `commit_steer`. No socket frame can be sent for this reservation
    /// before commit, even when provider events arrive concurrently.
    pub fn prepare_steer(&self, input: String) -> Result<PreparedSteer, AiError> {
        if input.is_empty() || input.len() > MAX_INPUT_BYTES {
            return Err(invalid("steering input must contain 1..=65536 UTF-8 bytes"));
        }
        let mut ledger = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        if self.sender.is_closed() {
            return Err(invalid("steering operation is closed"));
        }
        if ledger.len() >= MAX_STEERS {
            return Err(invalid("steering operation submission limit reached"));
        }
        let local_id = ledger.len() as u64;
        ledger.push(Submission {
            update: SteeringUpdate {
                local_id,
                steer_id: None,
                previous_response_id: None,
                state: SteeringState::Queued,
            },
            input,
            sent: false,
            committed: false,
        });
        Ok(PreparedSteer {
            local_id,
            control: self.clone(),
            committed: false,
        })
    }

    /// Commits a durably recorded reservation to the bounded socket queue.
    /// Admission is not provider acceptance. Commit receipts in local-id order;
    /// rejected/dropped receipts are never sent or implicitly retried.
    pub async fn commit_steer(&self, mut prepared: PreparedSteer) -> Result<u64, AiError> {
        if !Arc::ptr_eq(&self.ledger, &prepared.control.ledger) {
            return Err(invalid("steering receipt belongs to another operation"));
        }
        let mut ledger = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        let index = prepared.local_id as usize;
        if ledger[..index]
            .iter()
            .any(|entry| !entry.committed && unresolved(&entry.update.state))
        {
            return Err(invalid("commit steering receipts in local-id order"));
        }
        if !matches!(ledger[index].update.state, SteeringState::Queued) {
            return Err(invalid("steering reservation is no longer open"));
        }
        self.sender
            .try_send(Command::Steer(prepared.local_id))
            .map_err(|_| invalid("steering command queue is full or closed"))?;
        ledger[index].committed = true;
        prepared.committed = true;
        Ok(prepared.local_id)
    }

    /// Returns required client input on the same socket, without resending steering.
    ///
    /// Supply only new input, not full history. This explicit response uses its
    /// own tools, instructions and generation settings. The codec validates the
    /// request; the actor supplies the completed response's `previous_response_id`.
    pub async fn continue_with(&self, mut request: Request) -> Result<(), AiError> {
        let completed = self
            .completed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| {
                invalid("consume a completed response before returning required input")
            })?;
        // Validate canonical result pairing against the actual completed segment,
        // while transmitting only the new items. Never manufacture a tool call
        // from caller-supplied output or replay the accepted steering.
        let delta = request
            .responses
            .as_ref()
            .and_then(|options| options.input.clone())
            .unwrap_or_else(|| {
                crate::responses::encode_canonical_responses_input(
                    &self.model,
                    request.system.as_deref(),
                    &request.messages,
                    request.compatibility,
                )
            });
        request
            .messages
            .insert(0, crate::Message::Assistant(completed));
        let options = request.responses.get_or_insert_with(Default::default);
        options.input = Some(delta);
        options.previous_response_id = None;
        crate::json_repair::validate_tool_definitions(&request.tools).map_err(AiError::Decode)?;
        let parts = crate::protocol::openai_responses::build_request(&self.model, &request)?;
        request.messages.remove(0);
        if parts.body.len() > 64 * 1024 * 1024 {
            return Err(crate::DecodeError::ResponseTooLarge.into());
        }
        let mut body: serde_json::Value = serde_json::from_slice(&parts.body)
            .map_err(|_| invalid("invalid steering continuation body"))?;
        if let Some(body) = body.as_object_mut() {
            body.remove("stream");
            body.remove("background");
        }
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Command::Continue {
                body,
                request: Box::new(request),
                reply,
            })
            .map_err(|_| invalid("steering command queue is full or closed"))?;
        result
            .await
            .map_err(|_| invalid("steering connection closed; continuation acceptance unknown"))?
    }
}

/// One cancellable multi-response operation. Dropping it stops local socket work.
///
/// Errors and cancellation after opening require host-owned usage uncertainty;
/// known `Finished` response usage remains a separate, exact subtotal.
pub struct SteeringSession {
    pub(crate) control: SteeringControl,
    pub(crate) events:
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<SteeringEvent, AiError>> + Send>>,
    pub(crate) cancel: Option<oneshot::Sender<()>>,
}
impl SteeringSession {
    /// Returns a cloneable command port for concurrent mid-generation updates.
    pub fn control(&self) -> SteeringControl {
        self.control.clone()
    }
    /// Receives the next segment event or steering transition.
    pub async fn next_event(&mut self) -> Option<Result<SteeringEvent, AiError>> {
        let event = self.events.next().await;
        if matches!(&event, Some(Err(_))) {
            // A parser/guard failure also abandons provider work immediately;
            // callers need not remember to drop the session after an error.
            self.cancel();
        }
        event
    }
    /// Returns bounded, submission-ordered evidence, including errors/disconnects.
    pub fn steering_updates(&self) -> Vec<SteeringUpdate> {
        self.control
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|entry| entry.update.clone())
            .collect()
    }
    /// Cancels local work and marks unresolved steering ambiguous.
    /// This is not remote cancellation or evidence of zero provider usage.
    pub fn cancel(&mut self) -> Vec<SteeringUpdate> {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        self.events = Box::pin(futures_util::stream::empty());
        ambiguous(&self.control.ledger)
    }
}
impl Drop for SteeringSession {
    fn drop(&mut self) {
        self.cancel();
    }
}
