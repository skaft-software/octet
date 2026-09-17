//! Deferred provider responses: canonical handles and one-shot poll permits.
//!
//! A provider may answer a generation request with "not finished yet, here is a
//! handle" instead of a final message. Pi turns that into a durably suspended
//! run rather than an in-process wait. This module is the **transport half** of
//! that lifecycle: the canonical handle shape, the typed stop reason that
//! carries it, and the permit type that makes a deferred poll at-most-once.
//!
//! The durable suspend/resume leaf, poll numbering, generation, crash recovery
//! and effect-pending replacement live in the host/kernel layer; a transport
//! never decides to suspend a run and never re-polls on its own. Every refusal
//! here is fail-closed: a foreign, expired, or already-consumed permit produces
//! a typed refusal instead of a silent second provider poll.

use serde::{Deserialize, Serialize};

/// Provider handle for one deferred response.
///
/// Field names mirror the provider-neutral upstream shape. `id` is the
/// provider token (a response id, or a batch id plus row id); `data` is opaque
/// provider conversion material required to reconstruct the final message and
/// is never included in `Debug`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferredHandle {
    /// Provider id that owns the handle.
    pub provider: String,
    /// Provider-local model id that owns the handle.
    pub model_id: String,
    /// Api id that produced the response carrying this handle.
    pub api: String,
    /// Provider token: a response id, or a batch id plus row id.
    pub id: String,
    /// Absolute expiry, in milliseconds since the Unix epoch, when the provider
    /// supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    /// Provider-suggested minimum delay before the next poll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    /// Provider conversion data required to reconstruct the final message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl std::fmt::Debug for DeferredHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeferredHandle")
            .field("provider", &self.provider)
            .field("model_id", &self.model_id)
            .field("api", &self.api)
            .field("id", &self.id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("poll_after_ms", &self.poll_after_ms)
            .field(
                "data",
                &self.data.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl DeferredHandle {
    /// Builds a handle with only the required fields.
    pub fn new(
        provider: impl Into<String>,
        model_id: impl Into<String>,
        api: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model_id: model_id.into(),
            api: api.into(),
            id: id.into(),
            expires_at_ms: None,
            poll_after_ms: None,
            data: None,
        }
    }

    /// Sets the provider's absolute expiry (milliseconds since the Unix epoch).
    pub fn with_expires_at_ms(mut self, expires_at_ms: i64) -> Self {
        self.expires_at_ms = Some(expires_at_ms);
        self
    }

    /// Sets the provider-suggested minimum delay before the next poll.
    pub fn with_poll_after_ms(mut self, poll_after_ms: u64) -> Self {
        self.poll_after_ms = Some(poll_after_ms);
        self
    }

    /// Sets opaque provider conversion data.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Whether the provider's absolute expiry has passed at `now_ms`.
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_some_and(|expiry| expiry <= now_ms)
    }

    /// Why this handle cannot be polled for the given identity, if any.
    ///
    /// Mirrors upstream `deferredHandleIsValid`: the id must be non-empty, the
    /// provider and model id must equal the caller's configured identity, and
    /// the api must equal the api of the response that produced the handle.
    /// `now_ms` additionally enforces the provider's absolute expiry.
    pub fn rejection(
        &self,
        provider: &str,
        model_id: &str,
        response_api: &str,
        now_ms: i64,
    ) -> Option<DeferredHandleRejection> {
        if self.id.is_empty() {
            return Some(DeferredHandleRejection::EmptyId);
        }
        if self.provider != provider || self.model_id != model_id {
            return Some(DeferredHandleRejection::ForeignProvider {
                configured_provider: provider.to_owned(),
                configured_model_id: model_id.to_owned(),
                handle_provider: self.provider.clone(),
                handle_model_id: self.model_id.clone(),
            });
        }
        if self.api != response_api {
            return Some(DeferredHandleRejection::ForeignApi {
                configured: response_api.to_owned(),
                handle: self.api.clone(),
            });
        }
        if let Some(expires_at_ms) = self.expires_at_ms {
            if expires_at_ms <= now_ms {
                return Some(DeferredHandleRejection::Expired { expires_at_ms });
            }
        }
        None
    }
}

/// Why a provider's deferred handle cannot be trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeferredHandleRejection {
    /// The provider reported a deferred response without a usable id.
    EmptyId,
    /// Provider or model id does not match the run's durable configuration.
    ForeignProvider {
        /// Provider id captured by the caller.
        configured_provider: String,
        /// Model id captured by the caller.
        configured_model_id: String,
        /// Provider id the handle claims.
        handle_provider: String,
        /// Model id the handle claims.
        handle_model_id: String,
    },
    /// The handle's api does not match the response that carried it.
    ForeignApi {
        /// Api id of the response that carried the handle.
        configured: String,
        /// Api id the handle claims.
        handle: String,
    },
    /// The provider's absolute expiry has passed.
    Expired {
        /// Expiry the provider supplied.
        expires_at_ms: i64,
    },
}

impl std::fmt::Display for DeferredHandleRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyId => write!(formatter, "the deferred handle has an empty provider id"),
            Self::ForeignProvider {
                configured_provider,
                configured_model_id,
                handle_provider,
                handle_model_id,
            } => write!(
                formatter,
                "the deferred handle belongs to {handle_provider}/{handle_model_id} but the run is \
                 configured for {configured_provider}/{configured_model_id}"
            ),
            Self::ForeignApi {
                configured,
                handle,
            } => write!(
                formatter,
                "the deferred handle is for api {handle} but the response used api {configured}"
            ),
            Self::Expired { expires_at_ms } => write!(
                formatter,
                "the deferred handle expired at {expires_at_ms} ms since the Unix epoch"
            ),
        }
    }
}

impl std::error::Error for DeferredHandleRejection {}

/// Why a deferred poll was refused before any provider work could start.
///
/// Refusals never fall back to "still pending": that would leave a run parked
/// forever or admit a second billable poll for the same permit.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeferredPollRefusalKind {
    /// The driving pass carried no poll permit at all.
    #[error("deferred poll refused: the pass carries no poll permit")]
    NoPermit,
    /// The same permit already admitted a poll; a second poll would duplicate work.
    #[error("deferred poll refused: the permit was already consumed")]
    AlreadyConsumed,
    /// The permit was minted for a different (older or newer) durable leaf.
    #[error("deferred poll refused: permit generation {permit} does not match leaf generation {leaf}")]
    StaleGeneration {
        /// Generation carried by the permit.
        permit: u64,
        /// Generation currently durable.
        leaf: u64,
    },
}

/// At most one deferred poll permit per driving pass.
///
/// A permit is minted for one durable leaf generation, is consumed at most
/// once, and cannot be re-minted by a transport. [`DeferredPollPermit::none`]
/// is the honest representation of a pass that carries no permit at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredPollPermit {
    pass_id: String,
    generation: u64,
    remaining: u32,
    consumed: bool,
}

impl DeferredPollPermit {
    /// Grants one deferred poll against `generation`.
    pub fn one(pass_id: impl Into<String>, generation: u64) -> Self {
        Self {
            pass_id: pass_id.into(),
            generation,
            remaining: 1,
            consumed: false,
        }
    }

    /// A pass that carries no poll permit.
    pub fn none(pass_id: impl Into<String>, generation: u64) -> Self {
        Self {
            pass_id: pass_id.into(),
            generation,
            remaining: 0,
            consumed: false,
        }
    }

    /// Identifier of the driving pass.
    pub fn pass_id(&self) -> &str {
        &self.pass_id
    }

    /// Durable leaf generation the permit was minted for.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Permits still available to this pass.
    pub fn remaining(&self) -> u32 {
        self.remaining
    }

    /// Whether this permit already admitted a poll.
    pub fn is_consumed(&self) -> bool {
        self.consumed
    }

    /// Consumes the permit for exactly one poll against `leaf_generation`.
    ///
    /// Refuses before any provider work when the permit carries no poll, was
    /// already consumed, or was minted for another generation. A refusal is
    /// terminal for this call: a caller that wants a later poll must obtain a
    /// newly minted permit.
    pub fn consume(&mut self, leaf_generation: u64) -> Result<(), DeferredPollRefusalKind> {
        if self.consumed {
            return Err(DeferredPollRefusalKind::AlreadyConsumed);
        }
        if self.remaining == 0 {
            return Err(DeferredPollRefusalKind::NoPermit);
        }
        if self.generation != leaf_generation {
            return Err(DeferredPollRefusalKind::StaleGeneration {
                permit: self.generation,
                leaf: leaf_generation,
            });
        }
        self.remaining -= 1;
        self.consumed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_are_one_shot_and_generation_bound() {
        let mut permit = DeferredPollPermit::one("pass-1", 3);
        assert_eq!(permit.remaining(), 1);
        assert!(!permit.is_consumed());
        assert!(permit.consume(3).is_ok());
        assert!(permit.is_consumed());
        assert_eq!(
            permit.consume(3),
            Err(DeferredPollRefusalKind::AlreadyConsumed)
        );

        let mut stale = DeferredPollPermit::one("pass-1", 2);
        assert_eq!(
            stale.consume(3),
            Err(DeferredPollRefusalKind::StaleGeneration { permit: 2, leaf: 3 })
        );
        assert!(!stale.is_consumed());

        let mut none = DeferredPollPermit::none("pass-1", 3);
        assert_eq!(none.consume(3), Err(DeferredPollRefusalKind::NoPermit));
    }

    #[test]
    fn handle_rejection_matches_identity_and_expiry() {
        let handle = DeferredHandle::new("anthropic", "claude", "anthropic-messages", "resp-1");
        assert_eq!(handle.rejection("anthropic", "claude", "anthropic-messages", 0), None);
        assert_eq!(
            DeferredHandle::new("anthropic", "claude", "anthropic-messages", "").rejection(
                "anthropic",
                "claude",
                "anthropic-messages",
                0
            ),
            Some(DeferredHandleRejection::EmptyId)
        );
        assert!(matches!(
            handle.rejection("openai", "claude", "anthropic-messages", 0),
            Some(DeferredHandleRejection::ForeignProvider { .. })
        ));
        assert!(matches!(
            handle.rejection("anthropic", "claude", "openai-chat", 0),
            Some(DeferredHandleRejection::ForeignApi { .. })
        ));
        assert!(matches!(
            handle
                .clone()
                .with_expires_at_ms(10)
                .rejection("anthropic", "claude", "anthropic-messages", 10),
            Some(DeferredHandleRejection::Expired { .. })
        ));
    }

    #[test]
    fn handle_debug_redacts_conversion_data() {
        let handle = DeferredHandle::new("anthropic", "claude", "anthropic-messages", "resp-1")
            .with_data(serde_json::json!({"secret": "sensitive"}));
        let debug = format!("{handle:?}");
        assert!(debug.contains("resp-1"));
        assert!(!debug.contains("sensitive"));
    }
}
