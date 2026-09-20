//! Host-resolved session metadata for shell invocations, never global environment mutation.
use super::{BashTool, PowerShellTool};
use crate::effect::ToolEffect;
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};
use std::sync::Arc;

/// Current session/model metadata. Empty fields remove inherited parent metadata.
#[derive(Clone, Default)]
pub struct ShellSessionEnvironment {
    /// Stable session ID.
    pub session_id: Option<String>,
    /// Absolute session JSONL path; absent for ephemeral sessions.
    pub session_file: Option<String>,
    /// Currently selected provider, not a router's eventual upstream.
    pub provider: Option<String>,
    /// Currently selected model ID.
    pub model: Option<String>,
    /// Effective reasoning level.
    pub reasoning_level: Option<String>,
}
impl ShellSessionEnvironment {
    pub(super) fn apply(&self, command: &mut tokio::process::Command) -> Result<(), ToolError> {
        for (key, value) in [
            ("PI_SESSION_ID", &self.session_id),
            ("PI_SESSION_FILE", &self.session_file),
            ("PI_PROVIDER", &self.provider),
            ("PI_MODEL", &self.model),
            ("PI_REASONING_LEVEL", &self.reasoning_level),
        ] {
            command.env_remove(key);
            if let Some(value) = value {
                if value.len() > 32 * 1024 || value.contains('\0') {
                    return Err(ToolError::new("invalid shell session metadata"));
                }
                command.env(key, value);
            }
        }
        Ok(())
    }
}

/// Shell tool whose host callback resolves metadata anew immediately before execution.
/// Return `ShellSessionEnvironment::default()` to opt out and clear inherited values.
pub struct SessionShellTool {
    bash: BashTool,
    resolve: Arc<dyn Fn() -> ShellSessionEnvironment + Send + Sync>,
    command_prefix: Option<Arc<str>>,
    powershell: bool,
}
impl SessionShellTool {
    /// Prepends a fixed, host-owned command prefix to every accepted call,
    /// followed by a newline and the model's command. The prefix is trusted
    /// host configuration (shell setup such as a `PATH` or virtualenv
    /// activation); the model never supplies or overrides it. It is applied
    /// only after the call's arguments parse, so a malformed call still fails
    /// on its own arguments rather than executing the prefix.
    pub fn with_command_prefix(mut self, prefix: impl Into<String>) -> Self {
        let prefix: String = prefix.into();
        self.command_prefix = (!prefix.is_empty()).then(|| Arc::from(prefix.as_str()));
        self
    }

    /// Applies the host command prefix to a parsed argument object. Returns the
    /// arguments unchanged when no prefix is configured.
    fn apply_command_prefix(&self, args: serde_json::Value) -> serde_json::Value {
        let Some(prefix) = &self.command_prefix else {
            return args;
        };
        let serde_json::Value::Object(mut object) = args else {
            return args;
        };
        if let Some(serde_json::Value::String(command)) = object.get_mut("command") {
            *command = format!("{prefix}\n{command}");
        }
        serde_json::Value::Object(object)
    }
}
impl BashTool {
    /// Attach a host-owned live session resolver; it is not called during effect classification.
    pub fn with_session_environment(
        resolve: impl Fn() -> ShellSessionEnvironment + Send + Sync + 'static,
    ) -> SessionShellTool {
        SessionShellTool {
            bash: BashTool::default(),
            resolve: Arc::new(resolve),
            command_prefix: None,
            powershell: false,
        }
    }
}
impl PowerShellTool {
    /// Attach the same live session metadata resolver used for Bash.
    pub fn with_session_environment(
        resolve: impl Fn() -> ShellSessionEnvironment + Send + Sync + 'static,
    ) -> SessionShellTool {
        SessionShellTool {
            bash: BashTool::default(),
            resolve: Arc::new(resolve),
            command_prefix: None,
            powershell: true,
        }
    }
}
#[async_trait::async_trait]
impl Tool for SessionShellTool {
    fn definition(&self) -> octet_ai::ToolDef {
        if self.powershell {
            super::powershell::definition(&self.bash)
        } else {
            self.bash.definition()
        }
    }
    fn prompt_snippet(&self) -> Option<&str> {
        if self.powershell {
            Some("Execute PowerShell commands")
        } else {
            self.bash.prompt_snippet()
        }
    }
    fn prompt_guidelines(&self) -> &[&str] {
        // Mirrors Pi's `exposeSessionEnvironment && promptGuidelines` gate: only
        // the variant that really injects live session metadata advertises it,
        // because the plain Bash/PowerShell tools clear the inherited values.
        &["You can inspect PI_* environment variables for current model and session details."]
    }
    fn effect(
        &self,
        args: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        self.bash.effect(args, ctx)
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.effect(&args, ctx)?;
        let args = self.apply_command_prefix(args);
        let environment = (self.resolve)();
        #[cfg(windows)]
        {
            self.bash
                .execute_windows(args, ctx, self.powershell, &environment, None)
                .await
        }
        #[cfg(unix)]
        {
            if self.powershell {
                return Err(ToolError::new("powershell is available only on Windows"));
            }
            self.bash.execute_unix(args, ctx, &environment, None).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = environment;
            Err(ToolError::new("shell is unavailable on this platform"))
        }
    }
}
