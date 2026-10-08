//! Bounded pre-native terminal input requests, separate from legacy observations.
use super::*;

/// API 0.4 pre-native input interception, available only with a real frontend.
pub const EXTENSION_FEATURE_TERMINAL_INPUT_INTERCEPT: &str = "terminal_input_intercept_v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    data: String,
}

impl ExtensionProcess {
    /// Whether this connection selected the frontend's pre-native input channel.
    pub fn supports_terminal_input_interception(&self) -> bool {
        let connection = read_std_lock(&self.inner.connection);
        let supported = read_std_lock(&connection.protocol)
            .supports(EXTENSION_FEATURE_TERMINAL_INPUT_INTERCEPT);
        supported
    }

    /// Run the remote listener chain within the caller's remaining input budget.
    /// Empty output consumes input. Cancellation never replays a late reply.
    ///
    /// The owner travels in `params.resource_owner` instead of through owner
    /// routing: this is a per-keystroke frontend decision, so it must not
    /// acquire session-view or history-transport work, and a listener that only
    /// decides how one event is dispatched never needs a session binding.
    /// Liveness stays enforced here before and after the round trip, and the
    /// adapter revalidates the owner against its own foreground state.
    pub async fn intercept_terminal_input(
        &self,
        data: &str,
        owner: &ExtensionResourceOwner,
        timeout: Duration,
    ) -> Result<String, ExtensionRuntimeError> {
        if data.len() > MAX_EXTENSION_TERMINAL_INPUT_BYTES {
            return Err(ExtensionRuntimeError::Protocol(
                "terminal input exceeds byte bound".into(),
            ));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        if !self.supports_terminal_input_interception() || !self.resource_owner_is_live(owner) {
            return Err(ExtensionRuntimeError::Closed(
                "terminal input owner is unavailable".into(),
            ));
        }
        let result = connection
            .request(
                "ui/terminal-input/intercept",
                serde_json::json!({"data":data,"resource_owner":owner}),
                timeout,
            )
            .await?;
        let reply: Reply = serde_json::from_value(result)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        if reply.data.len() > MAX_EXTENSION_TERMINAL_INPUT_BYTES {
            return Err(ExtensionRuntimeError::Protocol(
                "terminal input reply exceeds byte bound".into(),
            ));
        }
        if !self.resource_owner_is_live(owner) {
            return Err(ExtensionRuntimeError::Closed(
                "terminal input owner was retired".into(),
            ));
        }
        Ok(reply.data)
    }
}
