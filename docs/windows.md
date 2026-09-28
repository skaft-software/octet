# Windows

[Documentation](README.md) · [Tools](tools.md) · [Configuration](configuration.md) · [Terminal](terminal.md)

octet builds and runs as a native Windows x64 program (`octet.exe` and
`octet-host.exe`); WSL is not used or required. There is **no published native
Windows release or support claim yet**. Pull requests produce unsigned Windows
test builds, described below. A CI pass is not native GUI qualification:
Windows Terminal visual behavior and live Cua Driver computer use need the
manual checks on this page, run by a person at an unlocked desktop.

## Target and toolchain

| | |
| --- | --- |
| Target | `x86_64-pc-windows-gnu`, the target of the opt-in release-candidate packager (`scripts/package-octet-windows-release.py`) and of `scripts/generate-octet-release-metadata.py --include-windows-candidate` |
| Rust | 1.86.0, the pinned CI toolchain and MSRV |
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

Known limitation (blocker): octet publishes file changes with a no-replace
rename after verifying the target is unchanged. Windows has no atomic exchange
to complete that check-and-replace safely, so replacing an *existing* file
fails closed (`atomic conditional file replacement is unavailable`) instead of
risking an overwrite of a file that changed underneath it. Creating new files
works. Replacing existing ones does not, which includes:

- the agent's `edit` tool, and `write` to a file that already exists;
- rewriting existing private state, for example updating a saved
  custom-provider registry or refreshing a cached model inventory.

The Windows runner confirms the refusal at the file-publication layer; a safe
Windows replacement primitive is still to be designed.

## Pull-request test builds

The CI job `windows (x86_64-pc-windows-gnu)` runs on a native Windows runner. It
builds both executables, checks `--version`, `--help` and the `octet-host.exe`
hello handshake, and gates on the Windows-relevant suites: terminal detection
and frame writing, ConPTY scenarios against both the test and release
binaries, the renderer, private file access, Python extension launch, Windows
Bash process control, the Python SDK, the computer-use extension (no driver
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
| `BUILD-INFO.json` | Commit, CI run, rustc and linker versions, observed `--version` and hello output |
| `WINDOWS.md` | This page at that commit |
| `extensions/octet-computer-use/` | The version-matched source computer-use bundle, for the [attended procedure](../extensions/octet-computer-use/README.md#windows) |

These builds are unsigned and are **not releases**: they are not published,
not attached to a GitHub release, and cannot be packaged as a release
candidate (the packager requires a probe recorded by the immutable release
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
- Crossterm reads Windows console input records and reports no paste event.
  How a multi-line paste arrives through ConPTY is part of the manual checklist
  below; until it passes, prefer `@file` references for long text.
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
4. **Tool updates**: ask for a command that prints for several seconds (bash
   through Git Bash, or `--powershell`). Live tool output updates in place and
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
   Record whether it arrives as one composer entry or submits early.
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

## Shell selection order

Bash execution on Windows requires a **Bash-compatible** shell. `cmd.exe` and
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

## PowerShell

Use `--powershell` to add the opt-in `powershell` tool on Windows. It prefers
`pwsh.exe` over Windows PowerShell and runs with `-NoProfile -NonInteractive
-ExecutionPolicy Bypass`, bounded output, and process-tree cleanup. This is
additive: it does not replace Bash or relax the configured process/effect policy.
The flag is inert on non-Windows hosts.

## Interactive shell commands

The interactive `!<command>` local command uses the same Bash-compatible shell
resolution as the bash tool on Windows, and runs in its own Job Object so
interrupting it ends its whole process tree. On other platforms it runs
through `sh -c` (`crates/octet-coding-agent/src/modes/interactive.rs`).

## Extensions

Executable extensions whose entrypoint is a Python script (a `.py` file or a
`#!…python…` first line, as every first-party bundle uses) need Python 3 on
Windows, because Windows cannot execute a script by its `#!` line. octet runs
the script with the first interpreter found on `PATH`: the `py -3` launcher
(installed by the python.org installer), then `python3.exe` or `python.exe`,
with Microsoft Store app-execution aliases tried last. The extension otherwise
starts, and is supervised, exactly as on other platforms.

## Computer use

The [computer-use extension](../extensions/octet-computer-use/README.md) drives
Windows desktop applications through a locally installed Cua Driver. Nothing is
provisioned, downloaded, or granted automatically: `/computer-use setup`
installs the driver only when you run it, and CI never provisions a driver or
performs GUI actions. Its
[Windows procedure](../extensions/octet-computer-use/README.md#windows) covers
setup, status, and an opt-in live observe-act-verify smoke.

## Paths

Use Windows paths (for example `C:\Users\me\project`); octet resolves workspace
and display paths through the normal host layer. `--workspace`/the working
directory selection is unchanged from other platforms ([CLI](cli.md)).
