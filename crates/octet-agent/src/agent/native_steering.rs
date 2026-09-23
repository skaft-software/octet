//! Native multi-response ownership, durable steering admission, and ordered replay.
use super::*;
use octet_ai::{SteeringControl, SteeringSession, SteeringState, SteeringUpdate};

pub(super) enum ProviderStream {
    Ordinary(octet_ai::ResponseStream),
    Native(
        SteeringSession,
        mpsc::Sender<SteeringUpdate>,
        Option<String>,
    ),
}
impl ProviderStream {
    pub(super) fn control(&self) -> Option<SteeringControl> {
        match self {
            Self::Native(session, _, _) => Some(session.control()),
            _ => None,
        }
    }
    pub(super) async fn next(&mut self) -> Option<Result<StreamEvent, AiError>> {
        match self {
            Self::Ordinary(stream) => stream.next().await,
            Self::Native(session, updates, current_response) => loop {
                match session.next_event().await? {
                    Ok(octet_ai::SteeringEvent::Response { event, response_id }) => {
                        *current_response = Some(response_id);
                        return Some(Ok(event));
                    }
                    Ok(octet_ai::SteeringEvent::Steer(update)) => {
                        if updates.try_send(update).is_err() {
                            return Some(Err(AiError::Config(octet_ai::ConfigError::Parse(
                                "native steering update queue exceeded its bound".into(),
                            ))));
                        }
                    }
                    Err(error) => return Some(Err(error)),
                }
            },
        }
    }
    pub(super) fn response_id(&self) -> Option<String> {
        match self {
            Self::Native(_, _, id) => id.clone(),
            Self::Ordinary(_) => None,
        }
    }
    pub(super) fn into_native(self) -> Option<SteeringSession> {
        match self {
            Self::Native(session, _, _) => Some(session),
            _ => None,
        }
    }
}

struct PendingInput {
    id: u64,
    payload: ReservedPayload,
    applied: Option<String>,
}
#[derive(Default)]
pub(super) struct NativeState {
    pub(super) connection: Option<SteeringSession>,
    pub(super) control: Option<SteeringControl>,
    operation: String,
    pending: Vec<PendingInput>,
    materialized: Vec<(u64, String)>,
    current_response: Option<String>,
    prefix_complete: bool,
    pub(super) required_input: bool,
}
impl NativeState {
    pub(super) fn begin(&mut self, operation: String, control: SteeringControl) {
        self.operation = operation;
        self.control = Some(control);
        self.prefix_complete = false;
    }
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub(super) fn completed_prefix(&mut self) {
        self.prefix_complete = true;
    }
    pub(super) fn started(&mut self, response_id: Option<String>) {
        self.prefix_complete = false;
        self.current_response = response_id;
    }

    pub(super) async fn submit(
        &mut self,
        input: ReservedInput,
        session: &mut Session,
        model: &Model,
    ) -> Result<Option<ReservedInput>, AgentError> {
        let text = match &input {
            ReservedInput::Ready(payload) => text_input(&payload.input),
            ReservedInput::Retractable(prepared) => prepared
                .receipt
                .payload
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .and_then(|payload| text_input(&payload.input)),
        };
        let Some(text) = text else {
            return Ok(Some(input));
        };
        let Some(control) = self.control.as_ref() else {
            return Ok(Some(input));
        };
        let prepared = control.prepare_steer(text)?;
        let Some(payload) = input.claim() else {
            return Ok(None);
        };
        let id = prepared.local_id();
        session.append(EntryValue::ResponsesSteering {
            endpoint: model.endpoint.id.clone(),
            model: model.spec.id.clone(),
            operation: self.operation.clone(),
            local_id: id,
            input: Some(UserMessage {
                content: payload.input.clone().into_user_parts(),
            }),
            state: None,
            completed: None,
        })?;
        // Durable non-context intent exists before dispatch. Actual user replay
        // is appended only after the completed prefix and provider application.
        self.pending.push(PendingInput {
            id,
            payload,
            applied: None,
        });
        control.commit_steer(prepared).await?;
        Ok(None)
    }
    pub(super) fn update(
        &mut self,
        update: SteeringUpdate,
        session: &mut Session,
        model: &Model,
    ) -> Result<(), AgentError> {
        session.append(EntryValue::ResponsesSteering {
            endpoint: model.endpoint.id.clone(),
            model: model.spec.id.clone(),
            operation: self.operation.clone(),
            local_id: update.local_id,
            input: None,
            state: Some(update.clone()),
            completed: None,
        })?;
        match update.state {
            SteeringState::Applied { response_id } => {
                if let Some(pending) = self
                    .pending
                    .iter_mut()
                    .find(|pending| pending.id == update.local_id)
                {
                    pending.applied = Some(response_id);
                }
            }
            SteeringState::Pending { .. } => self.required_input = true,
            SteeringState::Failed { .. } | SteeringState::Ambiguous => {
                return Err(AiError::Config(octet_ai::ConfigError::Parse("native steering failed or became ambiguous; input retained without automatic replay".into())).into());
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn deliver(
        &mut self,
        session: &mut Session,
        metadata: &EntryMetadata,
        evidence: &mut Option<TerminalGateEvidence>,
    ) -> Result<Option<AgentEvent>, AgentError> {
        let mut delivered = Vec::new();
        while self.pending.first().is_some_and(|pending| {
            pending.applied.as_ref().is_some_and(|response_id| {
                self.prefix_complete || self.current_response.as_ref() == Some(response_id)
            })
        }) {
            let pending = &self.pending[0];
            if self.prefix_complete && pending.applied == self.current_response {
                return Err(AiError::Config(octet_ai::ConfigError::Parse(
                    "native steering application arrived after its successor was committed; input retained without replay".into(),
                )).into());
            }
            let text = text_input(&pending.payload.input).expect("native text admission");
            let mut metadata = metadata.clone();
            metadata.native_steering = Some((self.operation.clone(), pending.id));
            session.append_with_metadata(
                user_message(pending.payload.input.clone()),
                Some(metadata),
            )?;
            self.materialized.push((
                pending.id,
                pending.applied.clone().expect("applied successor"),
            ));
            if let Some(evidence) = evidence {
                evidence.record_request(&text);
            }
            delivered.push(text);
            self.pending.remove(0);
        }
        Ok(
            (!delivered.is_empty()).then_some(AgentEvent::SteeringDelivered {
                messages: delivered,
            }),
        )
    }
    pub(super) fn settle_successor(
        &mut self,
        session: &mut Session,
        model: &Model,
        assistant: EntryId,
    ) -> Result<(), AgentError> {
        let mut index = 0;
        while index < self.materialized.len() {
            let (local_id, response_id) = &self.materialized[index];
            if self.current_response.as_ref() != Some(response_id) {
                index += 1;
                continue;
            }
            session.append(EntryValue::ResponsesSteering {
                endpoint: model.endpoint.id.clone(),
                model: model.spec.id.clone(),
                operation: self.operation.clone(),
                local_id: *local_id,
                input: None,
                state: None,
                completed: Some(assistant.clone()),
            })?;
            self.materialized.remove(index);
        }
        Ok(())
    }

    pub(super) fn cancel(
        &mut self,
        session: &mut Session,
        model: &Model,
    ) -> Result<(), AgentError> {
        if let Some(connection) = self.connection.as_mut() {
            for update in connection.cancel() {
                session.append(EntryValue::ResponsesSteering {
                    endpoint: model.endpoint.id.clone(),
                    model: model.spec.id.clone(),
                    operation: self.operation.clone(),
                    local_id: update.local_id,
                    input: None,
                    state: Some(update),
                    completed: None,
                })?;
            }
        }
        self.connection = None;
        self.control = None;
        Ok(())
    }
}
fn text_input(input: &UserInput) -> Option<String> {
    let mut text = String::new();
    for part in &input.parts {
        let InputPart::Text(part) = part else {
            return None;
        };
        text.push_str(part);
    }
    (!text.is_empty() && text.len() <= 65536).then_some(text)
}

pub(super) fn required_input_request(
    mut request: Request,
    session: &Session,
    model: &Model,
) -> Result<Request, AgentError> {
    let mut messages = Vec::new();
    for entry in active_branch_entries(session).into_iter().rev() {
        match &entry.value {
            EntryValue::Message(Message::Assistant(_)) => break,
            EntryValue::Message(Message::User(user))
                if user
                    .content
                    .iter()
                    .any(|part| matches!(part, UserPart::ToolResult(_))) =>
            {
                messages.push(user.clone())
            }
            _ => {}
        }
    }
    messages.reverse();
    let replay = messages
        .iter()
        .cloned()
        .map(ResponsesReplayItem::User)
        .collect::<Vec<_>>();
    let tier = request
        .responses
        .as_ref()
        .and_then(|options| options.service_tier);
    request.responses = Some(ResponsesOptions::full_replay(
        octet_ai::responses::encode_responses_replay(model, None, &replay)?,
    ));
    if let Some(options) = request.responses.as_mut() {
        options.service_tier = tier;
    }
    request.messages = messages.into_iter().map(Message::User).collect();
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_after_committed_successor_fails_closed_without_reordering() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("late.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-6-astra".into()))
            .unwrap();
        let input = UserInput::from("correction");
        session
            .append(EntryValue::ResponsesSteering {
                endpoint: model.endpoint.id.clone(),
                model: model.spec.id.clone(),
                operation: "run:late".into(),
                local_id: 0,
                state: None,
                completed: None,
                input: Some(UserMessage {
                    content: input.clone().into_user_parts(),
                }),
            })
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
                content: vec![AssistantPart::Text("already committed successor".into())],
            })))
            .unwrap();
        let mut native = NativeState {
            operation: "run:late".into(),
            current_response: Some("successor".into()),
            prefix_complete: true,
            pending: vec![PendingInput {
                id: 0,
                payload: ReservedPayload {
                    input,
                    reservation: None,
                },
                applied: None,
            }],
            ..Default::default()
        };
        native
            .update(
                SteeringUpdate {
                    local_id: 0,
                    steer_id: Some("s1".into()),
                    previous_response_id: Some("prefix".into()),
                    state: SteeringState::Applied {
                        response_id: "successor".into(),
                    },
                },
                &mut session,
                &model,
            )
            .unwrap();
        let count = session.entries().len();
        assert!(native
            .deliver(&mut session, &EntryMetadata::default(), &mut None)
            .is_err());
        assert_eq!(session.entries().len(), count);
        assert!(session.has_unsettled_native_steering());
        assert!(session.has_uncertain_usage());
        assert!(native.has_pending());
        assert!(native.materialized.is_empty());
        assert!(!session
            .entries()
            .iter()
            .any(|entry| matches!(entry.value, EntryValue::Message(Message::User(_)))));
    }
}
