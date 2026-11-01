#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::{AiError, Model};

pub struct HostHttpRequest {
    pub method: http::Method,
    pub url: url::Url,
    pub headers: http::HeaderMap,
    pub body: Bytes,
}

impl std::fmt::Debug for HostHttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostHttpRequest").finish_non_exhaustive()
    }
}

pub type HostHttpResponse = http::Response<reqwest::Body>;

#[async_trait::async_trait]
pub trait HostHttpSender: Send + Sync {
    async fn send(&self, request: HostHttpRequest) -> Result<HostHttpResponse, AiError>;
}

pub(crate) struct OneShotHostHttpSender {
    sender: Mutex<Option<Arc<dyn HostHttpSender>>>,
}

impl OneShotHostHttpSender {
    pub(crate) fn new(sender: Arc<dyn HostHttpSender>) -> Self {
        Self {
            sender: Mutex::new(Some(sender)),
        }
    }

    pub(crate) async fn send(
        &self,
        request: HostHttpRequest,
    ) -> Result<reqwest::Response, AiError> {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| {
                crate::ConfigError::Parse("custom HTTP sender was already consumed".into())
            })?;
        sender.send(request).await.map(reqwest::Response::from)
    }
}

pub(crate) fn validate_custom_http_model(model: &Model) -> Result<(), AiError> {
    if !matches!(&model.endpoint.auth, crate::Auth::None)
        || !model.endpoint.default_headers.is_empty()
        || !model.spec.preset.headers.is_empty()
    {
        return Err(crate::ConfigError::Parse(
            "custom HTTP sender requires an unauthenticated route without default headers".into(),
        )
        .into());
    }
    Ok(())
}

pub(crate) fn validate_custom_http_projection(
    selected: &Model,
    custom: &Model,
) -> Result<(), AiError> {
    validate_custom_http_model(selected)?;
    validate_custom_http_model(custom)?;
    if custom.spec.id != selected.spec.id
        || custom.spec.api_name != selected.spec.api_name
        || custom.spec.display_name != selected.spec.display_name
        || custom.spec.protocol != selected.spec.protocol
        || custom.spec.capabilities != selected.spec.capabilities
        || custom.spec.limits != selected.spec.limits
        || custom.spec.pricing != selected.spec.pricing
        || custom.spec.cache != selected.spec.cache
        || custom.spec.preset != selected.spec.preset
        || custom.spec.endpoint != custom.endpoint.id
        || custom.endpoint.id == selected.endpoint.id
    {
        return Err(
            crate::ConfigError::Parse("custom HTTP model projection changed".into()).into(),
        );
    }
    Ok(())
}

pub(crate) fn validate_custom_http_headers(headers: &http::HeaderMap) -> Result<(), AiError> {
    if headers.iter().any(|(name, value)| {
        value.is_sensitive()
            || matches!(
                name.as_str(),
                "authorization"
                    | "proxy-authorization"
                    | "x-api-key"
                    | "api-key"
                    | "x-goog-api-key"
                    | "x-auth-token"
                    | "cookie"
                    | "set-cookie"
            )
            || name.as_str().starts_with("x-amz-")
    }) {
        return Err(crate::ConfigError::Parse(
            "custom HTTP sender cannot receive authentication headers".into(),
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
