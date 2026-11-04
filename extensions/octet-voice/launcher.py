#!/usr/bin/env python3
"""Inert stdlib bootstrap; use only the explicitly provisioned voice interpreter.

The host already launches .py entrypoints natively on Linux, macOS and Windows.
No dependency resolution, downloads, device access or fallback installs occur here.
"""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys


def user_home() -> Path:
    if sys.platform != "win32":
        return Path.home()
    # The host intentionally strips USERPROFILE/HOMEDRIVE/HOMEPATH. Query the
    # native profile directory instead of assuming Path.home() can expand '~'.
    import ctypes
    from ctypes import wintypes

    buffer = ctypes.create_unicode_buffer(32768)
    get_folder = ctypes.windll.shell32.SHGetFolderPathW
    get_folder.argtypes = [wintypes.HWND, ctypes.c_int, wintypes.HANDLE,
                          wintypes.DWORD, wintypes.LPWSTR]
    get_folder.restype = ctypes.c_long
    if get_folder(None, 0x0028, None, 0, buffer) != 0:  # CSIDL_PROFILE
        raise OSError("cannot locate the Windows user profile")
    return Path(buffer.value)


def private_python(home: Path, platform: str) -> Path:
    venv = home / ".local" / "share" / "octet-voice" / "venv"
    return venv / ("Scripts/python.exe" if platform == "win32" else "bin/python")


def main() -> int:
    try:
        home = user_home()
        python = private_python(home, sys.platform)
        if not python.is_file():
            raise OSError(f"private voice Python is missing: {python}; see README setup")
        root = Path(os.environ.get("OCTET_EXTENSION_DIR", Path(__file__).resolve().parent))
        command = [str(python), "-B", str(root / "extension.py")]
        environment = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
        if sys.platform == "win32":
            # Restore only this native-derived value in our child environment;
            # settings Path.home() must also work under the sanitized host.
            environment["USERPROFILE"] = str(home)
            # Keep the supervised parent alive; Windows exec spawns a replacement
            # PID. The child inherits stdio and the host's process-tree job.
            return subprocess.call(command, env=environment)
        os.execve(str(python), command, environment)
    except OSError as error:
        print(f"octet-voice: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
