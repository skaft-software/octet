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
# Launching a desktop host is a quick LaunchServices handoff, not a daemon wait.
LAUNCH_TIMEOUT_SECONDS = 20
# After launching a host, allow the driver a bounded moment to publish its socket
# and read its TCC grants back. Kept short so an ungranted host cannot stall a
# tool call, and bounded so a host that never becomes usable is given up on.
HOST_START_ATTEMPTS = 3
HOST_START_INTERVAL_SECONDS = 2.0
# Probing a host's own daemon is a short MCP round trip, not a full tool
# timeout. Kept small so a wedged daemon cannot stall runtime selection.
DESKTOP_PROBE_TIMEOUT_SECONDS = 20


def desktop_app_socket() -> Optional[Path]:
    """The unix socket a desktop host's daemon publishes.

    The driver resolves this itself, so this only answers "is a daemon
    listening?", which is what decides whether probing is worth attempting.
    """

    override = os.environ.get("OCTET_CUA_DAEMON_SOCKET")
    if override:
        return Path(override)
    if platform.system().lower() != "darwin":
        return None
    return Path.home() / "Library" / "Caches" / "cua-driver" / "cua-driver.sock"


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
    def site_packages(self) -> Path:
        """Site-packages inside the provisioned venv.

        The optional TypeSafe SDK is installed here rather than into octet's own
        interpreter, because that interpreter is not guaranteed to have pip. The
        extension then imports it from this path.
        """

        return (
            self.venv
            / "lib"
            / f"python{sys.version_info.major}.{sys.version_info.minor}"
            / "site-packages"
        )

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


#: The optional TypeSafe SDK, installed into the same octet-owned venv as the
#: driver. It is optional: nothing in computer use requires it.
JEV_DISTRIBUTION = "typesafe-sdk"


def provision_jev(
    paths: DriverPaths,
    *,
    version: str = "",
    timeout: int = INSTALL_TIMEOUT_SECONDS,
) -> bool:
    """Install the optional TypeSafe SDK into the driver venv.

    Returns True when the SDK is importable afterwards, False when it could not
    be installed. This is a convenience for the setup command, not a hard
    requirement: JEV stays optional and computer use works without it.

    The SDK goes into the octet-owned venv rather than octet's own interpreter,
    because that interpreter is not guaranteed to have pip.
    """

    python = _venv_python_for(paths.venv)
    if not python.is_file():
        return False
    environment = _install_environment()
    spec = f"{JEV_DISTRIBUTION}{version}" if version else JEV_DISTRIBUTION
    try:
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
    except ProvisionError:
        return False
    if install.returncode != 0:
        return False
    # Confirm the SDK is actually importable from the venv before reporting
    # success; pip can exit zero without the module being usable.
    try:
        probe = _run(
            [str(python), "-c", "import typesafe_sdk"],
            env=environment,
            timeout=60,
        )
    except ProvisionError:
        return False
    return probe.returncode == 0


@dataclass(frozen=True)
class Health:
    """A bounded, content-free health summary safe to show in a UI."""

    installed: bool
    version: Optional[str]
    permissions: str
    doctor_ok: bool
    detail: str
    #: Which runtime the tools will actually use: ``direct`` or ``desktop-host``.
    #: The direct runtime inherits the calling host's grants and is the shipped
    #: default; the desktop host is optional and adds only the agent cursor.
    runtime: str = "direct"

    def as_dict(self) -> Dict[str, Any]:
        return {
            "installed": self.installed,
            "version": self.version,
            "permissions": self.permissions,
            "doctor_ok": self.doctor_ok,
            "detail": self.detail,
            "runtime": self.runtime,
        }


def active_runtime() -> str:
    """Which runtime the tools will use right now.

    Kept in one place so the status surface and the client cannot disagree about
    which path is live.
    """

    return "desktop-host" if desktop_app_usable() else "direct"


# A desktop host, when installed, owns the OS permission identity and the GUI
# main thread. On macOS that is the only thing that can draw the agent cursor:
# the overlay needs a certified AppKit main thread and Window Server access that
# a terminal-hosted process does not have. ChatGPT.app takes the same shape - it
# embeds the driver inside a signed app and inherits that app's TCC grants.
#
# Cua ships its own signed macOS bundles and octet can also build one. All are
# installed by their own tooling; octet uses whichever is present rather than
# shipping a driver binary of its own.
#
#   /Applications/CuaDriver.app         Cua release build, com.trycua.driver,
#                                      Developer ID signed and notarized
#   /Applications/OctetComputerUse.app  octet's host, com.octet.computeruse
#   /Applications/CuaDriverLocal.app    Cua source build, com.trycua.driver.local
#
# Identifiers are deliberately distinct: sharing one would merge TCC rows, so
# whichever app installed last would inherit permissions granted for the other.
DESKTOP_APP_CANDIDATES: Dict[str, Tuple[str, ...]] = {
    "darwin": (
        # Cua's own signed, notarized release app is the preferred host. It is
        # the only desktop host a user can be asked to trust and ship, so it
        # must win whenever it is installed.
        "/Applications/CuaDriver.app",
        # Octet's self-signed host is a fallback, not the default. It works, but
        # an unnotarized app asking for screen recording and Accessibility is a
        # poor thing to hand a user, so it is only adopted when the official app
        # is absent or cannot keep its grant.
        "/Applications/OctetComputerUse.app",
        "/Applications/CuaDriverLocal.app",
    ),
    "win32": (os.path.expandvars(r"%LOCALAPPDATA%\\CuaDriver\\CuaDriver.exe"),),
}

# Driver executables that can appear inside a macOS bundle. Cua's own bundles
# name their driver after one of these; octet's host app is named
# ``OctetComputerUseHost`` and ships the driver alongside it under the first of
# these names.
_MACOS_BUNDLE_DRIVERS = ("cua-driver", "cua-driver-local")


def _bundle_executable_name(app: Path) -> Optional[str]:
    """``CFBundleExecutable`` from a macOS bundle's ``Info.plist``.

    Read through ``plutil`` rather than a plist parser so the extension keeps no
    extra import and works with the system plist format, including the binary
    variant the release bundle ships.
    """

    plist = app / "Contents" / "Info.plist"
    if not plist.is_file():
        return None
    try:
        completed = _run(
            ["/usr/bin/plutil", "-extract", "CFBundleExecutable", "raw", "-o", "-", str(plist)]
        )
    except OSError:
        return None
    if completed.returncode != 0:
        return None
    name = (completed.stdout or "").strip()
    return name or None


def desktop_app() -> Optional[Path]:
    """The installed CuaDriver desktop host, when one is present.

    Absence is not an error: the direct runtime still drives the desktop, it
    just has no cursor overlay and keeps permissions on the calling process.
    """

    override = os.environ.get("OCTET_CUA_DESKTOP_APP")
    if override:
        path = Path(override)
        return path if path.exists() else None
    for candidate in DESKTOP_APP_CANDIDATES.get(platform.system().lower(), ()):
        path = Path(candidate)
        if path.exists():
            return path
    return None


def desktop_app_binary(app: Optional[Path] = None) -> Optional[Path]:
    """The driver executable inside an installed desktop host bundle.

    This must return a *driver*, never the host app itself: the client speaks
    MCP to it, and an app bundle's declared ``CFBundleExecutable`` is its own
    entry point. For Cua's bundles those coincide, but octet's host app declares
    ``OctetComputerUseHost`` and carries the driver beside it, so a declared
    name that is not a known driver is skipped rather than trusted.
    """

    host = app or desktop_app()
    if host is None:
        return None
    if platform.system().lower() != "darwin":
        return host if host.is_file() else None
    macos = host / "Contents" / "MacOS"
    for name in _MACOS_BUNDLE_DRIVERS:
        inner = macos / name
        if inner.is_file():
            return inner
    declared = _bundle_executable_name(host)
    if declared and declared in _MACOS_BUNDLE_DRIVERS:
        inner = macos / declared
        if inner.is_file():
            return inner
    return None


def desktop_app_display_name(app: Optional[Path] = None) -> Optional[str]:
    """LaunchServices name for a desktop host, for ``open -a``.

    Resolving by display name is unreliable for an ``LSUIElement`` bundle, so
    launch by bundle identifier when one is available.
    """

    host = app or desktop_app()
    if host is None:
        return None
    if platform.system().lower() != "darwin":
        return None
    plist = host / "Contents" / "Info.plist"
    if not plist.is_file():
        return None
    try:
        completed = _run(
            ["/usr/bin/plutil", "-extract", "CFBundleIdentifier", "raw", "-o", "-", str(plist)]
        )
    except OSError:
        return None
    if completed.returncode != 0:
        return None
    return (completed.stdout or "").strip() or None


def start_desktop_app(app: Optional[Path] = None) -> bool:
    """Launch a desktop host if it is installed but not already running.

    Best-effort: the caller still works without it, because the direct runtime
    drives the desktop on its own. A host that cannot be launched is not an
    error, it just means no cursor.
    """

    host = app or desktop_app()
    if host is None or platform.system().lower() != "darwin":
        return False
    identifier = desktop_app_display_name(host)
    arguments = ["/usr/bin/open", "-n", "-g"]
    # ``-g`` keeps the app in the background: a foreground automation host would
    # steal focus from whatever the user is actually doing.
    arguments += ["-b", identifier] if identifier else ["-a", host.name[:-4]]
    try:
        completed = _run(arguments, timeout=LAUNCH_TIMEOUT_SECONDS)
    except ProvisionError:
        return False
    return completed.returncode == 0


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


def desktop_app_permissions(binary: Optional[Path] = None) -> str:
    """Authoritative grant status for a desktop host, read from its own daemon.

    ``cua-driver permissions status`` is not usable for this decision. That CLI
    answers only for a daemon whose identity it recognises, and reports
    ``unknown`` for any other bundle - including octet's own host - even when
    that host's Accessibility and Screen Recording grants are fully live. It
    also documents that it deliberately skips the direct-capture probe on
    Tahoe. So a host verified only through the CLI would be discarded and the
    cursor silently lost.

    Ask the daemon instead, over the same MCP channel the tools use, and return
    ``granted`` only when both capabilities are live. ``unknown`` means the
    daemon could not be reached or the answer was unreadable.
    """

    if binary is None:
        binary = desktop_app_binary()
    if binary is None:
        return "unknown"
    socket_path = desktop_app_socket()
    if socket_path is None or not socket_path.exists():
        return "unknown"

    from octet_computer_use.driver_client import DriverClient

    client = DriverClient(binary, app_daemon=True)
    try:
        client.start(timeout=DESKTOP_PROBE_TIMEOUT_SECONDS)
        result = client.call("check_permissions", {})
    except Exception:
        # Probing must never be the reason a tool call fails: an unreachable
        # daemon is reported as unknown so the caller falls back cleanly.
        return "unknown"
    finally:
        try:
            client.close()
        except Exception:
            pass

    text = ""
    content = result.get("content") if isinstance(result, dict) else None
    if isinstance(content, list) and content:
        first = content[0]
        if isinstance(first, dict):
            text = str(first.get("text") or "")
    granted = text.count("granted.") >= 2
    if granted:
        return "granted"
    if "pending" in text or "not granted" in text:
        return "denied"
    return "unknown"


def desktop_app_usable(binary: Optional[Path] = None) -> bool:
    """Whether the installed desktop host can actually be driven.

    An installed host is not automatically a working one. The macOS grant can
    install and then fail to persist, in which case the host re-prompts on every
    launch and every tool call comes back ``permissions_pending``. Adopting such a
    host would be worse than not having one, because the direct runtime inherits
    the calling host's grants and works immediately.

    So treat a non-granted host as unusable and let the caller fall back. This
    only ever *narrows* host use: an app that cannot prove its grants is never
    silently trusted to hold the agent cursor.
    """

    if binary is None:
        binary = desktop_app_binary()
    if binary is None:
        return False
    if desktop_app_permissions(binary) == "granted":
        return True
    # A host that is installed but has no live grant usually just needs to be
    # running: the daemon, not the bundle, is what owns the permission probe.
    # Try once to bring it up before rejecting it, because a running host is
    # what makes the cursor available.
    if start_desktop_app():
        # Give the daemon a moment to publish its socket and read TCC back.
        for _ in range(HOST_START_ATTEMPTS):
            time.sleep(HOST_START_INTERVAL_SECONDS)
            if desktop_app_permissions(binary) == "granted":
                return True
    return False


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
        runtime=active_runtime(),
    )
