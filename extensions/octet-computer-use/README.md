# octet computer-use

**Distribution: 0.8.2.** This bundle requires exactly octet 0.8.2.
Use the [version-matched installation](../../docs/installation.md) and the
[0.8.2 release record](../../docs/releases/v0.8.2.md) for signed assets and
public-install evidence. Windows and Linux desktop parity remain unqualified.

Operate native desktop applications on **macOS, Windows, and Linux** through a
locally installed [Cua Driver](https://github.com/trycua/cua), the MIT-licensed
open-source computer-use driver. This bundle provisions the driver on request,
connects to it over local stdio MCP, and republishes a reviewed subset of its
tools as octet tools.

The driver is a separate open-source project. This bundle does not vendor or
fork the driver runtime and does not include any part of OpenAI's CUA runtime.
It includes Cua's MIT-licensed cursor dotLottie source and compiled Octet theme
variants, with attribution under `themes/`, and Cua's MIT-licensed GNOME Shell
helper for GNOME on Wayland under `gnome-shell/`. The driver itself is installed
from the published `cua-driver` Python distribution.

## Install and use

```console
octet extension install octet-computer-use
octet --enable-extension octet-computer-use
```

Then open `/extensions`, choose **octet-computer-use** (choosing a disabled
extension enables it first), and pick **Set up computer use**. Setup shows each
step live as it runs and does everything computer use needs: it provisions the
driver, installs the bundled model-colored cursor themes, installs the GNOME
Shell helper on GNOME Wayland, checks your permissions (macOS asks you to allow
access), and at the end offers the optional Jev setup. **Check status** re-runs
the driver self-check and permission probe, and the menu's header shows the
result. Esc cancels a running step.

The web UI has no options menu yet; there the same actions run as the
`/computer-use` command (`setup`, `status`, `jev`, and `jev-use …`).

Setup installs `cua-driver` 0.30.2 or newer. It publishes builds for macOS 13
or newer, Linux with glibc 2.31 or newer on x86_64 or aarch64, and 64-bit
Windows on x64 or ARM64. On other systems setup stops and says so, and an
existing older driver is replaced rather than reused.

The driver needs Python 3.10 or newer. When octet runs the bundle with an older
Python (macOS's Xcode Python is 3.9), setup builds the driver's runtime from a
compatible `python3` on `PATH` or a standard macOS install (Homebrew or
python.org). With none available, it stops and names the version to install.

The `computer_use_setup` agent tool provisions only the driver; it cannot
install themes. Theme installation is deliberately a trusted local operation,
not an agent tool. On Linux, whose driver wheel ships no theme compiler, the
bundle writes the same reviewed artifacts straight into the driver's theme store
(`~/.local/share/cua-driver/cursor-themes`), and it also does so the first time
the cursor starts when they are missing, so model colors work however the
driver was provisioned. On GNOME Wayland, setup also installs the bundled
GNOME Shell helper; log out and back in once to load it. The bundle stores the
provisioned driver under
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
- **Linux** — no system permission exists to grant. Run octet from a terminal
  inside your graphical session so the driver can reach it: X11, or Wayland
  such as Hyprland on Omarchy. In a Wayland session the bundle enables the
  driver's native Wayland backend, so native Wayland windows are visible as
  well as XWayland ones. AT-SPI 2 (`at-spi2-core`) supplies element trees;
  without it the driver still captures and acts by pixel. See
  [Linux setup](../../docs/linux.md) for the Omarchy walkthrough.

Until the permission is granted, observation and action calls fail. That is
expected, and `computer_use_status` will say so along with the runtime in use.
On Linux, status reports the reachable display session (X11, native Wayland,
or both) and whether AT-SPI is available instead of macOS grants, and holds
effectful actions only when no display session is reachable.

### Runtime modes

`computer_use_status` reports the selected runtime and its binary.

| Runtime | When it is used | Needs | Agent cursor |
| --- | --- | --- | --- |
| `desktop-host` | Default on macOS when signed `/Applications/CuaDriver.app` and its grants are live | Grants given to that Cua Driver app | Yes, after cursor-state verification |
| `direct` | Non-macOS by default, or macOS only with `OCTET_CUA_DESKTOP_HOST=0` | The grants of the app running octet; on Linux, a reachable display session | On Linux (X11, wlroots Wayland such as Hyprland/Sway, or GNOME via its Shell helper); otherwise no |
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

Then, inside octet, open `/extensions`, choose octet-computer-use, and pick
**Set up computer use** (this downloads the published `cua-driver` wheel from
the configured package index into `%USERPROFILE%\.octet\computer-use`, and
only when you run it), then **Check status**. The driver runs as your user; Windows needs no
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
| `computer_use_windows` | `list_windows` | Read-only. Optional `on_screen_only: true` filters to visible windows; `false` includes off-screen windows. |
| `computer_use_window_state` | `get_window_state` | Read-only. Accessibility tree by default; screenshot with `include_screenshot: true`. |
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
  are verified before any tool call is reported ready. On Linux the direct
  runtime draws the same themed cursor through Cua's X11 or Wayland overlay; it
  is best-effort there, so a cursor that cannot be shown is reported in status
  and never blocks an action. On native Wayland, Cua 0.30 cannot read cursor
  state back, so the setters' acknowledgements confirm it instead. GNOME draws
  the cursor in its Shell helper, which the bundle pins to the model color. The cursor uses a short,
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
  session variables (`DISPLAY`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`,
  `HYPRLAND_INSTANCE_SIGNATURE`, and the documented equivalents). On Linux,
  where the driver launches apps itself, it also receives the host-sanitized
  `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, and locale that octet
  already gives its own tool subprocesses, so launched apps start as they do
  from the desktop. Provider tokens and arbitrary ambient variables are not
  forwarded.
- **No secrets, no silent installs.** The bundle never types credentials for you
  and never downloads a driver outside the standard package install you trigger.
  Where the host Python cannot create a venv with pip (Debian and Ubuntu
  without `python3-venv`), that install downloads the same published Linux
  wheel from the package index, verifies its SHA-256 against the index, and
  extracts only the driver package.
  OS permissions are always yours to grant.
- **Re-snapshot before acting.** Element indices are replaced by the next window
  snapshot, so read state before each indexed action. The bundled skill documents
  the full observe-act-verify loop.

## Upstream Cua Driver `jev-use`

Octet can provision and run the **pinned upstream recipe**, rather than asking
Octet's main model to supervise every click. The Python or TypeScript runner owns
its persistent Driver MCP connection and isolated Chromium profile; it observes,
constructs candidates, asks Jev, executes the selected action, and verifies the
submitted value against the local fixture's independent `/state` endpoint.
The existing `computer_use_jev_choose` remains a separate lightweight chooser.

All of it is under `/extensions` → octet-computer-use → **jev-use recipe
(advanced)**: **Check readiness**, **Install the recipe** (optionally **with
TypeScript support**), **Run the demo with mock Jev** or **with live Jev**, and
**More run options** for the TypeScript runner and the visual path.

Setup and runs are background jobs because the host RPC deadline (≈30s)
is shorter than a recipe run. Tools and menu actions return immediately with a
`job_id`; poll for the terminal result instead of re-launching. Your jobs are
listed in the same menu, where each can be checked and a running one
cancelled.

`computer_use_jev_use_status` without arguments reports pinned-source
readiness synchronously and never installs anything. With `{"job_id": ...}`
it reports `running`, `cancelling`, or `finished` for a job owned by the
same host resource owner; finished jobs nest the tool result under
`result`. `computer_use_jev_use_cancel` signals the owned job; it is not
rollback. Only one setup/run/choose job runs at a time; a second launch is
refused until the first is inspected or cancelled. Jobs are fenced by the
host `resource_owner` (session, instance, generation); `session/settled`
cancels that session's jobs. Extension shutdown signals owned subprocesses
immediately, then joins workers for at most 0.5s (host deadline ≈2s). It
reports `cleanup_complete: false`: POSIX MCP children run in a separate
session and already-reparented descendants cannot be reclaimed by PID scan;
Windows suspended-launch/Job-Object paths are unexercised. Cancellation,
timeout, or unknown outcome never rolls back browser actions already
dispatched; inspect retained evidence before retrying.

Preparation, mock proof, live proof, and visual proof are distinct:

- **Preparation** (`setup`, optional `--typescript`) explicitly downloads the
  commit-pinned, integrity-checked Cua source snapshot and installs its
  locked dependencies into Octet-owned state. It does not install Driver, a
  browser, or perception; it does not change OS grants or operate the
  computer. Install `uv` first. For TypeScript, install Node.js 22+ and npm.
  Runs never silently install; they require explicit setup first.

  The menu's **Install with TypeScript support** and **More run options** →
  **Run with the TypeScript runner and live Jev** are the TypeScript variants.

- **Mock proof** (default `run`) uses the deterministic mock provider but
  still operates a real isolated browser against the local form fixture; it
  is not a no-effect dry run. Mock choices prove wiring, not model quality.
- **Live proof** (`--live`) adds live Jev checks and may incur TypeSafe
  charges. It uses the key already configured under **Jev (optional)**, never
  a key in command arguments. Compact observations go to TypeSafe;
  screenshot pixels do not. The upstream runner strips the provider key from
  Driver's child environment. The upstream SDK may retry requests, so step
  and process bounds are not billing caps.
- **Visual proof** is capability-gated by upstream. Normal runs prefer
  semantic browser references. With the separately installed Cua perception
  extension, exercise capture-bound visual clicks with **More run options** →
  **Run the visual path**.

For finer control, the `computer_use_jev_use_run` tool takes these options.
`visual_observation` (`auto`, `always` or `off`) controls observation and
`max_steps` (1–32) bounds each runner's decisions. `port` (0–65535) selects the
loopback fixture port (default `0` picks an unused port).
`expect_visual_status` (`ok`, `not_installed`, `error` or `unavailable`)
requires every attempted visual parse to log that status; with
`visual_fixture` and a non-`ok` status the run must log a fallback that never
submits (`observed: {submitted: null}`, outcome `refuted`/`unknown`/`abstained`/
`budget_exhausted`) instead of claiming task success. `skipped` steps do
not count as attempts, but at least one attempt is required.
`require_visual_path` needs `visual_fixture` and an `ok` visual status,
and fails unless every runner submitted through `click` with the exact
`capture_id`.

Source status reports the pinned revision and readiness. Each invocation
retains a new private proof directory; failed or cancelled runs never
overwrite an earlier proof. Check the returned `complete` flag and
per-check evidence, not just a process exit code.

| Tool | Purpose |
| --- | --- |
| `computer_use_jev_use_status` | Read setup/source readiness without running the recipe. |
| `computer_use_jev_use_setup` | Explicit source/dependency setup; optional TypeScript. |
| `computer_use_jev_use_run` | Managed mock/live form workflow with retained verification evidence. |
| `computer_use_jev_use_choose` | Upstream JSON chooser interface for another harness, Python or TypeScript, mock or live. |

The standalone chooser accepts `cua.jev_choice_request_v1` and returns
`cua.jev_choice_v1`. Its caller still owns observation, candidate construction,
execution, and verification. It never executes a selected ID. Use `mock: true`
for credential-free checks. Requests may include typed visual regions but not
arbitrary tool calls, screenshot bytes, or environment data.

**Scope and policy:** this runs upstream's fixed local form task, not a universal
native-app agent or arbitrary-site scraper. New workflows still need their own
candidate builder and independent verification. The separate, unmerged
`suggest_action` proposal is not required or enabled. The runner uses a separate
upstream-owned session, not Octet's manual-action cursor session. Under a per-action
confirmation policy, Octet refuses the autonomous runner because upstream cannot
prompt for each action; a whole-workflow approval cannot bypass that policy.
Use the existing individual tools in that mode. Source tests and mock process
fixtures are not evidence of live desktop, provider, or cross-platform success.

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
