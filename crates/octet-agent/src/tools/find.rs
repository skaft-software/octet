//! Glob discovery via installed fd; never downloads an executable.
use std::process::Stdio;
use octet_ai::ToolDef;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use crate::effect::ToolEffect;
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};
use super::{parse_args, validate_effect_path, SearchTool, MAX_TOOL_PATH_BYTES};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args { pattern: String, path: Option<String>, limit: Option<usize> }

/// Built-in `find` tool.
pub struct FindTool;
#[async_trait::async_trait]
impl Tool for FindTool {
    fn definition(&self) -> ToolDef {
        ToolDef { name: "find".into(), description: "Find paths by glob, relative to the search directory. Includes hidden paths and respects .gitignore. Default limit: 1000; requires installed fd.".into(), parameters: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"limit":{"type":"integer","minimum":1}},"required":["pattern"],"additionalProperties":false}),
            constrained_sampling: None,
        }
    }
    fn prompt_snippet(&self) -> Option<&str> { Some("Find files by glob pattern (respects .gitignore)") }
    fn effect(&self, args: &serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolEffect,ToolError> {
        let args: Args = parse_args(args.clone())?;
        validate_effect_path(args.path.as_deref().unwrap_or("."),ctx.sandbox.allow_external_paths)?;
        // Same native process gates/classification as search; model pattern stays data.
        SearchTool.effect(&serde_json::json!({"query":args.pattern,"max_results":args.limit.unwrap_or(1000)}),ctx)
    }
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput,ToolError> {
        self.effect(&args,ctx)?;
        let args: Args = parse_args(args)?;
        let root = ctx.resolve_existing(args.path.as_deref().unwrap_or("."))?;
        if !root.is_dir() { return Err(ToolError::new("find path must be a directory")); }
        let limit = args.limit.unwrap_or(1000);
        let mut command = tokio::process::Command::new("fd");
        command.args(["--glob","--color=never","--hidden","--print0","--path-separator","/","--max-results"])
            .arg(limit.to_string());
        if !root.ancestors().any(|p| p.join(".git").exists()) { command.arg("--no-require-git"); }
        let mut pattern = args.pattern;
        if pattern.contains('/') {
            command.arg("--full-path");
            if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" { pattern = format!("**/{pattern}"); }
            #[cfg(windows)]
            { pattern = pattern.replace('/', r"[/\]"); }
        }
        command.arg("--").arg(pattern).arg(".").current_dir(&root)
            .env_clear().envs(crate::extension_process::sanitized_subprocess_environment())
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| ToolError::new(format!("find requires installed fd; failed to start: {e}")))?;
        let mut stdout = child.stdout.take().expect("piped fd stdout");
        let work = async {
            let budget = ctx.sandbox.max_output_bytes.saturating_sub(128);
            let mut output = String::new();
            let mut entry = Vec::new();
            let mut count = 0usize;
            let mut truncated = false;
            let mut chunk = [0u8;8192];
            'read: loop {
                let n = stdout.read(&mut chunk).await.map_err(|e| ToolError::new(format!("find output read failed: {e}")))?;
                if n == 0 { break; }
                for byte in &chunk[..n] {
                    if *byte == 0 {
                        let path = String::from_utf8_lossy(&entry);
                        let path = path.strip_prefix("./").unwrap_or(&path);
                        if count >= limit || output.len().saturating_add(path.len()+1) > budget { truncated = true; break 'read; }
                        if !output.is_empty() { output.push('\n'); }
                        output.push_str(path);
                        entry.clear();
                        count += 1;
                    } else {
                        if entry.len() >= MAX_TOOL_PATH_BYTES { return Err(ToolError::new("find path exceeds output record limit")); }
                        entry.push(*byte);
                    }
                }
            }
            if truncated { let _ = child.start_kill(); }
            let status = child.wait().await.map_err(|e| ToolError::new(format!("find wait failed: {e}")))?;
            if !truncated && !status.success() { return Err(ToolError::new("find failed; check glob syntax and installed fd")); }
            if truncated || count == limit { output.push_str(&format!("\n[results or byte limit reached; limit={limit}; truncated=true]")); }
            else if output.is_empty() { output.push_str("No files found matching pattern"); }
            Ok(ToolOutput::new(output))
        };
        let result = tokio::select! {
            biased;
            _ = ctx.cancellation.cancelled() => Err(ToolError::new("find cancelled")),
            result = tokio::time::timeout(ctx.sandbox.bash_timeout,work) => result.unwrap_or_else(|_| Err(ToolError::new("find execution limit exceeded"))),
        };
        if result.is_err() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1),child.wait()).await;
        }
        result
    }
}
