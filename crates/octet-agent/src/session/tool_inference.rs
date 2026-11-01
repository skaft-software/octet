use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolInferenceAttempt {
    pub(super) attempt: u64,
    pub(super) parent: String,
    pub(super) endpoint: EndpointId,
    pub(super) model: ModelId,
}

impl ToolInferenceAttempt {
    pub(super) fn validate(&self) -> Result<(), SessionError> {
        if self.attempt == 0
            || self.parent.is_empty()
            || self.parent.len() > 1024
            || self.parent.chars().any(char::is_control)
        {
            return Err(SessionError::Limit("invalid tool inference attempt".into()));
        }
        UsageUncertaintyRecord {
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            operation: "tool_inference".into(),
        }
        .validate()
    }
}

impl Session {
    pub(crate) fn start_tool_inference(
        &mut self,
        parent: String,
        endpoint: EndpointId,
        model: ModelId,
    ) -> Result<u64, SessionError> {
        let attempt = self
            .next_tool_inference_attempt
            .checked_add(1)
            .ok_or_else(|| SessionError::Limit("tool inference attempt limit".into()))?;
        let record = ToolInferenceAttempt {
            attempt,
            parent,
            endpoint,
            model,
        };
        record.validate()?;
        let mut buffer = Vec::new();
        write_json_line(
            &mut buffer,
            &SessionRecordRef::ToolInferenceStarted { record: &record },
        )?;
        self.persist(&buffer)?;
        self.next_tool_inference_attempt = attempt;
        self.tool_inference_attempts.insert(attempt, record);
        Ok(attempt)
    }

    pub(crate) fn settle_tool_inference(&mut self, attempt: u64) -> Result<(), SessionError> {
        assert!(self.tool_inference_attempts.contains_key(&attempt));
        let mut buffer = Vec::new();
        write_json_line(
            &mut buffer,
            &SessionRecordRef::ToolInferenceSettled { attempt },
        )?;
        self.persist(&buffer)?;
        self.tool_inference_attempts.remove(&attempt);
        Ok(())
    }

    pub(crate) fn record_tool_inference_usage(
        &mut self,
        attempt: u64,
        usage: Usage,
        cost: Option<Cost>,
    ) -> Result<(), SessionError> {
        let pending = self
            .tool_inference_attempts
            .get(&attempt)
            .expect("usage owns a pending attempt")
            .clone();
        self.record_usage(Self::provider_usage_record(
            UsageRecordKind::ToolInference {
                attempt,
                parent: pending.parent,
            },
            pending.endpoint,
            pending.model,
            usage,
            cost,
            None,
        ))?;
        self.tool_inference_attempts.remove(&attempt);
        Ok(())
    }
}
