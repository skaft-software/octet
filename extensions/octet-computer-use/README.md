# octet computer-use

**Distribution: 0.8.0.** This bundle requires exactly octet 0.8.0.
Use the [version-matched installation](../../docs/installation.md) and the
[0.8.0 release record](../../docs/releases/v0.8.0.md) for signed assets and
public-install evidence. **This local RC checkout's `extension.toml` instead
requires `=0.8.1-rc.1`; use `--extension-dir ./extensions` with the candidate.**

Operate native desktop applications on **macOS, Windows, and Linux** through a
locally installed [Cua Driver](https://github.com/trycua/cua), the MIT-licensed
open-source computer-use driver. This bundle provisions the driver on request,
connects to it over local stdio MCP, and republishes a reviewed subset of its
tools as octet tools.

The driver is a separate open-source project. This bundle does not vendor it,
does not fork it, and does not include any part of OpenAI's CUA runtime. It
installs the published `cua-driver` Python distribution, the same way
`octet-browse` provisions its pinned Playwright runtime.

## Install and use

```console
octet extension install octet-computer-use
octet --enable-extension octet-computer-use
```

Then provision the driver and check its health:

```console
/computer-use
```

or ask the agent to run `computer_use_setup` once. The bundle stores the
provisioned driver under `~/.octet/computer-use` and never touches your Python
environment or any global install.

`computer_use_status` reports whether the driver is present, its version, its
self-check, and your operating system's permission state. It never prompts.

## Grant operating-system permissions

The driver needs permission to observe and control the desktop. **This bundle
never grants an operating-system permission for you.** Grant it yourself:

- **macOS** — Accessibility **and** Screen & System Audio Recording, granted to
  **the app you run octet from**: your terminal or your editor. The default
  runtime runs inside octet's own process and therefore uses that app's
  permissions, so there is no separate helper to install or grant. Restart octet
  afterwards so it re-reads the current grants.
- **Windows** — the driver runs as your user; some stacks need the process to
  be interactive (an unlocked, visible session).
- **Linux** — a live display session plus AT-SPI 2 accessibility. X11/XWayland
  routes more widely than native Wayland.

Until the permission is granted, observation and action calls fail. That is
expected, and `computer_use_status` will say so along with the runtime in use.

### Runtime modes

`computer_use_status` reports which runtime is live.

| Runtime | When it is used | Needs | Agent cursor |
| --- | --- | --- | --- |
| `direct` | Default, and whenever no usable host is present | Nothing beyond the grants above | No |
| `desktop-host` | A Cua Driver app is installed **and** its own grants are live | Grants given to that app | Yes |

The desktop host is the optional path to the visible agent cursor. It is used
only when an installed host reports live permissions, because on some macOS
releases `CuaDriver.app`'s grant never persists — it re-prompts on every launch,
and adopting it would make every action fail. When that happens octet falls back
to the direct runtime automatically, which is the supported default.

Set `OCTET_CUA_DESKTOP_HOST=0` to force the direct runtime, or `=1` to require
the desktop host.

### Optional: build the octet host app (macOS, agent cursor)

The cursor overlay needs an AppKit main thread with Window Server access, which
only a real application can provide. This bundle ships a small host app that
supplies exactly that and nothing else: it starts `cua-driver serve` under its
own permission identity and never shows a window or takes focus.

```console
bash extensions/octet-computer-use/build-host-app.sh --install
```

It installs `/Applications/OctetComputerUse.app` with the bundle identifier
`com.octet.computeruse`, builds from source, and signs with the first
codesigning identity it finds. Pass `--ad-hoc` for a throwaway build (its
permission grant resets on every rebuild) or `--identity "..."` to choose one.

The host installs to its own identity rather than Cua's `com.trycua.driver`, so
its permission grants cannot be confused with Cua's own app. When the host is
present but not running, octet starts it in the background once; if it still
cannot prove its grant, octet uses the direct runtime instead.

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

After this, `permissions status` reports `source.attribution: "driver-daemon"`
and the grants survive respawns and reboots. This bundle starts the host through
LaunchServices for exactly this reason, and prefers Cua's signed, notarized app
whenever it is installed.

Octet's own host (`build-host-app.sh`) is a fallback for when the official app is
absent or its grant will not take. It is not the default: an unnotarized app that
asks for screen recording and Accessibility is a poor thing to hand a user, so
Cua's signed app is adopted first. The app is optional; without any usable host
octet uses the direct runtime.

## What the agent can do

| Tool | Driver tool | Notes |
| --- | --- | --- |
| `computer_use_status` | local | Provisioning, version, permissions, self-check. |
| `computer_use_setup` | local | Install the driver from the package index. |
| `computer_use_installed_apps` | `list_apps` | Read-only. |
| `computer_use_windows` | `list_windows` | Read-only. |
| `computer_use_window_state` | `get_window_state` | Read-only. Accessibility tree + screenshot. |
| `computer_use_desktop_state` | `get_desktop_state` | Read-only. Full-screen capture. |
| `computer_use_click` | `click` | **Confirmation required.** |
| `computer_use_type_text` | `type_text` | **Confirmation required.** |
| `computer_use_press_key` | `press_key` | **Confirmation required.** |
| `computer_use_scroll` | `scroll` | **Confirmation required.** |
| `computer_use_launch_app` | `launch_app` | **Confirmation required.** |
| `computer_use_start_session` | `start_session` | **Confirmation required.** |
| `computer_use_end_session` | `end_session` | **Confirmation required.** |

The driver publishes a much larger catalog (58 tools on macOS at the time of
writing). This bundle republishes a small, reviewed subset so octet's tool
catalog stays stable across upstream releases. Only reviewed arguments are
forwarded; an unrecognised argument is dropped rather than passed through.

## Safety model

- **Confirmation on effect.** Every driver action that is not annotated
  `readOnlyHint: true` requires an explicit user confirmation before it is
  dispatched. A declined, cancelled, or unavailable confirmation denies the
  call — the agent never proceeds on assumption. A tool the driver has not
  described is treated as effectful.
- **Read-only means read-only.** Only `list_apps`, `list_windows`,
  `get_window_state`, and `get_desktop_state` run without a prompt.
- **Least environment.** The driver child receives only reviewed, non-secret
  desktop session variables (`DISPLAY`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`,
  and the documented equivalents). Provider tokens, `PATH` overrides, and
  arbitrary ambient variables are not forwarded.
- **No secrets, no silent installs.** The bundle never types credentials for
  you and never downloads a driver outside the standard package install you
  trigger. OS permissions are always yours to grant.
- **Re-snapshot before acting.** Element indices are replaced by the next
  window snapshot, so read state before each indexed action. The bundled skill
  documents the full observe-act-verify loop.

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

To include the live driver tests, point at an installed binary:

```console
PYTHONPATH=.:vendor:tests OCTET_CUA_DRIVER_BINARY=/path/to/cua-driver \
  python3 -m unittest discover -s tests -p 'test_*.py'
```
