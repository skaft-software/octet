//! Narrow host-mediated stream transport seam.
//!
//! This is intentionally not a generic HTTP hook. A registered transport sees
//! canonical requests and a secret-free selected-model view only; endpoint URL,
//! headers, and resolved credentials remain inside [`crate::AiClient`].

use async_trait::async_trait;

use crate::catalog::Model;
use crate::error::{AiError, Diagnostic, UnsupportedError};
use crate::pricing::Pricing;
use crate::stream::ResponseStream;
use crate::types::{ModelId, Protocol, Request};

/// Secret-free model facts supplied to a host-mediated stream transport.
#[derive(Clone, Debug)]
pub struct HostStreamModel {
    /// Canonical selected model identifier.
    pub id: ModelId,
    /// Canonical response protocol selected by the model catalog.
    pub protocol: Protocol,
    /// Immutable configured pricing, if any.
    pub pricing: Option<Pricing>,
}

impl From<&Model> for HostStreamModel {
    fn from(model: &Model) -> Self {
        Self {
            id: model.spec.id.clone(),
            protocol: model.spec.protocol,
            pricing: model.spec.pricing.clone(),
        }
    }
}

#[allow(missing_docs)]
pub struct HostProviderRequestHooks {
    pub(crate) attempt: Option<crate::client::hooks::ProviderRequestAttempt>,
}

#[allow(missing_docs)]
impl HostProviderRequestHooks {
    pub fn is_empty(&self) -> bool {
        self.attempt.is_none()
    }

    pub async fn payload(&self, payload: serde_json::Value) -> Result<serde_json::Value, AiError> {
        let Some(attempt) = &self.attempt else {
            return Ok(payload);
        };
        let body = serde_json::to_vec(&payload)
            .map_err(|_| crate::ConfigError::Parse("invalid provider hook payload".into()))?;
        let body = attempt.payload(body.into()).await?;
        serde_json::from_slice(&body)
            .map_err(|_| crate::ConfigError::Parse("invalid provider hook payload".into()).into())
    }

    pub async fn headers(&self, headers: &mut http::HeaderMap) -> Result<(), AiError> {
        if let Some(attempt) = &self.attempt {
            attempt.headers(headers).await?;
        }
        Ok(())
    }

    pub async fn response(
        &self,
        status: http::StatusCode,
        headers: &http::HeaderMap,
    ) -> Result<(), AiError> {
        if let Some(attempt) = &self.attempt {
            attempt.response(status, headers).await?;
        }
        Ok(())
    }
}

#[allow(missing_docs)]
pub struct NativeRequestContinuation {
    pub(crate) open: Box<
        dyn FnOnce(
                Request,
                Vec<std::sync::Arc<dyn crate::ProviderRequestHook>>,
                Option<std::sync::Arc<dyn crate::HostHttpSender>>,
                Option<crate::CustomOpenAiChatProfile>,
            )
                -> futures_util::future::BoxFuture<'static, Result<ResponseStream, AiError>>
            + Send,
    >,
}

impl NativeRequestContinuation {
    #[allow(missing_docs)]
    pub async fn stream(
        self,
        request: Request,
        hooks: Vec<std::sync::Arc<dyn crate::ProviderRequestHook>>,
    ) -> Result<ResponseStream, AiError> {
        (self.open)(request, hooks, None, None).await
    }

    #[allow(missing_docs)]
    pub async fn stream_custom_http(
        self,
        request: Request,
        hooks: Vec<std::sync::Arc<dyn crate::ProviderRequestHook>>,
        sender: std::sync::Arc<dyn crate::HostHttpSender>,
        custom_openai_chat: Option<crate::CustomOpenAiChatProfile>,
    ) -> Result<ResponseStream, AiError> {
        (self.open)(request, hooks, Some(sender), custom_openai_chat).await
    }
}

/// Host-owned transport for a selected catalog endpoint.
///
/// The client validates and normalizes the request before invoking this trait.
/// Implementations must not retry an accepted request implicitly, and should
/// use [`crate::CanonicalStreamAssembler`] for response construction. The
/// transport receives neither endpoint configuration nor credential material.
///
/// Deferred methods are optional and default to a typed [`UnsupportedError`]:
/// a transport that cannot park or poll a provider response must fail closed,
/// never fake a suspension. A deferred poll is admitted only through
/// [`crate::DeferredPollPermit`], which is consumed at most once; a transport
/// must not poll again on its own.
#[async_trait]
pub trait HostStreamTransport: Send + Sync {
    /// Starts one canonical request and returns its bounded response stream.
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError>;

    #[allow(missing_docs)]
    fn supports_request_hooks(&self) -> bool {
        false
    }

    #[allow(missing_docs)]
    async fn stream_with_request_hooks(
        &self,
        model: HostStreamModel,
        request: Request,
        diagnostics: Vec<Diagnostic>,
        continuation: Option<NativeRequestContinuation>,
        hooks: HostProviderRequestHooks,
    ) -> Result<ResponseStream, AiError> {
        if let Some(continuation) = continuation {
            self.stream_native(model, request, diagnostics, continuation)
                .await
        } else if hooks.is_empty() {
            self.stream(model, request, diagnostics).await
        } else {
            Err(crate::ConfigError::Parse("provider request hooks unavailable".into()).into())
        }
    }

    #[allow(missing_docs)]
    fn native_model(&self) -> Option<Model> {
        None
    }

    #[allow(missing_docs)]
    fn custom_http_model(&self, _selected: &Model) -> Option<Model> {
        None
    }

    #[allow(missing_docs)]
    async fn stream_native(
        &self,
        _model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
        _continuation: NativeRequestContinuation,
    ) -> Result<ResponseStream, AiError> {
        Err(crate::ConfigError::Parse("native request continuation unavailable".into()).into())
    }

    /// Starts one canonical request that may be parked by the provider.
    ///
    /// A parked provider returns a terminal response with
    /// [`crate::StopReason::Deferred`] and a [`crate::DeferredHandle`].
    /// `poll_after_ms` is the caller's request-local minimum delay before the
    /// next poll, or `None` for the transport default.
    async fn submit_deferred(
        &self,
        _model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
        _poll_after_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        Err(UnsupportedError::Deferred.into())
    }

    /// Polls one deferred handle, at most once per admitted permit.
    ///
    /// `wait_ms` is the maximum provider long-poll duration; `Some(0)`
    /// performs one status check and `None` uses the transport default. The
    /// permit is validated and consumed by the client before this method is
    /// called, so an implementation never needs to schedule a repeat poll.
    async fn fetch_deferred(
        &self,
        _model: HostStreamModel,
        _handle: crate::deferred::DeferredHandle,
        _wait_ms: Option<u64>,
    ) -> Result<ResponseStream, AiError> {
        Err(UnsupportedError::Deferred.into())
    }

    /// Best-effort cancellation of one deferred handle.
    ///
    /// Cancellation does not retroactively un-send provider work; callers must
    /// keep whatever usage/billing uncertainty the provider reports.
    async fn cancel_deferred(
        &self,
        _model: HostStreamModel,
        _handle: crate::deferred::DeferredHandle,
    ) -> Result<(), AiError> {
        Err(UnsupportedError::Deferred.into())
    }
}
