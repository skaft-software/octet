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
/computer-use setup
```

Linux has no system permission to grant. Computer use is ready when the driver can
reach your display session, and `/computer-use status` says which one it found.

- **Start octet inside your graphical session.** Launch it from a terminal on the
  desktop, not over SSH or from a TTY, so `WAYLAND_DISPLAY` or `DISPLAY`,
  `XDG_RUNTIME_DIR`, and `DBUS_SESSION_BUS_ADDRESS` reach the driver.
- **Wayland is native.** In a Wayland session octet turns on the driver's native
  Wayland backend, so native Wayland windows are visible, not only XWayland ones.
  On Hyprland the driver also uses `HYPRLAND_INSTANCE_SIGNATURE` to list windows.
- **AT-SPI gives element trees.** Install `at-spi2-core`
  (`sudo pacman -S at-spi2-core`) and log in again. Without it the driver still
  captures the screen and acts by pixel, and status says so. Chromium and
  Electron apps expose their tree only with `--force-renderer-accessibility`;
  Qt apps need `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`.
- **Portal prompts are expected.** Where the compositor offers no direct capture
  or input protocol, the driver goes through `xdg-desktop-portal`, which can ask
  for consent once per session.
- **No agent cursor.** The Linux driver runs direct, without the macOS
  agent-cursor overlay, so setup skips the cursor themes.

Launched apps inherit your `PATH`, `HOME`, and locale, so Omarchy's web-app
launchers and other user-installed commands start as they do from your desktop.
