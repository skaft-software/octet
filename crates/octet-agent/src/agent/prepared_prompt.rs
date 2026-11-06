use super::*;
use octet_ai::{EndpointId, HostRequestOptions, HostStreamTransport, ModelId, Pricing};

pub enum PreparedPromptMode {
    Normal,
    ResponsesPrewarm,
    WithoutTools,
}

pub struct PreparedHostPrompt {
    input: UserInput,
    authority: PreparedHostAuthority,
}

pub(super) struct PreparedHostAuthority {
    agent_scope: String,
    resource_owner: String,
    model_id: ModelId,
    endpoint_id: EndpointId,
    protocol: Protocol,
    pricing: Option<Pricing>,
    fetch: Arc<dyn HostStreamTransport>,
}

fn association_error() -> AiError {
    AiError::Config(octet_ai::ConfigError::Parse(
        "prepared host prompt no longer matches its Agent, session or selected model".into(),
    ))
}

impl PreparedHostAuthority {
    fn require_model(&self, model: &Model) -> Result<(), AiError> {
        if self.model_id != model.spec.id
            || self.endpoint_id != model.endpoint.id
            || self.endpoint_id != model.spec.endpoint
            || self.protocol != model.spec.protocol
            || self.pricing != model.spec.pricing
        {
            return Err(association_error());
        }
        Ok(())
    }

    pub(super) fn host_options_for(&self, model: &Model) -> Result<HostRequestOptions, AiError> {
        self.require_model(model)?;
        Ok(HostRequestOptions {
            fetch: Some(self.fetch.clone()),
            ..Default::default()
        })
    }

    pub(super) fn auxiliary_options(
        &self,
        model: &Model,
    ) -> Result<Option<HostRequestOptions>, AiError> {
        if self.model_id == model.spec.id {
            self.host_options_for(model).map(Some)
        } else {
            Ok(None)
        }
    }
}

impl Agent {
    pub fn prepare_host_prompt(
        &self,
        input: UserInput,
        fetch: Arc<dyn HostStreamTransport>,
    ) -> PreparedHostPrompt {
        PreparedHostPrompt {
            input,
            authority: PreparedHostAuthority {
                agent_scope: self.tool_scope.clone(),
                resource_owner: self.resource_owner.clone(),
                model_id: self.model.spec.id.clone(),
                endpoint_id: self.model.endpoint.id.clone(),
                protocol: self.model.spec.protocol,
                pricing: self.model.spec.pricing.clone(),
                fetch,
            },
        }
    }

    pub async fn prompt_prepared(
        &mut self,
        prepared: PreparedHostPrompt,
        mode: PreparedPromptMode,
    ) -> Result<Run<'_>, AgentError> {
        if prepared.authority.agent_scope != self.tool_scope
            || prepared.authority.resource_owner != self.resource_owner
            || prepared.authority.resource_owner != self.session.resource_owner_key()
        {
            return Err(association_error().into());
        }
        prepared.authority.require_model(&self.model)?;
        self.cache_warmer.cancel(&mut self.session, "new prompt")?;
        self.prompt_with_tools(
            prepared.input,
            !matches!(mode, PreparedPromptMode::WithoutTools),
            false,
            Some(prepared.authority),
        )
        .await
    }
}
