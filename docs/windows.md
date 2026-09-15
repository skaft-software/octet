# Windows setup

[Documentation](README.md) · [Tools](tools.md) · [Configuration](configuration.md)

octet's Bash execution on Windows requires a **Bash-compatible** shell. `cmd.exe`
and PowerShell are never implicit fallbacks; when no Bash-compatible shell is
found the tool fails closed with an `error unsupported_platform` message.

## Shell selection order

`crates/octet-agent/src/tools/bash.rs:386` resolves the shell in this order:

1. an explicit `shell_path` — `--shell-path PATH`, `OCTET_SHELL_PATH`, or the
   `shell_path` configuration key ([configuration](configuration.md));
2. `%ProgramFiles%\Git\bin\bash.exe` (and the `ProgramFiles(x86)` variant);
3. `bash.exe` or `bash` on `PATH` (Cygwin, MSYS2, …), **excluding** the legacy
   WSL `bash.exe` location.

For most users [Git for Windows](https://git-scm.com/download/win) is enough. An
explicit path must still provide Bash-compatible `-c` semantics; octet does not
reinterpret it as `cmd.exe` or PowerShell.

## PowerShell

The agent runtime implements an opt-in `powershell` tool
(`crates/octet-agent/src/tools/powershell.rs`) that prefers `pwsh.exe` over
Windows PowerShell and runs with `-NoProfile -NonInteractive
-ExecutionPolicy Bypass`, bounded output, and process-tree cleanup. It is **not**
part of the coding agent's model-visible tool allowlist yet
(`crates/octet-coding-agent/src/config.rs:119` lists `read`, `search`, `edit`,
`write`, `bash`, and the skill tools), so on Windows today the model uses `bash`
with a Bash-compatible shell. This is tracked as parity item `4.6` (opt-in
PowerShell + Windows CI evidence, currently `Pending`) in
[docs/parity/README.md](parity/README.md).

## Interactive shell commands

The interactive `!<command>` local command runs through `sh -c`
(`crates/octet-coding-agent/src/modes/interactive.rs:5838`). On Windows this
requires an `sh` on `PATH`; without one, use the `bash` tool instead.

## Paths

Use Windows paths (for example `C:\Users\me\project`); octet resolves workspace
and display paths through the normal host layer. `--workspace`/the working
directory selection is unchanged from other platforms
([CLI](cli.md)).
