# octet computer-use

**Distribution: 0.8.1.** This bundle requires exactly octet 0.8.1.
Use the [version-matched installation](../../docs/installation.md) and the
[0.8.1 release record](../../docs/releases/v0.8.1.md) for signed assets and
public-install evidence. Windows and Linux desktop parity remain unqualified.

Operate native desktop applications on **macOS, Windows, and Linux** through a
locally installed [Cua Driver](https://github.com/trycua/cua), the MIT-licensed
open-source computer-use driver. This bundle provisions the driver on request,
connects to it over local stdio MCP, and republishes a reviewed subset of its
tools as octet tools.

The driver is a separate open-source project. This bundle does not vendor or
fork the driver runtime and does not include any part of OpenAI's CUA runtime.
It includes Cua's MIT-licensed cursor dotLottie source and compiled Octet theme
variants, with attribution under `themes/`. The driver itself is installed from
the published `cua-driver` Python distribution.

## Install and use

```console
octet extension install octet-computer-use
octet --enable-extension octet-computer-use
```

Then run the local setup command to provision the driver **and** install the
bundled model-colored cursor themes:

```console
/computer-use setup
```

The `computer_use_setup` agent tool provisions only the driver; it cannot
install themes. Theme installation is deliberately a trusted local setup
operation, not an agent tool. The bundle stores the provisioned driver under
`~/.octet/computer-use` and never touches your Python environment or a global
Python install. Cua Driver stores installed themes in its own per-user registry.
Setup is idempotent and reports a theme installation failure rather than
claiming that the personalized cursor is active.

`computer_use_status` reports whether the driver is present, its version, its
self-check, and your operating system's permission state. It never prompts.

## Grant operating-system permissions

The driver needs permission to observe and control the desktop. **This bundle
never grants an operating-system permission for you.** Grant it yourself:

- **macOS** — by default, the signed `/Applications/CuaDriver.app` host needs
  Accessibility and Screen Recording grants. If it is unavailable or cannot
  verify its grants, computer use reports `unavailable` rather than silently
  switching to cursorless direct mode. Set `OCTET_CUA_DESKTOP_HOST=0` only when
  you explicitly want direct mode, which inherits the grants of the app running
  octet and has no agent-cursor overlay. Restart octet after changing grants.
- **Windows** — the driver runs as your user; some stacks need the process to
  be interactive (an unlocked, visible session).
- **Linux** — a live display session plus AT-SPI 2 accessibility. X11/XWayland
  routes more widely than native Wayland.

Until the permission is granted, observation and action calls fail. That is
expected, and `computer_use_status` will say so along with the runtime in use.

### Runtime modes

`computer_use_status` reports the selected runtime and its binary.

| Runtime | When it is used | Needs | Agent cursor |
| --- | --- | --- | --- |
| `desktop-host` | Default on macOS when signed `/Applications/CuaDriver.app` and its grants are live | Grants given to that Cua Driver app | Yes, after cursor-state verification |
| `direct` | Non-macOS by default, or macOS only with `OCTET_CUA_DESKTOP_HOST=0` | The grants of the app running octet | No |
| `unavailable` | macOS host is missing or cannot prove live grants while host mode is selected | Install/authorize the signed Cua app, or explicitly opt into direct mode | No |

A missing or unusable macOS host never silently falls back when cursor support
is required. `OCTET_CUA_DESKTOP_APP` is an explicit developer override; no
`/Applications/OctetComputerUse.app` dependency is introduced by default.

### Developer override: build the octet host app (macOS)

This repository contains a small optional host app for development and testing.
It is not selected by default. To explicitly use it, build/install it and set
`OCTET_CUA_DESKTOP_APP=/Applications/OctetComputerUse.app` before starting octet.
The app supplies an AppKit main thread and starts `cua-driver serve` under its
own permission identity.

```console
bash extensions/octet-computer-use/build-host-app.sh --install
```

It installs `/Applications/OctetComputerUse.app` with the bundle identifier
`com.octet.computeruse`, builds from source, and signs with the first
codesigning identity it finds. Pass `--ad-hoc` for a throwaway build (its
permission grant resets on every rebuild) or `--identity "..."` to choose one.

The host installs to its own identity rather than Cua's `com.trycua.driver`, so
its permission grants cannot be confused with Cua's own app. It is never selected
unless named with `OCTET_CUA_DESKTOP_APP`; if selected but unavailable, macOS
computer use fails closed rather than falling back silently.

**Menu bar indicator.** Once running, the host shows a pointer glyph in the menu
bar. It is grey while idle and turns orange while an agent session is live, so
you can always tell when the agent has control of your screen. The state is read
from the driver's own session list, not from anything the agent reports.

Clicking it opens **Stop computer use**, which revokes every live session
immediately. That is the one-click kill switch: an agent that can drive your
desktop should never be able to do so invisibly.

The app is macOS-only and optional. It is not required for computer use, and it
is not installed by `octet extension install`.

#### How the macOS permission grant is made to stick

On macOS 27 (Tahoe) the stock `CuaDriver.app` from Cua AI can appear to lose
its Accessibility/Screen Recording grant on every daemon respawn: the System
Settings toggle reads ON, yet the driver reports the grants as false and every
action fails with `permissions_pending`. This is a known upstream issue
([hermes-agent#99732](https://github.com/NousResearch/hermes-agent/issues/99732),
duplicate of hermes-agent#78361, tracked against a stale-TCC-row bug in
trycua/cua).

The cause is the macOS TCC *responsible process* rule, not the app's signature.
When a driver daemon is started as a direct child of a terminal or agent, macOS
attributes the grant to that parent rather than to the driver app, so the grant
does not survive the process that owns it going away. The fix is to start the
daemon through LaunchServices so the app is its own responsible process, and to
clear a stale row before re-granting:

```console
# 1. clear a stale row if grants were previously granted and are misreported
tccutil reset Accessibility com.trycua.driver
tccutil reset ScreenCapture com.trycua.driver
# 2. launch the daemon through LaunchServices, never as a child
open -n -g -a CuaDriver --args serve
# 3. grant (a macOS prompt appears; approve it)
cua-driver permissions grant
# 4. confirm
cua-driver permissions status --json
```

After this, `permissions status` reports `source.attribution: "driver-daemon"` and
the grants survive respawns and reboots. The extension uses the signed Cua host's
daemon permission probe and fails closed if the selected host cannot establish them.

## Windows

The bundle runs on native Windows (no WSL). Windows cannot execute a script by
its `#!` line, so octet starts the bundle with Python 3 from `PATH` (the
python.org installer's `py` launcher, or `python.exe`); install Python 3 first.
See [Windows](../../docs/windows.md#extensions).

**What CI verifies.** The Windows CI job runs this bundle's full test suite on
a native Windows runner **with no Cua Driver installed**: status reports
`not set up`, every driver tool fails closed without starting a process or
provisioning anything, and screenshot staging and key storage keep their
private-file guarantees (junctions and redirected parents are rejected, the
optional Jev key gets a current-user-only ACL). The suite's live-driver tests
skip. CI never provisions a driver, grants a permission, or performs a GUI
action, so it does **not** qualify live Windows computer use.

**Attended setup.** On an unlocked, visible desktop session (not a locked or
minimized remote session), from the extracted pull-request build, which
carries this bundle under `extensions\octet-computer-use` (from a checkout,
use the built `octet.exe` and the checkout's `extensions` directory):

```powershell
.\octet.exe --extension-dir .\extensions --enable-extension octet-computer-use
```

Then, inside octet, run `/computer-use setup` (this downloads the published
`cua-driver` wheel from the configured package index into
`%USERPROFILE%\.octet\computer-use`, and only when you run it) and
`/computer-use status`. The driver runs as your user; Windows needs no
separate grant, but it cannot drive windows of elevated (administrator)
applications from a non-elevated octet, nor the secure desktop (UAC prompts,
the lock screen). `--safe-mode` keeps executable extensions stopped, so run
this without it, inside a boundary you chose, and set `OCTET_CUA_CONFIRM=1`
to approve each effectful action.

**Opt-in live smoke.** After setup, a person watching the desktop can run a
scripted observe-act-verify pass through the same extension tools the model
uses. It launches Notepad, finds its window, types a unique marker into it,
reads the window state back to verify the marker, deletes it and verifies that,
then captures a desktop screenshot; Notepad is left open. It never provisions,
grants permissions, or types credentials, and it refuses to run under `CI`,
without the confirmation flag, or without an interactive terminal:

```powershell
cd extensions\octet-computer-use
python tests\live_smoke.py --i-have-an-unlocked-desktop --report live-smoke.json
```

`OCTET_CUA_DRIVER_BINARY` can point it at a driver installed elsewhere. Record
the Windows build, the `cua-driver` version from status, and the report.

**Model-driven check (optional).** With a provider configured and
`OCTET_CUA_CONFIRM=1`, ask octet: *"Use computer use to open Notepad, type
'octet windows check', then read the window back and confirm the text is
there."* Approve each action and confirm the transcript shows an observation
before and after each effectful call.

## Confirmation and safe mode

octet's security model ([SECURITY.md](../../SECURITY.md)) is that **full access -
the default - runs with your own permissions and does not ask**, and that
`--safe-mode` is the mode that asks before every effectful action. Computer use
inherits that: under full access an agent drives the desktop unattended, and the
agent cursor is the standing signal that it currently has control.

To ask before every effectful action instead, opt in:

```console
OCTET_CUA_CONFIRM=1 octet
```

`OCTET_CUA_CONFIRM=0` turns the gate off explicitly, even under a gated profile.

**This is the boundary that matters.** octet is not a sandbox: an agent with
computer use can read what is on your screen and press what it can reach, exactly
as you can. There is no per-app scoping and no incognito-window filtering, because
macOS gives a Screen Recording grantee no way to hide a window it can see. If that
matters, run octet inside a VM.

## What the agent can do

| Tool | Driver tool | Notes |
| --- | --- | --- |
| `computer_use_status` | local | Provisioning, version, permissions, self-check. |
| `computer_use_setup` | local | Install the driver from the package index. |
| `computer_use_installed_apps` | `list_apps` | Read-only. |
| `computer_use_windows` | `list_windows` | Read-only. |
| `computer_use_window_state` | `get_window_state` | Read-only. Accessibility tree + screenshot. |
| `computer_use_desktop_state` | `get_desktop_state` | Read-only. Full-screen capture. |
| `computer_use_click` | `click` | Effectful; follows Octet's effect-confirmation policy. |
| `computer_use_type_text` | `type_text` | Effectful; follows Octet's effect-confirmation policy. |
| `computer_use_press_key` | `press_key` | Effectful; follows Octet's effect-confirmation policy. |
| `computer_use_hotkey` | `hotkey` | Bounded key chord to a named window. |
| `computer_use_invoke_menu` | `invoke_menu` | Exact accessible menu path; ambiguous items fail closed. |
| `computer_use_move_cursor` | `move_cursor` | Moves only the visible overlay, within a named window. |
| `computer_use_scroll` | `scroll` | Effectful; follows Octet's effect-confirmation policy. |
| `computer_use_launch_app` | `launch_app` | Effectful; follows Octet's effect-confirmation policy. |
| `computer_use_start_session` | `start_session` | Starts/switches the session used by later driver actions. |
| `computer_use_end_session` | `end_session` | Ends the active action session. |

The driver publishes a much larger catalog (58 tools on macOS at the time of
writing). This bundle republishes a small, reviewed subset so octet's tool
catalog stays stable across upstream releases. Only reviewed arguments are
forwarded; an unrecognised argument is dropped rather than passed through.

## Safety model

- **Effect confirmation follows Octet's policy.** In a gated profile (or with
  `OCTET_CUA_CONFIRM=1`), effectful calls require approval and declined,
  unavailable, or failed confirmation denies dispatch. Full-access mode does not
  add per-action prompts by default; `OCTET_CUA_CONFIRM=0` explicitly disables
  them even in a gated profile. Unknown driver tools are treated as effectful.
- **Read-only observations.** Observation tools use the driver's read-only
  classification and do not prompt; actual permission or capture errors remain
  visible to the caller.
- **Targeted cursor feedback.** On the macOS host, cursor enablement and motion
  are verified before any tool call is reported ready. The cursor uses a short,
  straight 80 ms glide without curved turns and hides after 5 seconds of
  inactivity. The public cursor-move tool requires a window target and
  window-local coordinates and cannot move the real OS pointer. The bundled
  Octet-inspired dotLottie pointer is slightly smaller than Cua's default and
  uses the same stable, model-adaptive prompt color as Octet's TUI. Each model
  family has a compiled variant; the extension selects the installed variant
  at the next tool call after a model switch. If themes have not been installed,
  it uses Cua's default cursor and reports that fact. Cua's own session badge
  remains session-colored. Custom user-defined TUI palettes are not compiled
  into these fixed variants.
- **One action session.** Cursor setup, public session start/end, and eligible
  driver actions share one driver session; ending it clears the binding so a
  subsequent action must establish a new session.
- **Bounded outputs.** Text and structured driver output are bounded below
  Octet's 256 KiB structured-content limit while preserving window IDs, snapshot
  IDs, element tokens, and coordinates. Screenshots are published as artifacts,
  not inlined as base64.
- **Least environment.** The driver receives only reviewed, non-secret desktop
  session variables (`DISPLAY`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and the
  documented equivalents). Provider tokens, `PATH` overrides, and arbitrary
  ambient variables are not forwarded.
- **No secrets, no silent installs.** The bundle never types credentials for you
  and never downloads a driver outside the standard package install you trigger.
  OS permissions are always yours to grant.
- **Re-snapshot before acting.** Element indices are replaced by the next window
  snapshot, so read state before each indexed action. The bundled skill documents
  the full observe-act-verify loop.

## Relationship to octet-browse

`octet-browse` is [deprecated but still installable](../octet-browse/README.md#deprecation).
It drives an isolated, Octet-owned Chromium with manual authentication, which
remains the safer surface for authenticated page work. This bundle drives your
actual desktop, so it can see anything you can see — including a browser you
already have open. Prefer Browse for anything involving a login or a saved
session.

## Tests

The suite runs without a driver; the integration tests skip automatically when
no local driver is present.

```console
PYTHONPATH=.:vendor:tests python3 -m unittest discover -s tests -p 'test_*.py'
```

On Windows (PowerShell), the path separator is `;`:

```powershell
$env:PYTHONPATH = '.;vendor;tests'; python -m unittest discover -s tests -p 'test_*.py'
```

`tests/live_smoke.py` is not part of the suite; see [Windows](#windows).

To include the live driver tests, point at an installed binary:

```console
PYTHONPATH=.:vendor:tests OCTET_CUA_DRIVER_BINARY=/path/to/cua-driver \
  python3 -m unittest discover -s tests -p 'test_*.py'
```
