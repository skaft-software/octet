//! Opt-in Windows PowerShell, sharing bounded shell capture and Job Object cleanup.
use octet_ai::ToolDef;
use crate::effect::ToolEffect;
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};
use super::BashTool;

/// Optional Windows shell tool; never used as an implicit Bash fallback.
pub struct PowerShellTool;
#[async_trait::async_trait]
impl Tool for PowerShellTool {
    fn definition(&self) -> ToolDef {
        let mut def = BashTool.definition();
        def.name = "powershell".into();
        def.description = "Run PowerShell on Windows, preferring pwsh.exe over Windows PowerShell. Uses -NoProfile -NonInteractive -ExecutionPolicy Bypass, bounded output and process-tree cleanup. Never an implicit Bash fallback.".into();
        def
    }
    fn prompt_snippet(&self) -> Option<&str> { Some("Execute PowerShell commands") }
    fn effect(&self,args:&serde_json::Value,ctx:&ToolContext<'_>)->Result<ToolEffect,ToolError> { BashTool.effect(args,ctx) }
    async fn execute(&self,args:serde_json::Value,ctx:&ToolContext<'_>)->Result<ToolOutput,ToolError> {
        self.effect(&args,ctx)?;
        #[cfg(windows)]
        { BashTool.execute_windows(args,ctx,true,&super::ShellSessionEnvironment::default(),None).await }
        #[cfg(not(windows))]
        { Err(ToolError::new("powershell is available only on Windows")) }
    }
}

#[cfg(windows)]
pub(super) fn resolve_shell() -> Result<std::path::PathBuf,ToolError> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["pwsh.exe","powershell.exe"] {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if candidate.is_file() { return Ok(candidate); }
        }
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        let candidate = std::path::PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        if candidate.is_file() { return Ok(candidate); }
    }
    Err(ToolError::new("PowerShell is unavailable: install pwsh.exe or Windows PowerShell"))
}

#[cfg(windows)]
pub(super) fn configure_command(command:&mut tokio::process::Command,source:&str) {
    use base64::Engine;
    let source = format!("try {{ [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 }} catch {{}}\n{source}");
    let bytes = source.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>();
    command.args(["-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-EncodedCommand"])
        .arg(base64::engine::general_purpose::STANDARD.encode(bytes));
}
