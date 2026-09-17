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
