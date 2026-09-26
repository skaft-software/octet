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
only when an installed `CuaDriver.app` reports live permissions, because on some
macOS releases that app's grant never persists — it re-prompts on every launch,
and adopting it would make every action fail. When that happens octet falls back
to the direct runtime automatically, which is the supported default.

Set `OCTET_CUA_DESKTOP_HOST=0` to force the direct runtime, or `=1` to require
the desktop host.

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
