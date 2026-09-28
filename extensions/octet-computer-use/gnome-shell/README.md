# GNOME Shell helper (WinRects)

`winrects/` is Cua's WinRects GNOME Shell extension from
[trycua/cua](https://github.com/trycua/cua/tree/cua-driver-rs-v0.30.2/libs/cua-driver/wayland-helper/winrects@cua),
tag `cua-driver-rs-v0.30.2` (commit `a2229c5b829153ec3b1828387bc72ca8f1f18704`),
helper API version 8. Cua's MIT license is preserved in `CUA-LICENSE.md`.

On GNOME's Mutter Wayland compositor, an ordinary client cannot read window
geometry, verify window activation, or draw an overlay. Cua Driver gets all three
from this extension, which runs inside GNOME Shell. The `cua-driver` wheel does
not include it, so this bundle carries it. On GNOME Wayland, `/computer-use setup`
installs it to `~/.local/share/gnome-shell/extensions/winrects@cua` and enables
it. GNOME loads it at the next login.

## Octet change

Only `extension.js` differs from upstream. It adds one D-Bus method:

- `SetThemeColor(fill_color)` pins the helper-drawn cursor to a `#RRGGBB` color
  until an empty string releases it. While pinned, the driver's per-session
  `SetCursorColor` calls are ignored.

GNOME cannot load Cua's compiled cursor themes. The helper draws its own
vector cursor, which Cua colors per session. The pin lets the extension apply
the same model color that the compiled themes use on X11 and wlroots Wayland.
The method name, interface, and API version are otherwise unchanged, so Cua
Driver detects and verifies the helper exactly as it does the upstream copy.

To update, copy `wayland-helper/winrects@cua/` from the tag that matches the
provisioned driver, then reapply the `SetThemeColor` addition.
