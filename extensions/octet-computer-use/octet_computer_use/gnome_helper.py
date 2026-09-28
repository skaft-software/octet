"""Cua's WinRects GNOME Shell helper, bundled for GNOME on Wayland.

Mutter gives an ordinary Wayland client no window geometry, no verified window
activation, and no way to draw an overlay. Cua Driver gets all three from its
WinRects Shell extension, which runs inside the compositor. The ``cua-driver``
wheel does not ship it, so this bundle carries the MIT-licensed helper from the
matching driver release (see ``gnome-shell/README.md``) with one addition:
``SetThemeColor``, which pins the helper-drawn cursor to Octet's model color.

Installation is a trusted local setup step, never an agent tool. It writes only
the bundled files into the user's own GNOME Shell extension directory and adds
the helper to the enabled set. GNOME loads a new extension at the next login.
Every call here is best-effort and bounded; a failure never blocks computer use.
"""

from __future__ import annotations

import os
import platform
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Optional

UUID = "winrects@cua"
BUNDLE = Path(__file__).resolve().parent.parent / "gnome-shell" / "winrects"
FILES = ("metadata.json", "extension.js")
_DEST = "org.cua.WinRects"
_PATH = "/org/cua/WinRects"
_TIMEOUT_SECONDS = 5


def _desktop_names() -> List[str]:
    names: List[str] = []
    for variable in ("XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP", "DESKTOP_SESSION"):
        names += [part.strip().lower() for part in os.environ.get(variable, "").split(":")]
    return [name for name in names if name]


def is_gnome_wayland() -> bool:
    """Whether this is a GNOME Shell (Mutter) Wayland session.

    Ubuntu, Pop!_OS, and other GNOME derivatives report ``ubuntu:GNOME`` and
    similar, so any listed desktop naming GNOME counts.
    """

    if platform.system().lower() != "linux" or not os.environ.get("WAYLAND_DISPLAY"):
        return False
    return any("gnome" in name for name in _desktop_names())


def extension_directory() -> Optional[Path]:
    data = os.environ.get("XDG_DATA_HOME")
    if data:
        return Path(data) / "gnome-shell" / "extensions" / UUID
    home = os.environ.get("HOME")
    if not home:
        return None
    return Path(home) / ".local" / "share" / "gnome-shell" / "extensions" / UUID


def _run(argv: List[str]) -> Optional[subprocess.CompletedProcess]:
    try:
        return subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              timeout=_TIMEOUT_SECONDS, check=False)
    except (OSError, subprocess.SubprocessError):
        return None


def _gdbus(method: str, *arguments: str) -> Optional[str]:
    completed = _run(["gdbus", "call", "--session", "--dest", _DEST, "--object-path", _PATH,
                      "--method", f"{_DEST}.{method}", *arguments])
    if completed is None or completed.returncode != 0:
        return None
    return completed.stdout.strip()


def active_version() -> Optional[int]:
    """The running helper's API version, or None when it is not loaded."""

    reply = _gdbus("GetVersion")
    if not reply:
        return None
    digits = "".join(character for character in reply if character.isdigit())
    return int(digits) if digits else None


def set_theme_color(color: str) -> bool:
    """Pin the helper-drawn cursor to ``color`` (``#RRGGBB``); best-effort."""

    if len(color) != 7 or color[0] != "#":
        return False
    try:
        int(color[1:], 16)
    except ValueError:
        return False
    return _gdbus("SetThemeColor", color) is not None


def _write_if_changed(target: Path, data: bytes) -> bool:
    if target.is_symlink():
        raise RuntimeError(f"{target.name} in the GNOME helper directory must not be a symlink")
    try:
        if target.is_file() and target.read_bytes() == data:
            return False
    except OSError:
        pass
    staged = target.with_name(".%s.%d.tmp" % (target.name, os.getpid()))
    try:
        staged.write_bytes(data)
        os.replace(staged, target)
    finally:
        if staged.exists():
            staged.unlink()
    return True


def _enabled_extensions() -> Optional[List[str]]:
    completed = _run(["gsettings", "get", "org.gnome.shell", "enabled-extensions"])
    if completed is None or completed.returncode != 0:
        return None
    text = completed.stdout.strip()
    if text.startswith("@as"):
        text = text[3:].strip()
    if not (text.startswith("[") and text.endswith("]")):
        return None
    inner = text[1:-1].strip()
    if not inner:
        return []
    names = []
    for part in inner.split(","):
        part = part.strip()
        if len(part) >= 2 and part[0] == part[-1] and part[0] in "'\"":
            names.append(part[1:-1])
        else:
            return None
    return names


def _enable() -> bool:
    completed = _run(["gnome-extensions", "enable", UUID])
    if completed is not None and completed.returncode == 0:
        return True
    current = _enabled_extensions()
    if current is None:
        return False
    if UUID in current:
        return True
    value = "[" + ", ".join("'%s'" % name for name in current + [UUID]) + "]"
    completed = _run(["gsettings", "set", "org.gnome.shell", "enabled-extensions", value])
    return completed is not None and completed.returncode == 0


def user_extensions_disabled() -> bool:
    completed = _run(["gsettings", "get", "org.gnome.shell", "disable-user-extensions"])
    return completed is not None and completed.stdout.strip() == "true"


def install() -> Dict[str, Any]:
    """Install and enable the bundled helper; report what the user must do next."""

    directory = extension_directory()
    if directory is None:
        return {"gnome_helper": "unavailable", "gnome_helper_detail": "HOME is not set"}
    try:
        if directory.is_symlink():
            raise RuntimeError("the GNOME helper directory must not be a symlink")
        directory.mkdir(parents=True, exist_ok=True)
        changed = False
        for name in FILES:
            changed = _write_if_changed(directory / name, (BUNDLE / name).read_bytes()) or changed
    except (OSError, RuntimeError) as error:
        return {"gnome_helper": "failed", "gnome_helper_detail": str(error)[:300]}
    enabled = _enable()
    version = active_version()
    pinned = version is not None and not changed and _gdbus("SetThemeColor", "") is not None
    if pinned:
        state, detail = "active", "the GNOME Shell helper is active"
    elif not enabled:
        state = "installed"
        detail = ("the GNOME Shell helper is installed but could not be enabled; run "
                  f"`gnome-extensions enable {UUID}`, then log out and back in")
    else:
        state = "restart-required"
        detail = "log out and back in once so GNOME Shell loads the computer-use helper"
    if user_extensions_disabled():
        detail += "; GNOME user extensions are disabled (org.gnome.shell disable-user-extensions)"
    return {"gnome_helper": state, "gnome_helper_detail": detail}


def status() -> Dict[str, Any]:
    """Whether the Octet-patched helper is loaded, without changing anything."""

    directory = extension_directory()
    installed = bool(directory and (directory / "extension.js").is_file())
    version = active_version()
    if version is None:
        state = "restart-required" if installed else "missing"
    elif _gdbus("SetThemeColor", "") is None:
        # An upstream helper without the theme pin is loaded; the bundled one
        # takes over at the next login.
        state = "restart-required" if installed else "upstream"
    else:
        state = "active"
    return {"gnome_helper": state}
