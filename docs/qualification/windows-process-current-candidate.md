# Windows process boundary candidate

Status: source-only candidate; native Windows acceptance is not established.

## Boundary implemented

`bash` now has a Windows execution route. It keeps the existing effect admission,
argument validation, workspace `cwd` resolution, sanitized environment, bounded
stdout/stderr capture, live progress, per-call timeout, and cancellation-by-drop
contract. The complete command is passed as one `-c` argument to one selected
Bash-compatible executable.

Windows shell selection is explicit: `sandbox.shell_path` wins; otherwise the
host checks Git for Windows `bash.exe` under `ProgramFiles`/`ProgramFiles(x86)`
and then `bash.exe` on `PATH`. `COMSPEC`, `$SHELL`, `cmd.exe`, PowerShell, and
legacy implicit WSL `bash.exe` are not fallbacks. An explicit path is an
operator-provided Bash-compatible contract and is not rewritten by the host.
Missing Bash fails closed with `unsupported_platform` rather than interpreting
Bash source in another language.

Windows child processes are created suspended, assigned to a private Job Object,
and resumed only after assignment. `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and
`TerminateJobObject` cover descendants for timeout, cancellation, extension
shutdown, host watchdog cleanup, and failed registration. This uses an exact
kernel job handle, not `taskkill`, a PID-only kill, or a process-wide unrelated
process scan. Assignment failure fails closed. Unix process-group and identity
tracking code remains unchanged.

Executable extensions use the same suspended Job Object launch boundary. Their
existing enablement, trust, manifest validation, protocol, environment, cwd, and
resource gates are unchanged; the Windows job is cleanup containment only.

## Qualification surface

The new cfg-gated integration fixture is
`crates/octet-agent/tests/windows_process_current.rs`. It exercises explicit
Git Bash `-c` semantics plus a workspace-relative cwd, and bounds an infinite
Bash loop through the Job Object timeout path. The fixture skips with a bounded
diagnostic when Git Bash is not installed; this is not a passing native
qualification result.

Proposed checks (not run in this source-only lane):

- On native Windows with Git for Windows installed, run the focused
  `octet-agent` integration test with `OCTET_WINDOWS_BASH_PATH` set to the
  reviewed `bash.exe`, then repeat with default discovery and with a shell path
  containing spaces.
- Exercise timeout and cancellation while a Bash child starts a descendant;
  verify the root and descendant exit and no unrelated process is affected.
- Start a trusted API 0.1/0.2/0.3 extension whose entrypoint spawns a child;
  verify initialize/shutdown, protocol failure, reload, and watchdog cleanup
  terminate the complete extension job tree.
- Run the existing Unix unit/integration suites to establish unchanged Unix
  process-group, detached-descendant, output-bound, effect, and extension
  lifecycle behavior.
- Run the locked Windows Cargo checks only through the coordinator's sole
  `verify-rust` ownership. The centralized dependency phase must update the
  locked dependency graph for `windows-sys 0.61` with
  `Win32_System_JobObjects`; this lane did not edit `Cargo.lock`.
- Centralized cross-checking needs the workspace compiler floor Rust `1.86`,
  the `x86_64-pc-windows-gnu` target standard library, and a GNU Windows
  linker/CRT (MinGW or Zig `cc` configured for `x86_64-windows-gnu`). The
  existing macOS Zig and target-std installation is availability evidence only;
  this lane did not invoke Cargo or a compiler.

## Limits and handoffs

This candidate does not prove a Windows build, native Windows process behavior,
Git Bash availability, terminal behavior, packaging, installer/updater assets,
Windows shell configuration UI/CLI wiring, extension installation/lifecycle
acceptance, live provider behavior, or physical process-tree cleanup. The
Windows shell setting must be wired/documented by the CLI/config owner; the
current owned boundary only consumes the existing `SandboxConfig.shell_path`.
Unsupported-command messaging and release/package/installer work remain with
their owners. A user must provide native Windows access after the Linux artifact
is frozen; macOS, Linux, WSL, cross-compilation, or a Windows-target compile
cannot substitute for that gate.

Reference behavior was compared read-only against Codex HEAD
`3d3df0a0cad5d3d8d3340b633787e9dd304ea463` Job Object handling and the pinned
Pi HEAD `08dc60bc52d89d6823a9738cc90b1916e5e446e5` (the Pi 0.84.4
compatibility target). A Pi 0.85.1 checkout is a different comparison point,
not the pinned target or source authority. No reference source was copied.
Codex root licensing is Apache-2.0; its pty Job Object file records a separate
MIT-derived component. This candidate uses only independent Windows API calls
and retains no copied reference implementation.
