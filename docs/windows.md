# Windows

[Documentation](README.md) · [Tools](tools.md) · [Configuration](configuration.md) · [Terminal](terminal.md)

octet builds and runs as a native Windows x64 program (`octet.exe` and
`octet-host.exe`); WSL is not used or required. The
[v0.9.0 release page](https://github.com/skaft-software/octet/releases/tag/v0.9.0)
records actual MSVC artifact availability and qualification. Pull requests
produce unsigned GNU test builds, described below; these do not qualify the
MSVC release archive. Automated terminal checks are not physical-display or
live Cua Driver acceptance: those need the manual checks on this page, run by a
person at an unlocked desktop.

## Target and toolchain

| | |
| --- | --- |
| Target | `x86_64-pc-windows-gnu`, the target of the opt-in release-candidate packager (`scripts/package-octet-windows-release.py`) and of `scripts/generate-octet-release-metadata.py --include-windows-candidate` |
| Rust | 1.88.0, the pinned CI toolchain and MSRV |
| Linker and C compiler | MinGW-w64 GCC (`x86_64-w64-mingw32-gcc`); `ring`, bundled SQLite, zstd, and tree-sitter compile C |
| Runtime dependencies | None beyond Windows system DLLs: the MinGW runtime is linked statically |

Build from a checkout on Windows with Rust (rustup) and a MinGW-w64 GCC on
`PATH`, for example MSYS2's `mingw-w64-x86_64-gcc` package
(`C:\msys64\mingw64\bin`):

```powershell
rustup target add x86_64-pc-windows-gnu
cargo build --release --locked --target x86_64-pc-windows-gnu -p octet-coding-agent --bins
.\target\x86_64-pc-windows-gnu\release\octet.exe --version
```

Clone with `git clone -c core.autocrlf=false …`: Git for Windows converts
checkouts to CRLF by default, and the repository's test fixtures and golden
files are LF.

From Linux, `rustup target add x86_64-pc-windows-gnu` plus the `mingw-w64`
package allows `cargo check`/`cargo build --target x86_64-pc-windows-gnu`. That
only proves the code compiles for Windows; it runs nothing on Windows.

Run octet from a non-elevated terminal. octet keeps private state in files that
must be owned by the current user. It creates its own private files and
directories with that owner explicitly, but on Windows Server (and wherever the
"default owner for objects created by members of the Administrators group"
policy is set to the Administrators group) anything else created from an
elevated session, such as a directory made by Explorer, a script, or a test
harness, is owned by that group. octet refuses such objects rather than
treating them as private.

Replacing an *existing* file uses a pin, then displace, then publish
sequence (`secure_fs`): the target is pinned (opened with `DELETE` while
denying write and delete sharing), re-verified by identity and bytes, renamed
aside to `.octet-old-*`, and the staged file is published under the target
name with a no-replace rename. No step overwrites a name it did not vacate
itself: a writer that recreates the name in the displacement window wins
(`Changed`, with the original kept under `.octet-old-*`), and readers can
briefly see the target missing. A file held open for writing, even by a cooperative holder
that shares delete access, reports in use: allowing those writes could silently
discard an update made after the final comparison. Close the writable handle
before replacement. The two-rename interruption gap remains a qualification
blocker for both credentials and workspace files; error restoration is not crash
recovery. On-disk spelling (case and 8.3 aliases) and
read-only/hidden/system attributes carry over to the replacement.

## Pull-request test builds

The CI job `windows (x86_64-pc-windows-gnu)` runs on a native Windows runner. It
builds both executables, checks `--version`, `--help` and the `octet-host.exe`
hello handshake, and gates on the Windows-relevant suites: terminal detection
and frame writing, ConPTY scenarios against both the test and release
binaries, the renderer, private file access, partial-assistant journal settlement
and genuine interrupted-reasoning recovery, Python extension launch, Windows
Bash and PowerShell process control, the Python SDK, the computer-use extension (no driver
installed), and release tooling. It starts the packaged executables with only
the Windows system directories on `PATH` and as a freshly created standard
(non-administrator) account, and uploads an artifact named
`octet-pr-<commit>-x86_64-pc-windows-gnu` for 14 days. For a pull request,
`<commit>` is the merge commit CI built, which `BUILD-INFO.json` records.

The ConPTY scenarios (`cargo test -p octet-coding-agent --test
windows_conpty`) read the current account's octet profile, because Windows
resolves it through the known-folder API rather than an environment variable.
Run them from an account with no configured provider, as on a CI runner: they
continue through first-run provider setup without writing provider data.

The job also runs the whole workspace test suite as a **non-gating baseline**
and publishes per-target results in the job summary. Most of that suite had
never run on Windows: the runner account is an elevated administrator on
Windows Server (see above), and many tests assume Unix paths, file modes, or
signals. Those failures are recorded, not hidden, and are not yet a Windows
support claim either way.

The artifact contains:

| File | Contents |
| --- | --- |
| `octet.exe`, `octet-host.exe` | The native binaries built from that commit |
| `SHA256SUMS` | SHA-256 of both executables |
| `BUILD-INFO.json` | Commit and tree, CI run, rustc and linker versions, observed `--version` and hello output |
| `WINDOWS.md` | This page at that commit |
| `extensions/octet-computer-use/` | The version-matched source computer-use bundle, for the [attended procedure](../extensions/octet-computer-use/README.md#windows) |

Manually dispatched [dogfood candidates](distribution.md#unsigned-dogfood-candidates)
also include all five official extension bundles and extracted payloads.
The experimental Pi compatibility adapter is not shipped in v0.9.0. Their
root checksum manifest covers those extracted files as well as the binaries,
metadata and archives; verify it before running the extracted extensions.

Ordinary PR builds are unsigned and are **not releases**: they are not published,
not attached to a GitHub release, and cannot be repackaged as stable release
archives (the packager requires a probe recorded by the immutable release
workflow tag). Windows SmartScreen may warn about an unsigned download.

To test one from an ordinary Windows account, without WSL:

1. Open the pull request's **Checks** tab, choose the `CI` run, and download
   the `octet-pr-…-x86_64-pc-windows-gnu` artifact from the run summary
   (signed-in GitHub users only). With the GitHub CLI:
   `gh run download <run-id> -R skaft-software/octet -n <artifact-name>`.
2. Extract the zip, for example to `%USERPROFILE%\octet-pr`.
3. In Windows Terminal (PowerShell), verify and run it:

   ```powershell
   cd $env:USERPROFILE\octet-pr
   Get-FileHash -Algorithm SHA256 .\octet.exe, .\octet-host.exe   # compare with SHA256SUMS
   Unblock-File .\octet.exe, .\octet-host.exe                      # clears the download mark
   .\octet.exe --version
   '{"protocol_version":1,"request_id":"t","command":"hello"}' | .\octet-host.exe
   .\octet.exe --safe-mode
   ```

The binaries do not install themselves or change `PATH`. octet keeps its state
under `%USERPROFILE%\.octet`, as it keeps `~/.octet` elsewhere.

## Terminal

octet chooses its frontend from the console it is attached to. Windows Terminal
and conhost do not export `TERM`, so on Windows the console itself is the
contract:

| Where octet runs | Frontend |
| --- | --- |
| PowerShell or cmd in Windows Terminal (`WT_SESSION` set) | Interactive: Unicode glyphs, truecolor, italics, OSC 8 links |
| PowerShell or cmd in a classic console window (conhost) | Interactive: ASCII glyphs, 256 colors, no italics or links |
| Git Bash, MSYS2 or Cygwin inside Windows Terminal | Interactive, using the shell's exported `TERM`/`LANG` plus the Windows Terminal profile |
| mintty (Git Bash's own window) without ConPTY | Plain: its pty pipes are not a console. Run octet from Windows Terminal instead |
| Redirected standard input or output | Print or plain mode, with no terminal control sequences |

A console counts as interactive only when standard input and output are both
console handles and virtual-terminal (VT) output processing can be enabled.
`--plain`, `--color` and `NO_COLOR` behave as on other platforms. See
[terminal](terminal.md) for the frontends themselves.

While the interactive frontend owns the console, octet:

- enables VT output and requests delayed end-of-line wrapping
  (`DISABLE_NEWLINE_AUTO_RETURN`), so a row that fills the last column does not
  push later frames one row down on older console hosts. On exit it removes the
  delayed-wrap flag again and leaves VT output enabled, as crossterm does;
- writes each rendered frame with a single `WriteConsoleW` call. Rust's
  standard console writer would split a large frame into many small console
  writes, which ConPTY can forward, and Windows Terminal can paint, separately;
- brackets frames with synchronized-output markers (`CSI ? 2026 h/l`). octet
  does **not** assume the console host honors them: hosts without support
  ignore the markers, and whether ConPTY forwards them and Windows Terminal
  holds painting is not established by CI. The ConPTY test logs how many
  markers the runner's console host forwarded;
- restores the console after a normal exit, a panic, `Ctrl+Break`, closing the
  window, logoff, or shutdown. `Ctrl+C` is an ordinary key while the
  interactive frontend is active and cancels work as elsewhere; in plain and
  print modes it takes the same coordinated shutdown as the Unix `SIGINT`.

Known differences on Windows:

- The Kitty keyboard protocol is unavailable. Modifiers come from console input
  records instead; `Ctrl+J` always inserts a newline, and whether `Shift+Enter`
  does in a given host is part of the checklist. Shortcuts that differ on
  Windows are listed in [commands](commands.md).
- Crossterm reads Windows console input records and reports no paste event,
  so a pasted line break arrives as `Enter`: in Windows Terminal a multi-line
  paste submits once per line. Use `@file` references for long or multi-line
  text.
- octet does not query the terminal background (OSC 11) on native Windows. The
  console host turns the reply into key records, and Windows Terminal's reply
  reached the composer as typed text. Auto uses its neutral fallback; choose
  `--theme dark`, `--theme light` or `/theme` instead.
- Inline images are shown only on Kitty-compatible terminals, so tool images
  use the text fallback in Windows Terminal.
- Resizing replays the retained transcript once, as on other platforms.

### Windows Terminal visual checklist

CI cannot see pixels. Before claiming Windows Terminal support, a person should
record these results for a pull-request build on an unlocked desktop, noting
the Windows build, the Windows Terminal version (**Settings → About**) and the
shell. Run each item in PowerShell, and where noted in cmd and Git Bash inside
Windows Terminal. Use a disposable workspace and `--safe-mode`.

1. **Startup**: `.\octet.exe --safe-mode` shows the startup card and composer
   with box-drawing glyphs, colors, and no stray escape text (for example
   `]11;rgb:` or `[?2026h`) in the composer. Repeat in cmd and Git Bash.
2. **Typing and editing**: type, move with arrows, `Home`/`End`, delete words;
   each keystroke appears once, and the cursor stays in the composer.
   `Ctrl+J` inserts a newline; record whether `Shift+Enter` does too.
3. **Streaming**: with a configured provider, ask for a long answer. The
   transcript grows without full-screen flashes, flicker of earlier rows, or
   the composer jumping.
4. **Tool updates**: ask for a command that prints for several seconds
   (the default `powershell` tool, or bash through Git Bash with
   `--shell-path`). Live tool output updates in place and
   finishes with a clean final block.
5. **Scrolling**: during and after streaming, use the mouse wheel and
   `PageUp`/`PageDown`; history is intact and the live tail returns cleanly.
   Repeat with `--mouse app`.
6. **Resize**: drag the window narrower and wider, and maximize and restore,
   while idle and while streaming. The transcript reflows once per resize,
   without duplicated or missing rows.
7. **Completion and pickers**: type `/` and `@` to open completion menus, and
   open `/model`. Menus draw and close without leaving residue.
8. **Paste**: paste a multi-line block with `Ctrl+V` and with a right-click.
   Record whether it arrives as one composer entry or submits early. In 0.8.2
   release testing, Windows Terminal submitted once per line.
9. **Cancellation**: press `Esc` during a response and `Ctrl+C` during a long
   tool run; work stops and the frontend stays usable.
10. **Exit and restoration**: exit with `Ctrl+D` or `/exit`, and separately
    close the tab mid-response and press `Ctrl+Break`. The prompt returns on a
    fresh line with a visible cursor, typed characters echo, and nothing is
    left in raw mode.
11. **Plain and redirected output**: `.\octet.exe --plain --safe-mode`, then,
    with a configured provider, `.\octet.exe -p "hello" | Out-File out.txt`;
    `out.txt` contains no escape sequences.
12. **conhost**: repeat items 1, 3, 6 and 10 in a classic console window
    (`conhost.exe powershell`); expect the ASCII profile.

The following v0.9.0 release-blocker checks also require the exact candidate on
an attended Windows desktop; source changes and deterministic CI fixtures do
not qualify them:

- **Recovery**: complete at least three reasoning-enabled turns, then reopen
  and complete another. No successfully completed turn is shown as recovered
  output. Interrupt an actual reasoning stream, reopen, and verify only that
  interrupted prefix appears as recovered output, exactly once and separately
  from the next answer.
- **Local setup**: `/setup` offers editing an existing local configuration,
  creating a new one, and cancellation immediately. Edit once, confirm once,
  and use the updated model without restarting. Cancellation preserves the
  existing configuration; credentials are neither displayed nor needlessly
  re-entered.
- **Model facts**: compare selected model reasoning options/default and context
  with the configured server's assertions, including loaded context versus
  model maximum. Refresh metadata after changing the server's loaded context.
  Repeat with a sparse model list: absent facts must remain unknown/actionable,
  not guessed effort levels or a detected 131K limit.
- **Fullscreen Settings**: verify the default fullscreen mode, persisted
  `/settings` mode changes, restart and explicit invocation precedence. Repeat
  scrolling, resize, cancellation, exit restoration and plain/piped checks in
  each mode. Check the complete pinned Pi Settings row matrix against actual
  behavior, not only the presence of labels.

## PowerShell

On Windows the model's shell is the `powershell` tool. The default tool surface
is exactly `read`, `edit`, `write` and `powershell`: the `bash` tool is not
registered, and neither the tool list, the tool descriptions nor the system
prompt mention Bash, so the model does not spend calls on Bash syntax that
would fail. The system prompt says:

- `Configured core tools: read, edit, write, powershell.`
- `Use powershell for shell commands and repository content search; commands
  run in PowerShell, so use PowerShell syntax (; between statements,
  $env:NAME for environment variables). Prefer rg when available, otherwise
  use Get-ChildItem -Recurse -File and Select-String.`
- `Relative tool paths and powershell without an explicit cwd resolve from the
  workspace root.`

The tool takes the same arguments as `bash` (`command`, optional `cwd` and
`timeout_ms`) and shares its implementation: effect classification, output
limits and spill files, `bash_timeout_secs`, cancellation, Job Object
process-tree cleanup, the workspace working directory, and the sanitized child
environment. It runs `pwsh.exe` (PowerShell 7) when it is on `PATH`, otherwise
`powershell.exe` on `PATH`, otherwise the inbox
`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`, with
`-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand`.
The command travels as UTF-16LE base64, so quotes, `$`, backticks and newlines
are never re-quoted, and a short prelude sets `[Console]::OutputEncoding` and
`$OutputEncoding` to UTF-8 so output decodes the same under both versions.

Approval is the same as for `bash`, with one difference: under
`--effect-policy controlled` every `powershell` call asks first, because the
known-safe command list parses only Bash syntax. Under `unsafe_host` (the
default) and `controlled_bash_approval`/`--safe-mode` the two shells behave
identically. `--no-process`, `--no-shell` and `--exclude-tools powershell`
remove it.

### Using Bash on Windows instead

Bash is used only when you ask for it:

- set a Bash-compatible `shell_path` (`--shell-path`, `OCTET_SHELL_PATH`, or
  the `shell_path` setting). The default surface then has `bash` instead of
  `powershell`; add `--powershell` to keep both;
- or name it in an explicit allowlist, such as
  `--tools read,edit,write,bash` (add `powershell` to keep both).

### Extensions and Bash-named tools

- Subagent and Pi extension child sessions that request the standard `bash`
  tool get `powershell` when it is the parent's only shell, so such a child
  still has a shell. Commands the child writes must then be PowerShell.
- Pi extensions that override the `bash` builtin, or that wait for `bash` tool
  results (`isBashToolResult`), see no `bash` calls on a default Windows
  surface; shell results arrive under `powershell` (`isPowerShellToolResult`).
  Nothing fails at startup.
- codemode exposes the tools the host enables, so on Windows it offers
  `tools.powershell` with the same structured results and no `tools.bash`; a
  script calling `tools.bash` gets an ordinary unknown-tool rejection.
- The interactive `!<command>` escape is a user command, not a model tool, and
  still uses the Bash-compatible shell described below.

## Shell selection order

When the `bash` tool is enabled on Windows (see above), it requires a
**Bash-compatible** shell. `cmd.exe` and
PowerShell are never implicit fallbacks; when no Bash-compatible shell is found
the tool fails closed with an `error unsupported_platform` message.

`crates/octet-agent/src/tools/bash.rs` resolves the shell in this order:

1. an explicit `shell_path` — `--shell-path PATH`, `OCTET_SHELL_PATH`, or the
   `shell_path` configuration key ([configuration](configuration.md));
2. `%ProgramFiles%\Git\bin\bash.exe` (and the `ProgramFiles(x86)` variant);
3. `bash.exe` or `bash` on `PATH` (Cygwin, MSYS2, …), **excluding** the legacy
   WSL `bash.exe` location.

For most users [Git for Windows](https://git-scm.com/download/win) is enough. An
explicit path must still provide Bash-compatible `-c` semantics; octet does not
reinterpret it as `cmd.exe` or PowerShell. Commands run in a private Job Object,
so cancellation and timeouts end the whole process tree.

## Interactive shell commands

The interactive `!<command>` local command uses the same Bash-compatible shell
resolution as the bash tool on Windows, and runs in its own Job Object so
interrupting it ends its whole process tree. On other platforms it runs
through `sh -c` (`crates/octet-coding-agent/src/modes/interactive.rs`).

## Extensions

Executable extensions whose entrypoint is a Python script (a `.py` file or a
`#!…python…` first line, as the computer-use, MCP, subagents and web-search
bundles use) need Python 3 on
Windows, because Windows cannot execute a script by its `#!` line. octet runs
the script with the first interpreter found on `PATH`: the `py -3` launcher
(installed by the python.org installer), then `python3.exe` or `python.exe`,
with Microsoft Store app-execution aliases tried last. The extension otherwise
starts, and is supervised, exactly as on other platforms. Dogfood codemode
bundles instead contain native `bin/codemode.exe`; the Pi adapter uses Node
22.19+ and its bundled dependencies. Neither installation nor extraction enables
an extension or authorizes Pi factory execution.

## Computer use

The [computer-use extension](../extensions/octet-computer-use/README.md) drives
Windows desktop applications through a locally installed Cua Driver. Nothing is
provisioned, downloaded, or granted automatically: **Set up computer use**
(in `/extensions` → octet-computer-use) installs the driver only when you run it, and CI never provisions a driver or
performs GUI actions. Its
[Windows procedure](../extensions/octet-computer-use/README.md#windows) covers
setup, status, and an opt-in live observe-act-verify smoke.

## Paths

Use Windows paths (for example `C:\Users\me\project`); octet resolves workspace
and display paths through the normal host layer. `--workspace`/the working
directory selection is unchanged from other platforms ([CLI](cli.md)).
