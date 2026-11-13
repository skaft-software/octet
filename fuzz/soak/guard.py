#!/usr/bin/env python3
"""Isolation guard: the soak harness must never touch the real HOME or ~/.pi.

`check_isolated` raises `IsolationError` unless HOME, every Pi agent directory
(OCTET_PI_AGENT_DIR, PI_CODING_AGENT_DIR, HOME/.pi/agent) and the octet data
directory (OCTET_DATA_DIR, HOME/.octet) resolve inside the harness's own
temporary run directory. `preflight` is the cheap check soak.sh runs before
anything is launched. Nothing here writes to disk.
"""
import os
import pwd
import sys
from pathlib import Path

SENTINEL = ".soak-harness"


class IsolationError(Exception):
    pass


def _resolve(value) -> Path:
    return Path(os.path.realpath(os.path.expanduser(str(value))))


def _inside(path: Path, root: Path) -> bool:
    return path == root or root in path.parents


def real_home() -> Path:
    return _resolve(pwd.getpwuid(os.getuid()).pw_dir)


def check_isolated(env, run_dir) -> None:
    """Refuse unless every user-state location in `env` is inside `run_dir`."""
    root = _resolve(run_dir)
    if _inside(root, real_home()):
        raise IsolationError(
            f"refusing to start: the run directory {root} is inside the real home "
            f"{real_home()}; set TMPDIR or SOAK_RUN_ROOT to a path outside it")
    home = env.get("HOME")
    if not home:
        raise IsolationError("refusing to start: HOME is not set in the harness environment")
    home_path = _resolve(home)
    candidates = {
        "HOME": home_path,
        "Pi agent dir (HOME/.pi/agent)": home_path / ".pi" / "agent",
        "octet dir (HOME/.octet)": home_path / ".octet",
    }
    for name in ("OCTET_PI_AGENT_DIR", "PI_CODING_AGENT_DIR", "OCTET_DATA_DIR"):
        if env.get(name):
            candidates[name] = _resolve(env[name])
    for label, path in candidates.items():
        if not _inside(path, root):
            raise IsolationError(
                f"refusing to start: {label} resolves to {path}, outside the soak "
                f"run directory {root}. The soak harness never touches the real "
                f"HOME, ~/.pi or ~/.octet; run it through fuzz/soak/soak.sh.")


def preflight(tmp_root) -> None:
    root = _resolve(tmp_root)
    if _inside(root, real_home()):
        raise IsolationError(
            f"refusing to start: TMPDIR/SOAK_RUN_ROOT {root} is inside the real home "
            f"{real_home()}; point it outside the home directory (for example /tmp)")


if __name__ == "__main__":
    try:
        preflight(sys.argv[1] if len(sys.argv) > 1 else os.environ.get("SOAK_RUN_ROOT") or os.environ.get("TMPDIR") or "/tmp")
    except IsolationError as error:
        print(error, file=sys.stderr)
        sys.exit(2)
