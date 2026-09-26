"""Resolve, provision, and health-check the locally installed Cua Driver.

This module owns exactly one external dependency: the MIT-licensed
``cua-driver`` distribution from ``trycua/cua``. It never downloads a driver
binary, never runs a piped remote script, and never grants an operating-system
permission. It installs the published Python wheel into an octet-owned
virtual environment using the host's own interpreter, mirroring how
``octet-browse`` provisions a pinned Playwright runtime.

The driver is provisioned from the package index as
``cua-driver`` (unpinned by default, so the newest release is used; an exact
version can be requested for reproducible installs). The wheel is
platform-specific and bundles the ``cua-driver`` executable.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
import os
import platform
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

# The published distribution name on PyPI. It is MIT licensed and ships
# platform-specific wheels containing the driver executable.
DISTRIBUTION = "cua-driver"

# We track the newest release by default. Callers may request an exact version
# for a reproducible install; the value is passed straight to pip and is never
# assembled from untrusted input by this module.
DEFAULT_VERSION = ""

# Ceilings so a hostile or broken index response cannot make provisioning run
# unbounded. They are generous relative to the ~70 MiB wheel.
INSTALL_TIMEOUT_SECONDS = 900
PROBE_TIMEOUT_SECONDS = 60


class ProvisionError(RuntimeError):
    """A provisioning or health step failed in a way the caller must surface."""


@dataclass(frozen=True)
class DriverPaths:
    """Resolved, octet-owned locations for the provisioned driver.

    Constructing this is inert. No directory is created and no process is run
    until :func:`ensure` or :func:`health` is called.
    """

    root: Path

    @classmethod
    def for_home(cls, home: Optional[Path] = None) -> "DriverPaths":
        base = Path(home) if home is not None else Path.home()
        return cls(base.expanduser().absolute() / ".octet" / "computer-use")

    @property
    def venv(self) -> Path:
        return self.root / "runtime"

    @property
    def venv_python(self) -> Path:
        if platform.system() == "Windows":
            return self.venv / "Scripts" / "python.exe"
        return self.venv / "bin" / "python"

    @property
    def install_lock(self) -> Path:
        return self.root / "install.lock"

    @property
    def install_log(self) -> Path:
        return self.root / "install.log"


def _install_environment() -> Dict[str, str]:
    """A sanitized environment for pip, mirroring octet-browse's install path."""

    environment = os.environ.copy()
    # A caller-controlled Python path could make the venv import an ambient
    # package despite the exact distribution pin, so drop the selection
    # variables. Ordinary proxy/TLS variables are kept for the download.
    for name in ("PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV"):
        environment.pop(name, None)
    environment["PYTHONNOUSERSITE"] = "1"
    environment["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
    return environment


def _run(
    argv: Sequence[str],
    *,
    cwd: Optional[Path] = None,
    env: Optional[Mapping[str, str]] = None,
    timeout: int = PROBE_TIMEOUT_SECONDS,
) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            list(argv),
            cwd=str(cwd) if cwd is not None else None,
            env=dict(env) if env is not None else None,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise ProvisionError(
            f"command timed out after {timeout}s: {argv[0]} {argv[1] if len(argv) > 1 else ''}"
        ) from error
    except OSError as error:
        raise ProvisionError(f"failed to run {argv[0]}: {error}") from error


def _ensure_directories(paths: DriverPaths) -> None:
    paths.root.mkdir(parents=True, exist_ok=True)
    # The runtime and logs live under a user-owned root. Refuse to continue if
    # the root is a symlink so we never write through an attacker-planted link.
    if paths.root.is_symlink():
        raise ProvisionError("computer-use root must not be a symlink")


def driver_version(binary: Path) -> Optional[str]:
    """Return the driver's reported version, or None when it cannot run."""

    try:
        completed = _run([str(binary), "--version"], timeout=PROBE_TIMEOUT_SECONDS)
    except ProvisionError:
        return None
    if completed.returncode != 0:
        return None
    # Typical output: "cua-driver 0.29.1 — cross-platform ..."
    first = (completed.stdout or completed.stderr or "").strip().splitlines()
    if not first:
        return None
    parts = first[0].split()
    if len(parts) >= 2 and parts[0] == "cua-driver":
        return parts[1]
    return None


def installed_binary(paths: DriverPaths) -> Optional[Path]:
    """Locate the driver executable inside the provisioned environment, if any."""

    python = paths.venv_python
    if not python.is_file():
        return None
    probe = _run(
        [str(python), "-c", "import cua_driver, sys; sys.stdout.write(str(cua_driver.get_binary_path()))"],
        timeout=PROBE_TIMEOUT_SECONDS,
    )
    if probe.returncode != 0:
        return None
    candidate = Path((probe.stdout or "").strip())
    return candidate if candidate.is_file() else None


def _venv_python_for(venv: Path) -> Path:
    if platform.system() == "Windows":
        return venv / "Scripts" / "python.exe"
    return venv / "bin" / "python"


def _pip_spec(version: str) -> str:
    if version:
        # Version is caller-supplied; validate it against a strict pattern so it
        # can never smuggle pip options or a second requirement.
        stripped = version.strip()
        for character in stripped:
            if not (character.isdigit() or character in ".-+abcrpost"):
                raise ProvisionError(f"invalid driver version: {version!r}")
        if not stripped:
            raise ProvisionError("empty driver version")
        return f"{DISTRIBUTION}=={stripped}"
    return DISTRIBUTION


def provision(
    paths: DriverPaths,
    *,
    version: str = DEFAULT_VERSION,
    timeout: int = INSTALL_TIMEOUT_SECONDS,
) -> Path:
    """Provision the driver into the octet-owned venv and return its binary.

    This is idempotent: if a driver is already present it is reused. The install
    performs a network download from the configured package index; the caller is
    responsible for having obtained user consent for that network access.
    """

    _ensure_directories(paths)
    existing = installed_binary(paths)
    if existing is not None:
        return existing

    spec = _pip_spec(version)
    paths.venv.mkdir(parents=True, exist_ok=True)
    environment = _install_environment()

    create = _run(
        [sys.executable, "-m", "venv", "--clear", str(paths.venv)],
        env=environment,
        timeout=timeout,
    )
    if create.returncode != 0:
        raise ProvisionError(
            f"failed to create runtime venv: {(create.stderr or create.stdout or '').strip()[:400]}"
        )

    python = _venv_python_for(paths.venv)
    install = _run(
        [
            str(python),
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--no-input",
            "--no-cache-dir",
            spec,
        ],
        env=environment,
        timeout=timeout,
    )
    if install.returncode != 0:
        raise ProvisionError(
            f"failed to install {spec}: {(install.stderr or install.stdout or '').strip()[:400]}"
        )

    binary = installed_binary(paths)
    if binary is None:
        raise ProvisionError(f"{spec} installed but no driver executable was found")
    return binary


@dataclass(frozen=True)
class Health:
    """A bounded, content-free health summary safe to show in a UI."""

    installed: bool
    version: Optional[str]
    permissions: str
    doctor_ok: bool
    detail: str

    def as_dict(self) -> Dict[str, Any]:
        return {
            "installed": self.installed,
            "version": self.version,
            "permissions": self.permissions,
            "doctor_ok": self.doctor_ok,
            "detail": self.detail,
        }


# A desktop host, when installed, owns the OS permission identity and the GUI
# main thread. On macOS that is the only thing that can draw the agent cursor:
# the overlay needs a certified AppKit main thread and Window Server access that
# a terminal-hosted process does not have. ChatGPT.app takes the same shape - it
# embeds the driver inside a signed app and inherits that app's TCC grants.
DESKTOP_APP_CANDIDATES: Dict[str, str] = {
    "darwin": "/Applications/CuaDriver.app",
    "win32": os.path.expandvars(r"%LOCALAPPDATA%\\CuaDriver\\CuaDriver.exe"),
}


def desktop_app() -> Optional[Path]:
    """The installed CuaDriver desktop host, when one is present.

    Absence is not an error: the direct runtime still drives the desktop, it
    just has no cursor overlay and keeps permissions on the calling process.
    """

    override = os.environ.get("OCTET_CUA_DESKTOP_APP")
    if override:
        path = Path(override)
        return path if path.exists() else None
    candidate = DESKTOP_APP_CANDIDATES.get(platform.system().lower())
    if not candidate:
        return None
    path = Path(candidate)
    return path if path.exists() else None


def desktop_app_binary(app: Optional[Path] = None) -> Optional[Path]:
    """The driver executable inside an installed desktop host bundle."""

    host = app or desktop_app()
    if host is None:
        return None
    inner = (
        host / "Contents" / "Resources" / "cua-driver"
        if platform.system().lower() == "darwin"
        else host
    )
    return inner if inner.is_file() else None


def _permission_status(binary: Path) -> str:
    completed = _run([str(binary), "permissions", "status", "--json"])
    if completed.returncode != 0:
        return "unknown"
    try:
        payload = json.loads(completed.stdout or "{}")
    except json.JSONDecodeError:
        return "unknown"
    status = payload.get("status")
    return status if isinstance(status, str) and status else "unknown"


def permission_state(client: Any, *, prompt: bool = False) -> Dict[str, Any]:
    """Report the host's real TCC state over the live ``--direct`` MCP channel.

    ``cua-driver permissions status`` only answers from a CuaDriver *daemon*, so
    on the pip-provisioned macOS path - which ships a bare binary, never
    ``/Applications/CuaDriver.app`` - it reports ``unknown`` even when both
    grants are present. ``check_permissions`` over the already-running direct
    session reports the responsible host's real state instead, which is the
    process octet actually is. Pass ``prompt=True`` only from an explicit,
    user-initiated setup: the driver never prompts in host-inherit mode, so the
    macOS dialog is raised by this call on the host's behalf.
    """

    arguments: Dict[str, Any] = {"prompt": bool(prompt)}
    if prompt:
        # Staged request: Accessibility + Screen Recording only. Direct-capture
        # consent is a separate platform step, not part of first-run setup.
        arguments["probe_direct_capture"] = False
    try:
        result = client.call("check_permissions", arguments)
    except Exception:
        return {
            "permissions": "unknown",
            "accessibility": None,
            "screen_recording": None,
            "detail": "the driver did not answer a permission probe",
        }
    structured = result.get("structuredContent") or {}
    accessibility = structured.get("accessibility")
    screen_recording = structured.get("screen_recording")
    granted = accessibility is True and screen_recording is True
    if granted:
        status = "granted"
    elif accessibility is False or screen_recording is False:
        status = "denied"
    else:
        status = "unknown"
    missing = []
    if accessibility is False:
        missing.append("Accessibility")
    if screen_recording is False:
        missing.append("Screen Recording")
    detail = "Accessibility and Screen Recording are allowed" if granted else (
        "still needs: " + " and ".join(missing) if missing else "permission state is unknown"
    )
    return {
        "permissions": status,
        "accessibility": accessibility,
        "screen_recording": screen_recording,
        "detail": detail,
    }


def health(paths: DriverPaths) -> Health:
    """Report whether the driver is present and permitted, without prompting."""

    binary = installed_binary(paths)
    if binary is None:
        return Health(
            installed=False,
            version=None,
            permissions="unknown",
            doctor_ok=False,
            detail=f"{DISTRIBUTION} is not provisioned yet",
        )
    version = driver_version(binary)
    permissions = _permission_status(binary)
    doctor = _run([str(binary), "doctor", "--json"])
    doctor_ok = False
    detail = ""
    if doctor.returncode == 0:
        try:
            payload = json.loads(doctor.stdout or "{}")
            doctor_ok = bool(payload.get("ok"))
            detail = f"cua-driver {version or 'unknown'}"
        except json.JSONDecodeError:
            detail = "doctor returned unreadable output"
    else:
        detail = "doctor reported a failing probe"
    return Health(
        installed=True,
        version=version,
        permissions=permissions,
        doctor_ok=doctor_ok,
        detail=detail,
    )
