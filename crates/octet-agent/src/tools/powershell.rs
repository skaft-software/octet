//! Windows PowerShell tool, sharing bounded shell capture and Job Object cleanup.
//!
//! On Windows this is the default model shell; `bash` is registered instead
//! only when the host opts into a Bash-compatible shell. It reuses the
//! `bash` tool's argument schema, effect classification, timeout, output
//! limits, cancellation, working-directory and environment handling, and
//! differs only in the launched program and how the command reaches it.
use super::BashTool;
use crate::effect::ToolEffect;
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};
use octet_ai::ToolDef;

/// Windows shell tool. On other platforms it is never executed.
#[derive(Default)]
pub struct PowerShellTool {
    bash: BashTool,
}

/// Prompt snippet shared by the plain and session-environment variants.
pub(super) const PROMPT_SNIPPET: &str = "Execute PowerShell commands (prefer rg when available; otherwise use Select-String and Get-ChildItem for file and content search)";

#[async_trait::async_trait]
impl Tool for PowerShellTool {
    fn composition_is_unmetered(&self) -> bool {
        self.bash.composition_is_unmetered()
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        self.bash.output_schema()
    }

    fn definition(&self) -> ToolDef {
        definition(&self.bash)
    }
    fn prompt_snippet(&self) -> Option<&str> {
        Some(PROMPT_SNIPPET)
    }
    fn replay_safety(&self) -> crate::tool::ReplaySafety {
        self.bash.replay_safety()
    }
    fn concurrency(&self) -> crate::tool::ToolConcurrency {
        self.bash.concurrency()
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
        #[cfg(windows)]
        {
            self.bash
                .execute_windows(
                    args,
                    ctx,
                    true,
                    &super::ShellSessionEnvironment::default(),
                    None,
                )
                .await
        }
        #[cfg(not(windows))]
        {
            Err(ToolError::new("powershell is available only on Windows"))
        }
    }
}

/// The model-visible schema. The description names only PowerShell so a
/// Windows model is never steered toward Bash syntax.
pub(super) fn definition(bash: &BashTool) -> ToolDef {
    let mut def = bash.definition();
    def.name = "powershell".into();
    def.description = "Run a command in PowerShell (pwsh.exe when installed, otherwise Windows PowerShell 5.1) with -NoProfile -NonInteractive. Use PowerShell syntax: `;` separates statements, `$env:NAME` reads environment variables, and Get-ChildItem, Get-Content and Select-String list, read and search files; rg also works when installed. Omit cwd to run at the workspace root. Output reports the exit status and bounded UTF-8 stdout/stderr. Complete streams end with complete_<stream>=true; truncated_<stream>=... means bytes were omitted. Truncated output may include a private full_output_path, or partial_output_path containing only a prefix if the 16 MiB per-stream spill limit was reached or storage failed. Spills are limited to 64 MiB / 32 files per resource owner and expire oldest-first or at owner shutdown.".into();
    def
}

/// Script prelude: make console and pipeline output UTF-8 so captured bytes
/// decode the same way under pwsh 7 and Windows PowerShell 5.1.
#[cfg_attr(not(windows), allow(dead_code))]
const UTF8_PRELUDE: &str = "try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding } catch {}\n";

/// `-EncodedCommand` payload: the prelude plus the model's source as UTF-16LE,
/// base64-encoded. Passing the script this way never goes through Windows
/// command-line quoting, so quotes, `$`, backticks and newlines arrive intact.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn encoded_command(source: &str) -> String {
    use base64::Engine;
    let bytes = UTF8_PRELUDE
        .encode_utf16()
        .chain(source.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Complete argument vector passed to `pwsh.exe` or `powershell.exe`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn powershell_arguments(source: &str) -> Vec<String> {
    vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-EncodedCommand".into(),
        encoded_command(source),
    ]
}

/// The first existing candidate: `pwsh.exe` anywhere on `PATH`, then
/// `powershell.exe` on `PATH`, then the inbox Windows PowerShell 5.1 under
/// `SystemRoot`. Relative `PATH` entries are ignored because they would
/// resolve against the workspace.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn select_powershell(
    path: Option<&std::ffi::OsStr>,
    system_root: Option<&std::ffi::OsStr>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    let directories = path
        .map(|path| {
            std::env::split_paths(path)
                .filter(|directory| directory.is_absolute())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for name in ["pwsh.exe", "powershell.exe"] {
        for directory in &directories {
            let candidate = directory.join(name);
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    let candidate = std::path::PathBuf::from(system_root?)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    exists(&candidate).then_some(candidate)
}

#[cfg(windows)]
pub(super) fn resolve_shell() -> Result<std::path::PathBuf, ToolError> {
    select_powershell(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("SystemRoot").as_deref(),
        std::path::Path::is_file,
    )
    .ok_or_else(|| {
        ToolError::new(
            "error unsupported_platform\nPowerShell is unavailable: install pwsh.exe or Windows PowerShell",
        )
    })
}

#[cfg(windows)]
pub(super) fn configure_command(command: &mut tokio::process::Command, source: &str) {
    command.args(powershell_arguments(source));
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn decode(encoded: &str) -> String {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        assert_eq!(bytes.len() % 2, 0, "UTF-16 code units are two bytes");
        let units = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units).expect("valid UTF-16LE")
    }

    #[test]
    fn encoded_command_is_utf16le_base64_of_the_prelude_and_exact_source() {
        let source = "Write-Output \"it's $env:USERNAME `n ünïcødé 🚀\"\r\nexit 3";
        let decoded = decode(&encoded_command(source));
        assert_eq!(decoded, format!("{UTF8_PRELUDE}{source}"));
        assert!(decoded.ends_with(source));
        // Known vector: "A" is 0x41 0x00 in UTF-16LE.
        let mut expected = UTF8_PRELUDE.encode_utf16().collect::<Vec<_>>();
        expected.push(u16::from(b'A'));
        let bytes = expected
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            encoded_command("A"),
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
    }

    #[test]
    fn arguments_are_non_interactive_profile_free_and_carry_no_raw_source() {
        let source = "Get-ChildItem -Recurse | Select-String -Pattern 'needle'";
        let arguments = powershell_arguments(source);
        assert_eq!(
            &arguments[..6],
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand"
            ]
        );
        assert_eq!(arguments.len(), 7);
        assert!(arguments.iter().all(|argument| !argument.contains(' ')));
        assert!(decode(&arguments[6]).ends_with(source));
    }

    #[test]
    fn pwsh_is_preferred_over_windows_powershell() {
        let root = std::env::temp_dir();
        let first = root.join("first");
        let second = root.join("second");
        let path = std::env::join_paths([&first, &second]).unwrap();
        let system_root = root.join("Windows");
        let inbox = system_root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let only = |present: Vec<std::path::PathBuf>| {
            move |candidate: &std::path::Path| present.iter().any(|path| path == candidate)
        };
        // pwsh later on PATH still wins over powershell.exe earlier on PATH.
        assert_eq!(
            select_powershell(
                Some(path.as_os_str()),
                Some(system_root.as_os_str()),
                only(vec![first.join("powershell.exe"), second.join("pwsh.exe")]),
            ),
            Some(second.join("pwsh.exe"))
        );
        assert_eq!(
            select_powershell(
                Some(path.as_os_str()),
                Some(system_root.as_os_str()),
                only(vec![first.join("powershell.exe"), inbox.clone()]),
            ),
            Some(first.join("powershell.exe"))
        );
        assert_eq!(
            select_powershell(
                Some(path.as_os_str()),
                Some(system_root.as_os_str()),
                only(vec![inbox.clone()]),
            ),
            Some(inbox)
        );
        assert_eq!(
            select_powershell(None, None, |_: &std::path::Path| true),
            None
        );
        // A relative PATH entry would resolve inside the workspace.
        assert_eq!(
            select_powershell(
                Some(std::ffi::OsStr::new("relative")),
                None,
                |_: &std::path::Path| true
            ),
            None
        );
    }

    #[test]
    fn the_model_visible_definition_never_mentions_bash() {
        let definition = definition(&BashTool);
        assert_eq!(definition.name, "powershell");
        let text = format!(
            "{} {} {}",
            definition.description, definition.parameters, PROMPT_SNIPPET
        )
        .to_ascii_lowercase();
        assert!(!text.contains("bash"), "{text}");
        assert!(text.contains("-noprofile -noninteractive"));
    }
}
