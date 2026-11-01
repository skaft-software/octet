use std::sync::Arc;

use octet_ai::{HostStreamTransport, Model, Request, Response, StreamEvent};
use tokio::sync::mpsc;

use crate::tool::{CancellationToken, ToolError};

pub struct ToolInferenceRequest {
    pub provider: String,
    pub model: String,
    pub request: Request,
}

pub struct ResolvedToolInferenceModel {
    pub model: Model,
    pub fetch: Option<Arc<dyn HostStreamTransport>>,
}

pub trait ToolInferenceModelResolver: Send + Sync {
    fn resolve(&self, provider: &str, model: &str)
        -> Result<ResolvedToolInferenceModel, ToolError>;
}

#[async_trait::async_trait]
pub trait ToolInferenceService: Send + Sync {
    async fn stream(
        &self,
        request: ToolInferenceRequest,
        cancellation: CancellationToken,
        updates: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<Response, ToolError>;
}
