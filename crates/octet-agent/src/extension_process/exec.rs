//! Owner-scoped direct process execution wire contract.
use super::*;

/// Direct argv execution request; authorization and launching belong to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionExecRequest {
    /// Active parent request.
    pub parent_request_id: u64,
    /// Issued owner for a deferred call.
    #[serde(default)]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Executable, without shell interpretation.
    pub command: String,
    /// Exact argv entries.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: String,
    /// Optional timeout in milliseconds; zero means no timeout.
    pub timeout_ms: Option<u64>,
    /// Whether the supplied Pi AbortSignal was already aborted.
    #[serde(default)]
    pub cancelled: bool,
}

impl ExtensionExecRequest {
    /// Validate untrusted process request bounds before forwarding.
    pub fn validate(&self) -> Result<(), String> {
        if self.command.is_empty()
            || self.command.len() > 131072
            || self.command.contains('\0')
            || self.args.len() > 256
            || self
                .args
                .iter()
                .any(|arg| arg.len() > 131072 || arg.contains('\0'))
            || self.cwd.is_empty()
            || self.cwd.len() > 4096
            || self.cwd.contains('\0')
            || self.timeout_ms.is_some_and(|ms| ms > 9_007_199_254_740_991)
        {
            return Err("invalid or oversized exec request".into());
        }
        Ok(())
    }
}
