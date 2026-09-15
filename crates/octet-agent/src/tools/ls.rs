//! Bounded directory listing, including dotfiles.
use octet_ai::ToolDef;
use serde::Deserialize;
use crate::effect::ToolEffect;
use crate::tool::{Tool, ToolConcurrency, ToolContext, ToolError, ToolOutput, ReplaySafety};
use super::{parse_args, validate_effect_path, MAX_FILE_BYTES};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args { path: Option<String>, limit: Option<usize> }

/// Built-in `ls` tool.
pub struct LsTool;

#[async_trait::async_trait]
impl Tool for LsTool {
    fn definition(&self) -> ToolDef {
        ToolDef { name: "ls".into(), description: "List directory contents, including dotfiles, sorted case-insensitively. Directories have a '/' suffix. Default limit: 500 entries; output is byte-bounded.".into(), parameters: serde_json::json!({"type":"object", "properties": {"path":{"type":"string"}, "limit":{"type":"integer","minimum":1}}, "additionalProperties":false}),
            constrained_sampling: None,
        }
    }
    fn prompt_snippet(&self) -> Option<&str> { Some("List directory contents") }
    fn effect(&self, args: &serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolEffect,ToolError> {
        let args: Args = parse_args(args.clone())?;
        validate_effect_path(args.path.as_deref().unwrap_or("."), ctx.sandbox.allow_external_paths)?;
        if args.limit == Some(0) { return Err(ToolError::new("invalid arguments: limit must be positive")); }
        // Windows has no exposed descriptor-relative enumeration primitive yet.
        Ok(if cfg!(unix) && !ctx.sandbox.allow_external_paths { ToolEffect::WorkspaceRead } else { ToolEffect::HostRead })
    }
    fn concurrency(&self) -> ToolConcurrency { ToolConcurrency::Parallel }
    fn replay_safety(&self) -> ReplaySafety { ReplaySafety::Safe }
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput,ToolError> {
        self.effect(&args, ctx)?;
        let args: Args = parse_args(args)?;
        let path = ctx.resolve_existing(args.path.as_deref().unwrap_or("."))?;
        let limit = args.limit.unwrap_or(500);
        let budget = ctx.sandbox.max_output_bytes.saturating_sub(128);
        let cancellation = ctx.cancellation.clone();
        let timeout = ctx.sandbox.bash_timeout;
        tokio::task::spawn_blocking(move || {
            let start = std::time::Instant::now();
            let mut entries = Vec::new();
            let mut bytes = 0usize;
            enumerate(&path, |name, directory| {
                if cancellation.is_cancelled() || start.elapsed() >= timeout { return Err(ToolError::new("ls cancelled or execution limit exceeded")); }
                bytes = bytes.saturating_add(name.len());
                if entries.len() >= 100_000 || bytes > MAX_FILE_BYTES { return Err(ToolError::new("ls directory exceeds enumeration resource limit")); }
                entries.push((name.to_lowercase(), name, directory));
                Ok(())
            })?;
            entries.sort_unstable();
            let mut output = String::new();
            let mut truncated = false;
            for (index, (_, name, directory)) in entries.into_iter().enumerate() {
                if index >= limit || output.len().saturating_add(name.len() + 2) > budget { truncated = true; break; }
                if !output.is_empty() { output.push('\n'); }
                output.push_str(&name);
                if directory { output.push('/'); }
            }
            if truncated { output.push_str(&format!("\n[entries or byte limit reached; limit={limit}; truncated=true]")); }
            else if output.is_empty() { output.push_str("(empty directory)"); }
            Ok(ToolOutput::new(output))
        }).await.map_err(|e| ToolError::new(format!("ls worker failed: {e}")))?
    }
}

#[cfg(unix)]
fn enumerate(path: &std::path::Path, mut emit: impl FnMut(String,bool)->Result<(),ToolError>) -> Result<(),ToolError> {
    use rustix::fs::{open, openat, statat, Dir, OFlags, Mode, AtFlags, FileType};
    use std::path::Component;
    let io = |e| ToolError::new(format!("ls directory read failed: {e}"));
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = open("/", flags, Mode::empty()).map_err(io)?;
    for component in path.components() {
        if let Component::Normal(name) = component { fd = openat(&fd, name, flags, Mode::empty()).map_err(io)?; }
    }
    for entry in Dir::read_from(&fd).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." { continue; }
        let Ok(stat) = statat(&fd, name, AtFlags::SYMLINK_NOFOLLOW) else { continue; };
        // Never follow an entry symlink outside the authorized directory.
        emit(name.to_string_lossy().into_owned(), FileType::from_raw_mode(stat.st_mode) == FileType::Directory)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn enumerate(path: &std::path::Path, mut emit: impl FnMut(String,bool)->Result<(),ToolError>) -> Result<(),ToolError> {
    let entries = std::fs::read_dir(path).map_err(|e| ToolError::new(format!("ls directory read failed: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| ToolError::new(format!("ls directory read failed: {e}")))?;
        let Ok(kind) = entry.file_type() else { continue; };
        emit(entry.file_name().to_string_lossy().into_owned(), kind.is_dir())?;
    }
    Ok(())
}
