//! Steering-only extension of the pooled socket actor; no inference replay.
use super::*;
use crate::steering::{
    ambiguous, invalid, unresolved, Command, Ledger, SteeringState, SteeringUpdate,
};
use std::sync::Mutex as StdMutex;

pub(crate) struct SteeringOperation {
    pub commands: mpsc::Receiver<Command>,
    pub ledger: Ledger,
    pub request: Arc<StdMutex<crate::Request>>,
    pub initial_timeout: Duration,
    pub idle_timeout: Duration,
    pub deadline: Duration,
    pub cancel: oneshot::Receiver<()>,
    pub redactor: crate::auth::CredentialRedactor,
}

async fn publish(reply: &EventSender, update: SteeringUpdate) -> Result<(), AiError> {
    reply
        .send(Ok(
            serde_json::json!({"type":"octet.steer.update", "update":update}),
        ))
        .await
        .map_err(|_| invalid("steering consumer closed"))
}
fn protocol(message: &str) -> AiError {
    crate::StreamProtocolError::UnexpectedEvent(message.to_owned()).into()
}
fn identifier(value: Option<&Value>) -> Result<String, AiError> {
    value
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .map(str::to_owned)
        .ok_or_else(|| protocol("missing or oversized steering identifier"))
}
fn pending(ledger: &Ledger) -> bool {
    ledger
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .any(|entry| unresolved(&entry.update.state))
}
async fn send_json<S>(socket: &mut S, value: Value, timeout: Duration) -> Result<(), AiError>
where
    S: futures_util::Sink<Message, Error = tungstenite::Error> + Unpin,
{
    if bounded_event_size(&value, MAX_WS_MESSAGE_BYTES).is_none() {
        return Err(crate::DecodeError::ResponseTooLarge.into());
    }
    tokio::time::timeout(
        timeout,
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .map_err(|_| {
        transport_error(
            TransportPhase::Body,
            "steering send timed out; acceptance unknown",
        )
    })?
    .map_err(|_| {
        transport_error(
            TransportPhase::Body,
            "steering send failed; acceptance unknown",
        )
    })
}
pub(super) async fn run<S>(
    socket: &mut S,
    command: &RequestCommand,
    mut op: SteeringOperation,
) -> GenerationEnd
where
    S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
        + futures_util::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
    let ledger = op.ledger.clone();
    let deadline = op.deadline;
    let outcome = tokio::select! {
        biased;
        _ = &mut op.cancel => GenerationEnd::Abandoned,
        _ = command.reply.closed() => GenerationEnd::Abandoned,
        result = tokio::time::timeout(deadline, pump(socket, command, &mut op.commands, &op.ledger, &op.request, &op.redactor, op.initial_timeout, op.idle_timeout)) => {
            match result {
                Ok(Ok(None)) => GenerationEnd::Completed,
                Ok(Ok(Some(value))) => GenerationEnd::Forwarded { value },
                Ok(Err(error)) => GenerationEnd::Fatal { error },
                Err(_) => GenerationEnd::Fatal { error: AiError::Transport(TransportError { phase:TransportPhase::Body, timeout:true, message:"steering operation deadline exceeded; provider usage may be unknown".into() }) },
            }
        }
    };
    if !matches!(outcome, GenerationEnd::Completed) {
        ambiguous(&ledger);
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn pump<S>(
    socket: &mut S,
    command: &RequestCommand,
    commands: &mut mpsc::Receiver<Command>,
    ledger: &Ledger,
    request: &Arc<StdMutex<crate::Request>>,
    redactor: &crate::auth::CredentialRedactor,
    initial_timeout: Duration,
    idle_timeout: Duration,
) -> Result<Option<Value>, AiError>
where
    S: futures_core::Stream<Item = Result<Message, tungstenite::Error>>
        + futures_util::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
    let mut current: Option<String> = None;
    let mut active = false;
    let mut waiting_create = true;
    let mut needs_client_input = false;
    let mut successors = HashMap::<String, String>::new();
    let mut seen = HashSet::new();
    let idle = tokio::time::sleep(initial_timeout);
    tokio::pin!(idle);
    let heartbeat = tokio::time::sleep(command.liveness.interval);
    tokio::pin!(heartbeat);
    let ack = tokio::time::sleep(HEARTBEAT_ACK_DEADLINE);
    tokio::pin!(ack);
    let mut expected_pong = None;
    let mut sequence = 0_u64;
    let mut event_count = 0_usize;
    loop {
        // Inspect shared admission, not merely received commands: a terminal
        // racing a locally submitted update must not lose that update.
        if current.is_some() && !active && !waiting_create {
            let entries = ledger.lock().unwrap_or_else(|p| p.into_inner());
            if !entries.iter().any(|entry| unresolved(&entry.update.state)) {
                // Atomic with SteeringControl admission: no queued update may
                // slip between the final pending check and closing the port.
                commands.close();
                return Ok(None);
            }
        }
        let message = tokio::select! {
            _ = &mut idle => return Err(AiError::Transport(TransportError {phase:TransportPhase::Body,timeout:true,message:"steering provider-event idle timeout; usage may be unknown".into()})),
            _ = &mut ack, if expected_pong.is_some() => return Err(AiError::Transport(TransportError {phase:TransportPhase::Body,timeout:true,message:"steering heartbeat acknowledgement timed out; usage may be unknown".into()})),
            _ = &mut heartbeat, if expected_pong.is_none() => {
                sequence += 1;
                let payload: tungstenite::Bytes = sequence.to_be_bytes().to_vec().into();
                tokio::time::timeout(command.liveness.acknowledgement_timeout, socket.send(Message::Ping(payload.clone()))).await
                    .map_err(|_| transport_error(TransportPhase::Body,"steering heartbeat send timeout"))?
                    .map_err(|_| transport_error(TransportPhase::Body,"steering heartbeat send failure"))?;
                expected_pong = Some(payload);
                ack.as_mut().reset(tokio::time::Instant::now()+command.liveness.acknowledgement_timeout);
                continue;
            }
            incoming = commands.recv(), if !commands.is_closed() => {
                match incoming {
                    Some(Command::Steer(local_id)) => {
                        let update = {
                            let entries = ledger.lock().unwrap_or_else(|p| p.into_inner());
                            let entry = &entries[local_id as usize];
                            (!entry.sent).then(|| entry.update.clone())
                        };
                        if let Some(update) = update { publish(&command.reply, update).await?; }
                        if let Some(id) = &current {
                            send_waiting(socket, &command.reply, ledger, id, command.liveness.acknowledgement_timeout).await?;
                        }
                    }
                    Some(Command::Continue { mut body, request: next_request, reply }) => {
                        if active || waiting_create || current.is_none() || !pending(ledger) || !needs_client_input {
                            let _ = reply.send(Err(invalid("steering continuation requires a completed response awaiting client input")));
                            continue;
                        }
                        let object = body.as_object_mut().ok_or_else(|| invalid("continuation body must be an object"))?;
                        object.insert("type".into(), Value::String("response.create".into()));
                        object.insert("previous_response_id".into(), Value::String(current.clone().expect("checked response")));
                        let sent = send_json(socket, body, command.liveness.acknowledgement_timeout).await;
                        if let Err(error) = sent {
                            let _ = reply.send(Err(invalid("continuation send failed; acceptance unknown")));
                            return Err(error);
                        }
                        *request.lock().unwrap_or_else(|p| p.into_inner()) = *next_request;
                        waiting_create = true;
                        let _ = reply.send(Ok(()));
                    }
                    None => {}
                }
                continue;
            }
            message = socket.next() => message,
        };
        match message {
            Some(Ok(Message::Text(text))) => {
                let value = decode_websocket_event(text.as_bytes())?;
                if let Some(value) = process(
                    value,
                    command,
                    ledger,
                    redactor,
                    &mut current,
                    &mut active,
                    &mut waiting_create,
                    &mut needs_client_input,
                    &mut successors,
                    &mut seen,
                    &mut event_count,
                )
                .await?
                {
                    return Ok(Some(value));
                }
                idle.as_mut()
                    .reset(tokio::time::Instant::now() + idle_timeout);
            }
            Some(Ok(Message::Binary(bytes))) => {
                let value = decode_websocket_event(&bytes)?;
                if let Some(value) = process(
                    value,
                    command,
                    ledger,
                    redactor,
                    &mut current,
                    &mut active,
                    &mut waiting_create,
                    &mut needs_client_input,
                    &mut successors,
                    &mut seen,
                    &mut event_count,
                )
                .await?
                {
                    return Ok(Some(value));
                }
                idle.as_mut()
                    .reset(tokio::time::Instant::now() + idle_timeout);
            }
            Some(Ok(Message::Ping(payload))) => {
                tokio::time::timeout(
                    command.liveness.acknowledgement_timeout,
                    socket.send(Message::Pong(payload)),
                )
                .await
                .map_err(|_| transport_error(TransportPhase::Body, "steering pong timeout"))?
                .map_err(|_| transport_error(TransportPhase::Body, "steering pong failure"))?;
            }
            Some(Ok(Message::Pong(payload))) => {
                if expected_pong.as_ref() == Some(&payload) {
                    expected_pong = None;
                    heartbeat
                        .as_mut()
                        .reset(tokio::time::Instant::now() + command.liveness.interval);
                }
            }
            Some(Ok(Message::Frame(_))) => {}
            _ => {
                return Err(transport_error(
                    TransportPhase::Body,
                    "steering socket disconnected; provider work and queued input are ambiguous",
                ))
            }
        }
        if let Some(id) = &current {
            send_waiting(
                socket,
                &command.reply,
                ledger,
                id,
                command.liveness.acknowledgement_timeout,
            )
            .await?;
        }
    }
}

async fn send_waiting<S>(
    socket: &mut S,
    reply: &EventSender,
    ledger: &Ledger,
    current: &str,
    timeout: Duration,
) -> Result<(), AiError>
where
    S: futures_util::Sink<Message, Error = tungstenite::Error> + Unpin,
{
    loop {
        let next = {
            let mut ledger = ledger.lock().unwrap_or_else(|p| p.into_inner());
            ledger
                .iter_mut()
                .find(|entry| {
                    entry.committed
                        && !entry.sent
                        && matches!(entry.update.state, SteeringState::Queued)
                })
                .map(|entry| {
                    entry.sent = true;
                    entry.update.previous_response_id = Some(current.to_owned());
                    (entry.input.clone(), entry.update.clone())
                })
        };
        let Some((input, update)) = next else {
            return Ok(());
        };
        publish(reply, update).await?;
        send_json(socket, serde_json::json!({"type":"response.steer", "previous_response_id":current, "input":input}), timeout).await?;
    }
}

#[allow(clippy::too_many_arguments)]
async fn process(
    value: Value,
    command: &RequestCommand,
    ledger: &Ledger,
    redactor: &crate::auth::CredentialRedactor,
    current: &mut Option<String>,
    active: &mut bool,
    waiting_create: &mut bool,
    needs_client_input: &mut bool,
    successors: &mut HashMap<String, String>,
    seen: &mut HashSet<String>,
    event_count: &mut usize,
) -> Result<Option<Value>, AiError> {
    *event_count += 1;
    if *event_count > 100_000 {
        return Err(crate::DecodeError::ResponseTooLarge.into());
    }
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    if kind.starts_with("octet.") {
        return Err(protocol("reserved steering event type"));
    }
    if kind.starts_with("response.steer.") {
        if bounded_event_size(&value, 64 * 1024).is_none() {
            return Err(crate::DecodeError::ResponseTooLarge.into());
        }
        let steer = value
            .get("steer")
            .ok_or_else(|| protocol("missing steering envelope"))?;
        let id = identifier(steer.get("id"))?;
        let parent = identifier(steer.get("previous_response_id"))?;
        let update = {
            let mut entries = ledger.lock().unwrap_or_else(|p| p.into_inner());
            let index = entries
                .iter()
                .position(|e| e.update.steer_id.as_ref() == Some(&id))
                .or_else(|| {
                    entries.iter().position(|e| {
                        e.sent
                            && e.update.steer_id.is_none()
                            && e.update.previous_response_id.as_ref() == Some(&parent)
                            && (kind != "response.steer.failed"
                                || steer.get("input").and_then(Value::as_str)
                                    == Some(e.input.as_str()))
                    })
                })
                .ok_or_else(|| protocol("unmatched steering acknowledgement"))?;
            let entry = &mut entries[index];
            if entry.update.previous_response_id.as_ref() != Some(&parent)
                || !unresolved(&entry.update.state)
            {
                return Err(protocol("out-of-order steering acknowledgement"));
            }
            match kind {
                "response.steer.accepted" => {
                    if entry.update.steer_id.is_some() {
                        return Err(protocol("duplicate steering acceptance"));
                    }
                    entry.update.state = successors
                        .get(&parent)
                        .map(|id| SteeringState::Applied {
                            response_id: id.clone(),
                        })
                        .unwrap_or(SteeringState::Accepted);
                }
                "response.steer.pending" => {
                    if entry.update.steer_id.is_none() {
                        return Err(protocol("pending steering was not accepted"));
                    }
                    *needs_client_input = true;
                    entry.update.state = SteeringState::Pending {
                        required_input: value.get("required_input").cloned().unwrap_or(Value::Null),
                    };
                }
                "response.steer.failed" => {
                    // Error messages and echoed user input are deliberately not
                    // diagnostics. Only a bounded machine code crosses the API.
                    let code = value
                        .pointer("/error/code")
                        .and_then(Value::as_str)
                        .filter(|code| {
                            code.len() <= 128
                                && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        })
                        .map(|code| redactor.redact(code));
                    entry.update.state = SteeringState::Failed { code };
                }
                _ => return Err(protocol("unknown steering acknowledgement")),
            }
            entry.update.steer_id = Some(id);
            entry.update.clone()
        };
        publish(&command.reply, update).await?;
        return Ok(None);
    }
    if kind == "response.created" {
        if *active {
            return Err(protocol("overlapping steering response segments"));
        }
        let id = identifier(value.pointer("/response/id"))?;
        if seen.len() > crate::steering::MAX_STEERS || !seen.insert(id.clone()) {
            return Err(protocol(
                "duplicate or excessive steering response segments",
            ));
        }
        let mut updates = Vec::new();
        if let Some(parent) = current.as_ref() {
            successors.insert(parent.clone(), id.clone());
            let mut entries = ledger.lock().unwrap_or_else(|p| p.into_inner());
            for entry in entries.iter_mut() {
                if entry.update.previous_response_id.as_ref() == Some(parent)
                    && matches!(
                        entry.update.state,
                        SteeringState::Accepted | SteeringState::Pending { .. }
                    )
                {
                    entry.update.state = SteeringState::Applied {
                        response_id: id.clone(),
                    };
                    updates.push(entry.update.clone());
                }
            }
        }
        *current = Some(id);
        *active = true;
        *waiting_create = false;
        *needs_client_input = false;
        for update in updates {
            publish(&command.reply, update).await?;
        }
    } else if kind == "error" {
        return Ok(Some(value));
    } else if !*active {
        return Err(protocol("response event outside a steering segment"));
    }
    if let Some(id) = value
        .pointer("/response/id")
        .or_else(|| value.get("response_id"))
        .and_then(Value::as_str)
    {
        if current.as_deref() != Some(id) {
            return Err(protocol("steering event response id mismatch"));
        }
    }
    if terminal_kind(&value).is_some() {
        *active = false;
        *needs_client_input = value
            .pointer("/response/output")
            .and_then(Value::as_array)
            .is_some_and(|output| {
                output
                    .iter()
                    .any(|item| match item.get("type").and_then(Value::as_str) {
                        Some("function_call" | "custom_tool_call" | "computer_call") => {
                            item.get("async") != Some(&Value::Bool(true))
                        }
                        Some("mcp_approval_request") => true,
                        _ => false,
                    })
            });
        // Same failure provenance and retire-before-publication ordering as
        // ordinary WS. `steered` alone is an additional successful boundary.
        if failed_terminal(&value)
            && value
                .pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str)
                != Some("steered")
        {
            return Ok(Some(value));
        }
    }
    command
        .reply
        .send(Ok(value))
        .await
        .map_err(|_| invalid("steering consumer closed"))?;
    Ok(None)
}
