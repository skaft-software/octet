# Linux setup

[Documentation](README.md) · [Installation](installation.md) · [Terminal](terminal.md)

octet runs natively on GNU/Linux x86-64. This page covers the desktop pieces that
differ from macOS: the clipboard, opening links, and native computer use. It uses
[Omarchy](https://omarchy.org) (Arch Linux with the Hyprland Wayland compositor) as
the worked example, but every step applies to any X11 or Wayland desktop.

## Install

The [native installer](installation.md) needs `curl` and `python3`, and the
prebuilt release requires GNU libc (musl is not supported). octet itself also
needs `ripgrep`. On Arch and Omarchy:

```sh
sudo pacman -S --needed curl python ripgrep
```

Then run the installer from the [README](../README.md#install). Building from
source works the same way as on macOS.

## Clipboard

Copying from the transcript writes through a native helper and through OSC 52,
and pasting reads through a native helper. The helper follows the session octet
runs in:

| Session | Helper | Arch package |
| --- | --- | --- |
| Wayland (`WAYLAND_DISPLAY`), e.g. Hyprland | `wl-copy` / `wl-paste` | `wl-clipboard` |
| X11 or XWayland (`DISPLAY`) | `xclip`, then `xsel` | `xclip` or `xsel` |

Omarchy ships `wl-clipboard`. When no helper is available, copying still uses
OSC 52, which Alacritty, Ghostty, and kitty accept. Inside tmux, OSC 52 also
needs `set -g set-clipboard on`; see [tmux setup](tmux.md).

## Opening links

Sign-in flows and `octet serve` open URLs with `xdg-open` from `xdg-utils`,
which Omarchy ships. Install it on desktops that lack it.

## Computer use

The [computer-use extension](../extensions/octet-computer-use/README.md) drives
Linux desktops through Cua Driver's Linux build. Install and set it up the same
way as on other platforms:

```console
octet extension install octet-computer-use
octet --enable-extension octet-computer-use
```

Then open `/extensions`, choose octet-computer-use, and pick **Set up computer
use**. Linux has no system permission to grant. Computer use is ready when the
driver can reach your display session, and **Check status** says which one it
found.
Setup also works without `python3-venv` or pip: on Debian and Ubuntu the extension
installs the published driver wheel directly after verifying its checksum.

| Desktop | Windows and capture | Input | Agent cursor |
| --- | --- | --- | --- |
| Hyprland (Omarchy), Sway, labwc, other wlroots | Native Wayland, compositor IPC | wlroots virtual pointer and keyboard | Model-colored, layer-shell overlay |
| GNOME on Wayland | Cua's GNOME Shell helper, installed by setup | Portal (libei), after the helper verifies focus | Model-colored, drawn by the helper |
| KDE Plasma on Wayland | AT-SPI and portal | Accessibility actions and portal input | Model-colored, layer-shell overlay |
| Any X11 session, including GNOME/KDE on Xorg | X11 | X11 | Model-colored, X11 overlay |

- **Start octet inside your graphical session.** Launch it from a terminal on the
  desktop, not over SSH or from a TTY, so `WAYLAND_DISPLAY` or `DISPLAY`,
  `XDG_RUNTIME_DIR`, and `DBUS_SESSION_BUS_ADDRESS` reach the driver.
- **Wayland is native.** In a Wayland session octet turns on the driver's native
  Wayland backend, so native Wayland windows are visible, not only XWayland ones.
  The driver identifies the compositor from `XDG_CURRENT_DESKTOP` and uses
  `HYPRLAND_INSTANCE_SIGNATURE` or `SWAYSOCK` to list windows.
- **Wayland input targets the focused window.** Wayland has no way to send input
  to a window in the background, so the agent retries such an action with
  foreground delivery, which briefly focuses the target window.
- **GNOME needs one login.** On GNOME Wayland, setup installs and
  enables Cua's GNOME Shell helper. GNOME loads it at the next login, so log out
  and back in once. Until then, status says so.
- **KDE limits.** Cua's KWin helper must be built against your KWin, so it is not
  bundled. KDE Wayland works through accessibility actions and the portal, and
  the driver refuses raw pixel input it cannot aim at the right window.
- **AT-SPI gives element trees.** Install `at-spi2-core`
  (`sudo pacman -S at-spi2-core`) and log in again. The driver asks the session to
  turn accessibility on, so Chromium, Electron, GTK, and Qt apps expose their
  trees. Without AT-SPI the driver still captures the screen and acts by pixel,
  and status says so.
- **Portal prompts are expected.** Where the compositor offers no direct capture
  or input protocol, the driver goes through `xdg-desktop-portal`, which can ask
  for consent once per session.
- **The cursor follows your model.** The agent cursor uses the same color as the
  model-adaptive prompt in the TUI and switches at the next action after
  `/model`. The themes install themselves the first time the cursor starts.

Launched apps inherit your `PATH`, `HOME`, and locale, so Omarchy's web-app
launchers and other user-installed commands start as they do from your desktop.
