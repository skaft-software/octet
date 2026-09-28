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
import hashlib
import json
import os
import platform
import shutil
import stat
import subprocess
import sys
import time
import urllib.request
import zipfile
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from octet_computer_use.windows_security import is_link_or_reparse_point

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

        A Windows venv keeps site-packages unversioned at ``Lib/site-packages``
        next to ``Scripts/python.exe``, so the POSIX ``lib/pythonX.Y`` layout
        would name a directory that never exists there and silently make the SDK
        look uninstalled.
        """

        if platform.system() == "Windows":
            return self.venv / "Lib" / "site-packages"
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
    # Force UTF-8 stdio so pip/driver output decodes deterministically
    # regardless of the Windows ANSI code page (see `_run`).
    environment["PYTHONUTF8"] = "1"
    environment["PYTHONIOENCODING"] = "utf-8"
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
            # Without an explicit encoding, Windows decodes with the ANSI
            # code page (cp1252): any byte undefined there (e.g. from pip or
            # `--version` banners) raises `UnicodeDecodeError` out of
            # `communicate()`, which is neither `TimeoutExpired` nor
            # `OSError` and would escape uncaught below.
            encoding="utf-8",
            errors="replace",
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
    # the root is a symlink (or, on Windows, a junction or other reparse point)
    # so we never write through an attacker-planted link.
    if paths.root.is_symlink() or is_link_or_reparse_point(paths.root.lstat()):
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
    # The venv path can contain non-ASCII characters (non-ASCII usernames);
    # run the probe under the sanitized UTF-8 environment so the printed
    # path decodes to the real path.
    probe = _run(
        [str(python), "-c", "import cua_driver, sys; sys.stdout.write(str(cua_driver.get_binary_path()))"],
        env=_install_environment(),
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


def _release_tuple(version: str) -> Optional[Tuple[int, ...]]:
    """The dotted numeric release segment, or None for anything else.

    A pre/post/dev/local marker returns None rather than being ordered here:
    PEP 440 precedence and local ordering are not implemented in this module,
    and guessing at them would let a mismatched pin pass.
    """

    parts = version.split(".")
    if not parts or not all(part.isdigit() for part in parts):
        return None
    return tuple(int(part) for part in parts)


def _versions_match(requested: str, installed: str) -> bool:
    """Whether an installed version satisfies an exact requested pin.

    PEP 440 compares release segments with zero padding, so ``0.29`` and
    ``0.29.0`` name the same release and a caller who abbreviates a pin must not
    trigger a reinstall on every call. Anything carrying a marker falls back to
    exact text equality.
    """

    requested = requested.strip()
    installed = installed.strip()
    if requested == installed:
        return True
    left, right = _release_tuple(requested), _release_tuple(installed)
    if left is None or right is None:
        return False
    width = max(len(left), len(right))
    return left + (0,) * (width - len(left)) == right + (0,) * (width - len(right))


def _satisfies_request(binary: Path, requested: str) -> bool:
    """Whether an already-installed driver satisfies the caller's request.

    An unpinned request wants the newest release, so an existing install always
    satisfies it. A pinned request is satisfied only by a matching reported
    version: reporting a mismatch as provisioned would hand back a runtime the
    caller did not ask for. An install that cannot report a version at all is
    treated as unsatisfied, because the pin cannot be confirmed.
    """

    if not requested:
        return True
    reported = driver_version(binary)
    if reported is None:
        return False
    return _versions_match(requested, reported)


def provision(
    paths: DriverPaths,
    *,
    version: str = DEFAULT_VERSION,
    timeout: int = INSTALL_TIMEOUT_SECONDS,
) -> Path:
    """Provision the driver into the octet-owned venv and return its binary.

    An unpinned request is idempotent: an installed driver is reused as-is. A
    pinned request is idempotent only when the installed driver actually reports
    that version; otherwise the octet-owned venv is cleared and the pinned
    version installed, so a stale or mismatched runtime is never silently
    returned. The install performs a network download from the configured
    package index; the caller is responsible for having obtained user consent
    for that network access.
    """

    _ensure_directories(paths)
    # Validate the request before anything else. An explicit pin must be checked
    # against what is installed, never satisfied by whatever happens to be
    # present, and a malformed pin must not be reported as a successful reuse.
    spec = _pip_spec(version)
    requested = version.strip()
    existing = installed_binary(paths)
    if existing is not None and _satisfies_request(existing, requested):
        return existing

    # A mismatch between the request and what is installed re-enters the owned
    # venv from scratch, so a version switch cannot leave a half-upgraded
    # runtime behind.
    paths.venv.mkdir(parents=True, exist_ok=True)
    environment = _install_environment()

    create = _run(
        [sys.executable, "-m", "venv", "--clear", str(paths.venv)],
        env=environment,
        timeout=timeout,
    )
    if create.returncode != 0:
        # Debian and Ubuntu split `ensurepip` into python3-venv, so a stock
        # interpreter cannot bootstrap pip into a venv. Install the published
        # wheel directly instead of asking the user to install a system package.
        if _direct_wheel_supported():
            return _provision_without_pip(paths, requested, environment, timeout)
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
        output = (install.stderr or install.stdout or "")
        if "No module named pip" in output and _direct_wheel_supported():
            return _provision_without_pip(paths, requested, environment, timeout)
        raise ProvisionError(
            f"failed to install {spec}: {output.strip()[:400]}"
        )

    binary = installed_binary(paths)
    if binary is None:
        raise ProvisionError(f"{spec} installed but no driver executable was found")
    return binary


# The wheel-only fallback reads the package index's JSON API and downloads the
# single matching wheel, verified against the index's own SHA-256 digest.
PACKAGE_INDEX_JSON = "https://pypi.org/pypi"
_MAX_INDEX_BYTES = 8 * 1024 * 1024
_MAX_WHEEL_BYTES = 512 * 1024 * 1024
# Cua publishes manylinux_2_31 wheels, so the host C library must be glibc 2.31+.
_MANYLINUX_GLIBC = (2, 31)
_LINUX_ARCHES = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}


def _direct_wheel_supported() -> bool:
    return (platform.system().lower() == "linux"
            and platform.machine().lower() in _LINUX_ARCHES)


def _glibc_version() -> Optional[Tuple[int, int]]:
    library, version = platform.libc_ver()
    if library != "glibc":
        return None
    parts = version.split(".")
    try:
        return int(parts[0]), int(parts[1])
    except (IndexError, ValueError):
        return None


def _select_wheel(files: Any, arch: str) -> Mapping[str, Any]:
    if not isinstance(files, list):
        raise ProvisionError("the package index returned no release files")
    for entry in files:
        if not isinstance(entry, Mapping):
            continue
        name = entry.get("filename")
        digest = (entry.get("digests") or {}).get("sha256") if isinstance(entry.get("digests"), Mapping) else None
        if (isinstance(name, str) and name.endswith(".whl") and "manylinux" in name
                and name.endswith(f"_{arch}.whl") and isinstance(entry.get("url"), str)
                and entry["url"].startswith("https://") and isinstance(digest, str)):
            return entry
    raise ProvisionError(f"{DISTRIBUTION} publishes no Linux {arch} wheel for this release")


def _safe_member(name: str) -> bool:
    parts = name.split("/")
    if name.startswith("/") or "\\" in name or any(part in ("", ".", "..") for part in parts):
        return False
    top = parts[0]
    return top == "cua_driver" or (top.startswith("cua_driver-") and top.endswith(".dist-info"))


def _provision_without_pip(
    paths: DriverPaths, version: str, environment: Mapping[str, str], timeout: int
) -> Path:
    """Install the published Linux wheel into a pip-less venv.

    The wheel carries no dependencies and no build step: it is the
    ``cua_driver`` package, its bundled executable, and its dist-info. Only
    those paths are extracted, only from the file whose SHA-256 matches the
    package index's published digest.
    """

    arch = _LINUX_ARCHES[platform.machine().lower()]
    glibc = _glibc_version()
    if glibc is None or glibc < _MANYLINUX_GLIBC:
        raise ProvisionError(
            f"{DISTRIBUTION} needs GNU libc {_MANYLINUX_GLIBC[0]}.{_MANYLINUX_GLIBC[1]} or newer"
        )
    create = _run(
        [sys.executable, "-m", "venv", "--clear", "--without-pip", str(paths.venv)],
        env=environment, timeout=timeout,
    )
    if create.returncode != 0:
        raise ProvisionError(
            f"failed to create runtime venv: {(create.stderr or create.stdout or '').strip()[:400]}"
        )
    url = f"{PACKAGE_INDEX_JSON}/{DISTRIBUTION}/{version}/json" if version else \
        f"{PACKAGE_INDEX_JSON}/{DISTRIBUTION}/json"
    try:
        with urllib.request.urlopen(url, timeout=60) as response:
            index = json.loads(response.read(_MAX_INDEX_BYTES + 1)[:_MAX_INDEX_BYTES])
    except (OSError, ValueError) as error:
        raise ProvisionError(f"could not read the package index for {DISTRIBUTION}: {error}") from error
    wheel = _select_wheel(index.get("urls") if isinstance(index, Mapping) else None, arch)
    archive = paths.root / (".%s.%d.part" % (wheel["filename"], os.getpid()))
    digest = hashlib.sha256()
    try:
        with urllib.request.urlopen(wheel["url"], timeout=timeout) as response, \
                open(archive, "wb") as sink:
            size = 0
            while True:
                chunk = response.read(1 << 20)
                if not chunk:
                    break
                size += len(chunk)
                if size > _MAX_WHEEL_BYTES:
                    raise ProvisionError(f"{wheel['filename']} exceeds the download ceiling")
                digest.update(chunk)
                sink.write(chunk)
        if digest.hexdigest() != wheel["digests"]["sha256"]:
            raise ProvisionError(f"{wheel['filename']} does not match its published SHA-256")
        site = paths.site_packages
        site.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(archive) as bundle:
            for member in bundle.infolist():
                if member.is_dir():
                    continue
                if not _safe_member(member.filename):
                    raise ProvisionError(f"{wheel['filename']} contains an unexpected path")
                target = site / member.filename
                target.parent.mkdir(parents=True, exist_ok=True)
                with bundle.open(member) as source, open(target, "wb") as sink:
                    shutil.copyfileobj(source, sink)
                mode = (member.external_attr >> 16) & 0o777
                if mode & stat.S_IXUSR:
                    target.chmod(0o755)
    except OSError as error:
        raise ProvisionError(f"failed to install {wheel['filename']}: {error}") from error
    finally:
        if archive.exists():
            archive.unlink()
    binary = installed_binary(paths)
    if binary is None:
        raise ProvisionError(f"{wheel['filename']} installed but no driver executable was found")
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
    runtime: str = "direct"
    runtime_binary: Optional[str] = None
    host_app: Optional[str] = None
    cursor_available: bool = False
    cursor_enabled: bool = False

    def as_dict(self) -> Dict[str, Any]:
        return {
            "installed": self.installed,
            "version": self.version,
            "permissions": self.permissions,
            "doctor_ok": self.doctor_ok,
            "detail": self.detail,
            "runtime": self.runtime,
            "runtime_binary": self.runtime_binary,
            "host_app": self.host_app,
            "cursor_available": self.cursor_available,
            "cursor_enabled": self.cursor_enabled,
            "platform": host_platform(),
        }


def desktop_host_requested() -> bool:
    """Whether desktop-host mode was selected, preserving the explicit opt-out."""

    override = os.environ.get("OCTET_CUA_DESKTOP_HOST")
    if override is not None:
        return override.strip().lower() not in ("0", "false", "no", "off")
    return True


def cursor_host_required() -> bool:
    """macOS defaults to the signed app host because that is the cursor runtime."""

    return platform.system().lower() == "darwin" and desktop_host_requested()


def active_runtime() -> str:
    """Return the runtime that can actually be used, or ``unavailable``."""

    requested = desktop_host_requested()
    if requested and desktop_app_usable():
        return "desktop-host"
    if cursor_host_required():
        return "unavailable"
    return "direct"


# The signed Cua Driver app supplies the macOS daemon, permissions identity,
# AppKit host, and agent-cursor overlay. Other local host builds are developer
# overrides only via OCTET_CUA_DESKTOP_APP; they are never silently selected.
DESKTOP_APP_CANDIDATES: Dict[str, Tuple[str, ...]] = {
    "darwin": ("/Applications/CuaDriver.app",),
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
    except (OSError, ProvisionError):
        return None
    if completed.returncode != 0:
        return None
    name = (completed.stdout or "").strip()
    return name or None


def desktop_app() -> Optional[Path]:
    """The selected desktop host, if present.

    macOS selects the signed Cua Driver app by default. An alternate host is
    considered only when explicitly named with OCTET_CUA_DESKTOP_APP.
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
    except (OSError, ProvisionError):
        return None
    if completed.returncode != 0:
        return None
    return (completed.stdout or "").strip() or None


def start_desktop_app(app: Optional[Path] = None) -> bool:
    """Launch a desktop host if it is installed but not already running.

    Best-effort: when this host is required, failure to launch is reported as
    unavailable rather than silently switching to a cursorless runtime. The
    explicit direct-runtime opt-out can still operate without the host.
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


def host_platform() -> str:
    """``darwin``, ``windows``, ``linux``, or another lowercased system name."""

    return platform.system().lower()


def _permission_status(binary: Path) -> str:
    # `permissions status` reads macOS TCC grants through a CuaDriver daemon.
    # Elsewhere it has nothing to report, and on Linux it answers with macOS
    # instructions, so leave the decision to the live session probe.
    if host_platform() != "darwin":
        return "unknown"
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
        # An unreachable daemon is unknown; runtime selection decides whether
        # to fail closed or use an explicitly selected direct mode.
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
    launch and every tool call comes back ``permissions_pending``. Such a host's
    cursor cannot be trusted; direct mode is a separate, explicit choice.

    So treat a non-granted host as unusable. On macOS, the default policy then
    reports computer use as unavailable rather than silently switching to a
    cursorless direct runtime; direct mode is available only when explicitly
    selected by the user or on platforms that do not require the cursor host.
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
    """Report the host's real TCC state over the selected live MCP channel.

    ``cua-driver permissions status`` only answers from a CuaDriver *daemon*, so
    on the pip-provisioned direct path it reports ``unknown`` even when both
    grants are present. ``check_permissions`` over the already-running selected
    session reports the responsible host's real state instead. Pass
    ``prompt=True`` only from an explicit, user-initiated setup: the driver never
    prompts in host-inherit mode, so the macOS dialog is raised by this call on
    the host's behalf.
    """

    if host_platform() == "linux":
        return _linux_session_state(client)
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


def _linux_session_state(client: Any) -> Dict[str, Any]:
    """Linux readiness is a reachable display session, not a system grant.

    There is no Accessibility or Screen Recording permission to hold on Linux.
    The driver instead needs an X11 display or a Wayland session with its native
    backend enabled, and AT-SPI on the session bus for element trees. Without
    AT-SPI the driver still captures and acts by pixel, so it is reported but
    does not hold actions back. Nothing here prompts.
    """

    try:
        result = client.call("check_permissions", {"prompt": False})
    except Exception:
        return {
            "permissions": "unknown",
            "detail": "the driver did not answer a desktop-session probe",
        }
    structured = result.get("structuredContent") or {}
    if not isinstance(structured, Mapping):
        structured = {}
    x11 = structured.get("x11")
    wayland = structured.get("wayland")
    wayland_enabled = structured.get("wayland_enabled")
    atspi = structured.get("atspi")
    native_wayland = wayland is True and wayland_enabled is True
    if x11 is True and native_wayland:
        display_server = "wayland+x11"
    elif native_wayland:
        display_server = "wayland"
    elif x11 is True:
        display_server = "x11"
    else:
        display_server = None

    if display_server is not None:
        status = "granted"
        names = {"wayland+x11": "Wayland (native) and XWayland",
                 "wayland": "Wayland (native)", "x11": "X11"}
        detail = names[display_server] + " reachable"
        if atspi is True:
            detail += "; AT-SPI accessibility available"
        elif atspi is False:
            detail += "; AT-SPI unavailable, so element trees are empty and actions go by pixel"
    elif x11 is False and wayland is not None:
        status = "denied"
        if wayland is True:
            detail = ("a Wayland session is present but the driver's native Wayland "
                      "backend is off and no XWayland display is reachable")
        else:
            detail = ("no display session is reachable: start octet from a terminal "
                      "inside your graphical session")
    else:
        status = "unknown"
        detail = "desktop session state is unknown"

    state: Dict[str, Any] = {"permissions": status, "detail": detail,
                             "display_server": display_server}
    for key, value in (("x11", x11), ("wayland", wayland), ("atspi", atspi)):
        if isinstance(value, bool):
            state[key] = value
    return state


def health(paths: DriverPaths) -> Health:
    """Report health for the exact binary selected for dispatch, without prompting."""

    runtime = active_runtime()
    host = desktop_app() if runtime == "desktop-host" else None
    binary = desktop_app_binary(host) if host is not None else (
        installed_binary(paths) if runtime == "direct" else None
    )
    if runtime == "unavailable":
        present = desktop_app_binary() is not None or installed_binary(paths) is not None
        return Health(
            installed=present,
            version=None,
            permissions="unknown",
            doctor_ok=False,
            detail=(
                "The signed Cua Driver app is unavailable or lacks live permissions; "
                "refusing to fall back to a cursorless direct runtime."
            ),
            runtime="unavailable",
            host_app=str(desktop_app()) if desktop_app() else None,
        )
    if binary is None:
        return Health(
            installed=False,
            version=None,
            permissions="unknown",
            doctor_ok=False,
            detail=f"{DISTRIBUTION} is not provisioned for the selected runtime",
            runtime=runtime,
            host_app=str(host) if host else None,
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
        runtime=runtime,
        runtime_binary=str(binary),
        host_app=str(host) if host else None,
        cursor_available=(runtime == "desktop-host"),
    )
