"""Managed, pinned upstream jev-use recipe, not a general desktop agent.

Only setup downloads/installs. Run invokes upstream verify_setup unchanged;
its independent fixture readback (not a model assertion) is checked again here.
All raw diagnostics stay in private, bounded artifacts. No automatic retries.
"""
from __future__ import annotations

import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import shutil
import signal
import subprocess
import tarfile
import tempfile
import threading
import time
import urllib.request

from . import jev

COMMIT = "1fadc40b3e40e042590c9f63cf8ef837a6263258"
ARCHIVE_URL = f"https://codeload.github.com/trycua/cua/tar.gz/{COMMIT}"
ARCHIVE_SHA256 = "4a1c02562eb2fa4b325777b61bb97075ec9c409feb5bfba501a341888c5f7e5f"
PREFIX = f"cua-{COMMIT}/libs/cua-driver/examples/jev-use/"
MAX_ARCHIVE = 256 * 1024 * 1024
MAX_LOG = 1024 * 1024
_ACTIVE_LOCK = threading.Lock()
_ACTIVE_PROCESSES = {}
_STOPPING = threading.Event()


def cancel_all_processes():
    """Signal owned jobs immediately; worker threads retain cleanup/wait ownership.

    Called at extension shutdown, which is terminal for this process. Sets the
    process-wide stopping flag so a subprocess racing creation is also
    signalled. No process scans, sleeps or joins occur here. Windows closes
    owned jobs. POSIX interrupts the owned group so Python's asyncio/MCP
    context managers can close their separate-session transports. This does
    not claim reclamation of previously reparented POSIX descendants and
    reports ``cleanup_complete: False``. Already-reparented descendants
    cannot be conclusively reclaimed by a retrospective PID scan.
    """
    _STOPPING.set()
    with _ACTIVE_LOCK:
        active = list(_ACTIVE_PROCESSES.values())
    for process, close_job in active:
        if close_job is not None:
            close_job()
        elif process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
    return {"signalled": len(active), "cleanup_complete": False}


def reset_shutdown_state():
    """Clear the process-wide stopping flag (tests only).

    Production extension shutdown is terminal for the process and never
    clears this flag. Test managers sharing one Python process must call
    this in setUp/tearDown so one manager's shutdown does not permanently
    cancel later managers.
    """
    _STOPPING.clear()


class RuntimeFailure(Exception):
    pass


def _root(home=None):
    return (Path(home) if home is not None else Path.home()).resolve() / ".octet" / "computer-use" / "jev-use"


def _cancelled(cancellation):
    if cancellation is None:
        return False
    if callable(cancellation):
        return bool(cancellation())
    if hasattr(cancellation, "cancelled"):
        value = cancellation.cancelled
        return bool(value() if callable(value) else value)
    return bool(cancellation.is_set())


def _check(cancellation):
    if _STOPPING.is_set() or _cancelled(cancellation):
        raise RuntimeFailure("cancelled")


def _private_dir(path):
    # Reject symlinks at every existing component, including user state parents.
    for item in (path, *path.parents):
        if item.is_symlink():
            raise RuntimeFailure("symlink_state_path")
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.chmod(0o700)
    if os.name == "nt":
        _windows_private(path)


def _windows_private(path):
    """Protected, inheritable owner-only DACL (chmod alone is not private on Windows).

    Unexercised on Windows in this checkout; covered only by POSIX tests and
    a suspended-launch unit test with mocked OS calls.
    """
    import ctypes
    from ctypes import wintypes as w
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    convert = advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW
    convert.argtypes = [w.LPCWSTR, w.DWORD, ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p]
    convert.restype = w.BOOL
    apply = advapi.SetFileSecurityW
    apply.argtypes = [w.LPCWSTR, w.DWORD, ctypes.c_void_p]
    apply.restype = w.BOOL
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.LocalFree.restype = ctypes.c_void_p
    descriptor = ctypes.c_void_p()
    if not convert("D:P(A;OICI;FA;;;OW)", 1, ctypes.byref(descriptor), None):
        raise RuntimeFailure("private_acl_creation_failed")
    try:
        if not apply(str(path), 0x80000004, descriptor):
            raise RuntimeFailure("private_acl_failed")
    finally:
        kernel.LocalFree(descriptor)


def _json(path, limit=MAX_LOG):
    if path.is_symlink():
        raise RuntimeFailure("symlink_artifact")
    with path.open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise RuntimeFailure("artifact_limit")
    return json.loads(data)


def _python(runtime):
    return runtime / ".venv" / ("Scripts" if os.name == "nt" else "bin") / ("python.exe" if os.name == "nt" else "python")


def status(home=None):
    root = _root(home)
    runtime = root / COMMIT
    prepared = False
    ts = False
    try:
        if any(path.is_symlink() for path in (runtime, *runtime.parents)):
            raise RuntimeFailure("symlink_state_path")
        manifest = _json(runtime / "octet-runtime.json")
        prepared = (manifest.get("commit") == COMMIT and
                    manifest.get("sha256") == ARCHIVE_SHA256 and
                    _python(runtime).is_file() and (runtime / "verify_setup.py").is_file())
        ts = prepared and manifest.get("typescript") is True and (runtime / "node_modules/tsx").is_dir()
    except (OSError, ValueError, RuntimeFailure, AttributeError):
        pass
    return {"available": True, "prepared": bool(prepared), "typescript_prepared": bool(ts),
            "verified": False, "commit": COMMIT, "archive_url": ARCHIVE_URL,
            "archive_sha256": ARCHIVE_SHA256, "runtime": str(runtime),
            "platform_supported": os.name in ("posix", "nt"),
            "note": "Prepared is not desktop or live Jev verification. This is the upstream fixture recipe."}


def _environment():
    names = ("HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "LANG", "LC_ALL",
             "DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR",
             "DBUS_SESSION_BUS_ADDRESS", "AT_SPI_BUS_ADDRESS", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP",
             "SYSTEMROOT", "WINDIR", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "PATHEXT")
    env = {key: os.environ[key] for key in names if key in os.environ}
    env["PYTHONNOUSERSITE"] = "1"
    return env


def _descendants(pid):
    # MCP's Python SDK starts its stdio child in a separate POSIX session.
    # ps returns only numeric identities, never command lines or secrets.
    with tempfile.TemporaryFile() as output:
        subprocess.run(["/bin/ps", "-axo", "pid=,ppid="], stdout=output,
                       stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, timeout=5, check=True)
        output.seek(0)
        raw = output.read(MAX_LOG + 1)
    if len(raw) > MAX_LOG:
        raise RuntimeFailure("process_table_limit")
    rows = [tuple(map(int, line.split())) for line in raw.splitlines()]
    owned = [pid]
    for parent in owned:
        owned.extend(child for child, ppid in rows if ppid == parent and child not in owned)
    return owned[1:]


def _kill_tree(process):
    # Freeze runners, then their separately-sessioned MCP child before killing.
    # The shared daemon is not in this owned descendant tree.
    descendants = []
    try:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGSTOP)
            for _ in range(4):
                found = _descendants(process.pid)
                fresh = [pid for pid in found if pid not in descendants]
                if not fresh:
                    break
                for pid in fresh:
                    try:
                        os.kill(pid, signal.SIGSTOP)
                    except ProcessLookupError:
                        pass
                descendants.extend(fresh)
    finally:
        for pid in reversed(descendants):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)



def _windows_job(process):
    """Assign a suspended process before it can spawn; resume inside an owned job.

    Unexercised on real Windows; only the mocked lifecycle unit test covers it.
    """
    import ctypes
    from ctypes import wintypes as w

    class Limits(ctypes.Structure):
        _fields_ = [("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64),
                    ("flags", w.DWORD), ("minimum", ctypes.c_size_t), ("maximum", ctypes.c_size_t),
                    ("active", w.DWORD), ("affinity", ctypes.c_size_t), ("priority", w.DWORD), ("scheduling", w.DWORD)]

    class Extended(ctypes.Structure):
        _fields_ = [("basic", Limits), ("io", ctypes.c_uint64 * 6),
                    ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                    ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateJobObjectW.argtypes = [ctypes.c_void_p, w.LPCWSTR]
    kernel.CreateJobObjectW.restype = w.HANDLE
    kernel.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
    kernel.SetInformationJobObject.restype = w.BOOL
    kernel.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
    kernel.AssignProcessToJobObject.restype = w.BOOL
    kernel.CloseHandle.argtypes = [w.HANDLE]
    kernel.CloseHandle.restype = w.BOOL
    handle = kernel.CreateJobObjectW(None, None)
    if not handle:
        raise RuntimeFailure("windows_job_create_failed")
    try:
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not kernel.SetInformationJobObject(handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            raise RuntimeFailure("windows_job_limits_failed")
        if not kernel.AssignProcessToJobObject(handle, int(process._handle)):
            raise RuntimeFailure("windows_job_assign_failed")
        # Popen closes the initial thread handle. Resume the suspended process
        # only after assignment; no child can escape during job setup.
        resume = ctypes.WinDLL("ntdll").NtResumeProcess
        resume.argtypes = [w.HANDLE]
        resume.restype = ctypes.c_long
        if resume(int(process._handle)) != 0:
            raise RuntimeFailure("windows_job_resume_failed")
    except Exception:
        kernel.CloseHandle(handle)
        raise
    close_lock = threading.Lock()
    closed = False
    def close_job():
        nonlocal closed
        with close_lock:
            if not closed:
                closed = True
                kernel.CloseHandle(handle)
    return close_job

def _process(argv, *, cwd, env, artifacts, timeout, cancellation=None, stdin=None):
    _check(cancellation)
    overflow = threading.Event()
    log_failed = threading.Event()
    def drain(stream, path):
        try:
            with path.open("xb") as sink:
                path.chmod(0o600)
                count = 0
                while True:
                    block = stream.read(8192)
                    if not block:
                        break
                    sink.write(block[:max(0, MAX_LOG - count)])
                    count += len(block)
                    if count > MAX_LOG:
                        overflow.set()
        except OSError:
            log_failed.set()
        finally:
            stream.close()
    # Input is a bounded private file, avoiding a blocked stdin pipe writer.
    input_stream = subprocess.DEVNULL
    if stdin is not None:
        input_path = artifacts / "request.json"
        with input_path.open("xb") as handle:
            input_path.chmod(0o600)
            handle.write(stdin)
        input_stream = input_path.open("rb")
    try:
        options = {"creationflags": 4} if os.name == "nt" else {"start_new_session": True, "umask": 0o077}
        process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=input_stream,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, **options)
        job = None
        if os.name == "nt":
            try:
                job = _windows_job(process)
            except Exception:
                process.kill()
                process.wait(timeout=5)
                raise RuntimeFailure("windows_job_setup_failed") from None
    finally:
        if stdin is not None:
            input_stream.close()
    with _ACTIVE_LOCK:
        _ACTIVE_PROCESSES[id(process)] = (process, job)
    # Handles a shutdown racing process creation without holding the registry
    # lock over CreateProcess/Popen or other potentially blocking system calls.
    if _STOPPING.is_set():
        cancel_all_processes()
    threads = [threading.Thread(target=drain, args=(stream, artifacts / name), daemon=True)
               for stream, name in ((process.stdout, "stdout.log"), (process.stderr, "stderr.log"))]
    for thread in threads:
        thread.start()
    deadline = time.monotonic() + timeout
    try:
        while process.poll() is None:
            _check(cancellation)
            if log_failed.is_set():
                raise RuntimeFailure("artifact_write_failed")
            if overflow.is_set():
                raise RuntimeFailure("output_limit")
            if time.monotonic() >= deadline:
                raise RuntimeFailure("timeout")
            time.sleep(0.05)
        _check(cancellation)
        if overflow.is_set():
            raise RuntimeFailure("output_limit")
    finally:
        try:
            if job is not None:
                job()  # Idempotent with shutdown; closes every owned descendant.
                process.wait(timeout=5)
            else:
                if _STOPPING.is_set() and process.poll() is None:
                    # Give MCP's context manager a short opportunity to clean
                    # up its separate-session child after the shutdown SIGINT.
                    try:
                        process.wait(timeout=0.5)
                    except subprocess.TimeoutExpired:
                        pass
                _kill_tree(process)
        finally:
            with _ACTIVE_LOCK:
                _ACTIVE_PROCESSES.pop(id(process), None)
            for thread in threads:
                thread.join(timeout=5)
    if log_failed.is_set():
        raise RuntimeFailure("artifact_write_failed")
    if overflow.is_set():
        raise RuntimeFailure("output_limit")
    return process.returncode


def _download(path, cancellation):
    digest = hashlib.sha256()
    count = 0
    deadline = time.monotonic() + 180
    with urllib.request.urlopen(ARCHIVE_URL, timeout=15) as response, path.open("xb") as sink:
        path.chmod(0o600)
        while True:
            _check(cancellation)
            if time.monotonic() > deadline:
                raise RuntimeFailure("download_timeout")
            chunk = response.read(65536)
            if not chunk:
                break
            count += len(chunk)
            if count > MAX_ARCHIVE:
                raise RuntimeFailure("archive_limit")
            digest.update(chunk)
            sink.write(chunk)
    if digest.hexdigest() != ARCHIVE_SHA256:
        raise RuntimeFailure("archive_digest_mismatch")


def _extract(archive, destination, cancellation):
    total = 0
    members = 0
    deadline = time.monotonic() + 180
    with tarfile.open(archive, "r|gz") as source:
        for member in source:
            _check(cancellation)
            members += 1
            if time.monotonic() > deadline:
                raise RuntimeFailure("extraction_timeout")
            if members > 100000:
                raise RuntimeFailure("archive_member_limit")
            if not member.name.startswith(PREFIX):
                continue
            relative = PurePosixPath(member.name[len(PREFIX):])
            if not relative.parts:
                continue
            if relative.is_absolute() or ".." in relative.parts or "\\" in str(relative):
                raise RuntimeFailure("unsafe_archive_path")
            target = destination.joinpath(*relative.parts)
            if member.isdir():
                _private_dir(target)
            elif member.isfile():
                total += member.size
                if total > 16 * 1024 * 1024:
                    raise RuntimeFailure("extraction_limit")
                _private_dir(target.parent)
                with source.extractfile(member) as incoming, target.open("xb") as outgoing:
                    target.chmod(0o600)
                    shutil.copyfileobj(incoming, outgoing, 65536)
            else:
                raise RuntimeFailure("unsafe_archive_member")


def _failure(error, artifacts=None):
    result = {"ok": False, "complete": False, "verified": False, "error": str(error) if isinstance(error, RuntimeFailure) else type(error).__name__}
    if artifacts is not None:
        result["artifacts"] = str(artifacts)
    return result



def _npm_command(npm):
    if os.name != "nt":
        return [npm, "ci", "--ignore-scripts"]
    # A .cmd shim requires a shell. Use npm's own JS entrypoint instead.
    cli = Path(npm).parent / "node_modules/npm/bin/npm-cli.js"
    if not cli.is_file():
        raise RuntimeFailure("npm_cli_not_found_beside_windows_shim")
    return [shutil.which("node"), str(cli), "ci", "--ignore-scripts"]

def setup(home=None, typescript=False, cancellation=None):
    artifacts = None
    lock = None
    lock_path = None
    try:
        _check(cancellation)
        current = status(home)
        if current["prepared"] and (not typescript or current["typescript_prepared"]):
            return {"ok": True, **current}
        root = _root(home)
        _private_dir(root)
        lock_path = root / "setup.lock"
        try:
            lock = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        except FileExistsError:
            raise RuntimeFailure("setup_already_in_progress_or_stale_lock") from None
        destination = root / COMMIT
        source_ready = False
        if destination.exists() or destination.is_symlink():
            try:
                source_ready = not destination.is_symlink() and _json(destination / "octet-source.json") == {
                    "commit": COMMIT, "sha256": ARCHIVE_SHA256}
            except (OSError, ValueError, RuntimeFailure):
                pass
            if not current["prepared"] and not source_ready:
                raise RuntimeFailure("existing_runtime_not_overwritten_use_fresh_home")
        uv = shutil.which("uv")
        npm = shutil.which("npm.cmd" if os.name == "nt" else "npm") if typescript else None
        if not uv or (typescript and (not npm or not shutil.which("node"))):
            raise RuntimeFailure("missing_uv_or_node_npm")
        artifacts = Path(tempfile.mkdtemp(prefix="setup-", dir=root))
        if not current["prepared"] and not source_ready:
            archive = artifacts / "source.tar.gz"
            _download(archive, cancellation)
            # Reserve exclusively: uv environments are not relocatable.
            destination.mkdir(mode=0o700, exist_ok=False)
            try:
                _extract(archive, destination, cancellation)
                with (destination / "octet-source.json").open("x") as handle:
                    os.chmod(handle.name, 0o600)
                    json.dump({"commit": COMMIT, "sha256": ARCHIVE_SHA256}, handle)
            except Exception:
                # Only this attempt's newly created incomplete source is removed.
                # Diagnostics/archive stay in artifacts; dependency failures below
                # retain source and may be retried by an explicit setup call.
                shutil.rmtree(destination)
                raise
        env = _environment()
        env["UV_CACHE_DIR"] = str(artifacts / "uv-cache")
        for name, command in ([] if current["prepared"] else [("python", [uv, "sync", "--frozen", "--python", "3.12"])]) + (
                [("typescript", _npm_command(npm))] if typescript else []):
            logs = artifacts / name
            _private_dir(logs)
            env["npm_config_cache"] = str(artifacts / "npm-cache")
            if _process(command, cwd=destination, env=env, artifacts=logs, timeout=900, cancellation=cancellation):
                raise RuntimeFailure("dependency_setup_failed")
        marker = destination / "octet-runtime.json"
        with tempfile.NamedTemporaryFile(mode="w", dir=destination, delete=False) as handle:
            json.dump({"commit": COMMIT, "sha256": ARCHIVE_SHA256, "typescript": bool(typescript)}, handle)
        os.replace(handle.name, marker)
        return {"ok": True, **status(home), "artifacts": str(artifacts)}
    except (OSError, ValueError, tarfile.TarError, subprocess.SubprocessError, RuntimeFailure) as error:
        return _failure(error, artifacts)
    finally:
        if lock is not None:
            os.close(lock)
            lock_path.unlink()


def _ready(home, typescript):
    state = status(home)
    if not state["prepared"] or (typescript and not state["typescript_prepared"]):
        raise RuntimeFailure("explicit_setup_required")
    return Path(state["runtime"])


def _key_env(home, live, api_key):
    env = _environment()
    if live:
        key = api_key or jev.resolve_key(home=Path(home) / ".octet" if home is not None else None)
        if not key:
            raise RuntimeFailure("api_key_required")
        env[jev.API_KEY_ENV] = key
    return env


def _check_summary(summary, live, typescript, visual_fixture, require_visual_path, expect_visual_status):
    expected = {(language, provider) for language in (["python", "typescript"] if typescript else ["python"])
                for provider in (["mock", "live"] if live else ["mock"])}
    if not isinstance(summary, dict) or summary.get("complete") is not True:
        return False
    checks = summary.get("checks")
    if not isinstance(checks, list) or len(checks) != len(expected):
        return False
    fallback = visual_fixture and expect_visual_status not in (None, "ok")
    for check in checks:
        if not isinstance(check, dict):
            return False
        pair = (check.get("language"), check.get("provider"))
        if pair not in expected:
            return False
        expected.remove(pair)
        token = f"jev-guide-{pair[1]}"
        if check.get("token") != token:
            return False
        if fallback:
            if (check.get("observed") != {"submitted": None} or check.get("submit_tool") is not None
                    or check.get("outcome") not in {"refuted", "unknown", "abstained", "budget_exhausted"}):
                return False
        elif check.get("outcome") != "verified" or check.get("observed") != {"submitted": token}:
            return False
        if require_visual_path and (check.get("acted_path") != "visual" or check.get("submit_tool") != "click"):
            return False
        if expect_visual_status is not None:
            statuses = check.get("visual_statuses")
            if not isinstance(statuses, list):
                return False
            attempts = [s for s in statuses if s != "skipped"]
            if not attempts or any(s != expect_visual_status for s in attempts):
                return False
    return True


def run(home=None, *, driver_binary: Path, live=False, typescript=False, visual_fixture=False,
        require_visual_path=False, visual_observation="auto", max_steps=4, cancellation=None,
        api_key=None, expect_visual_status=None, port=0):
    artifacts = None
    try:
        _check(cancellation)
        if visual_observation not in ("auto", "always", "off"):
            raise RuntimeFailure("invalid_visual_observation")
        if type(port) is not int or not 0 <= port <= 65535:
            raise RuntimeFailure("invalid_port")
        if type(max_steps) is not int or not 1 <= max_steps <= 32:
            raise RuntimeFailure("max_steps_must_be_1_to_32")
        if expect_visual_status not in (None, "ok", "not_installed", "error", "unavailable"):
            raise RuntimeFailure("invalid_visual_status")
        if require_visual_path and expect_visual_status not in (None, "ok"):
            raise RuntimeFailure("visual_path_requires_ok_status")
        runtime = _ready(home, typescript)
        binary = Path(driver_binary).absolute()
        if not binary.is_file():
            raise RuntimeFailure("driver_binary_missing")
        env = _key_env(home, live, api_key)
        env["CUA_DRIVER_BIN"] = str(binary)
        artifacts = Path(tempfile.mkdtemp(prefix="proof-", dir=_root(home)))
        proof = artifacts / "upstream"
        command = [str(_python(runtime)), str(Path(__file__).with_name("jev_use_verifier.py")), "--visual-observation", visual_observation, "--output-dir", str(proof), "--max-steps", str(max_steps), "--port", str(port)]
        for enabled, flag in ((live, "--live"), (typescript, "--typescript"), (visual_fixture, "--visual-fixture"), (require_visual_path, "--require-visual-path")):
            if enabled:
                command.append(flag)
        if expect_visual_status:
            command += ["--expect-visual-status", expect_visual_status]
        code = _process(command, cwd=runtime, env=env, artifacts=artifacts,
                        timeout=780, cancellation=cancellation)
        summary = _json(proof / "summary.json")
        complete = code == 0 and _check_summary(summary, live, typescript, visual_fixture, require_visual_path, expect_visual_status)
        fallback = visual_fixture and expect_visual_status not in (None, "ok")
        return {"ok": complete, "complete": complete, "verified": complete and not fallback, "fallback_verified": complete and fallback,
                "live": bool(live), "checks": [{key: check[key] for key in
                    ("language", "provider", "outcome", "token", "observed", "acted_path", "submit_tool", "visual_statuses")
                    if key in check} for check in summary["checks"]] if complete else [], "artifacts": str(artifacts), "summary_path": str(proof / "summary.json"),
                "error": None if complete else "proof_incomplete_or_readback_mismatch"}
    except (OSError, ValueError, TypeError, subprocess.SubprocessError, RuntimeFailure) as error:
        return _failure(error, artifacts)


def choose(request, home=None, mock=False, typescript=False, cancellation=None, api_key=None):
    """Invoke the unchanged upstream validating CLI; never executes an action."""
    artifacts = None
    try:
        _check(cancellation)
        encoded = json.dumps(request, allow_nan=False).encode()
        if len(encoded) > 65536:
            raise RuntimeFailure("request_limit")
        runtime = _ready(home, typescript)
        env = _key_env(home, not mock, api_key)
        artifacts = Path(tempfile.mkdtemp(prefix="choice-", dir=_root(home)))
        command = ([shutil.which("node"), "--import", "tsx", "typescript/choose_action.ts"] if typescript
                   else [str(_python(runtime)), "python/choose_action.py"])
        if mock:
            command.append("--mock")
        code = _process(command, cwd=runtime, env=env, artifacts=artifacts, timeout=180,
                        cancellation=cancellation, stdin=encoded)
        if code:
            raise RuntimeFailure("chooser_failed")
        response = _json(artifacts / "stdout.log", 65536)
        allowed = {item["id"] for item in request["candidates"]}
        def probability(value):
            return type(value) in (int, float) and math.isfinite(value) and 0 <= value <= 1
        if (not isinstance(response, dict) or set(response) != {"schema", "selected_id", "model", "confidence", "probabilities"}
                or response["schema"] != "cua.jev_choice_v1" or response["selected_id"] not in allowed
                or not probability(response["confidence"]) or not isinstance(response["probabilities"], dict)
                or not set(response["probabilities"]).issubset(allowed)
                or not all(probability(p) for p in response["probabilities"].values())
                or (response["model"] is not None and (not isinstance(response["model"], str) or len(response["model"]) > 256))):
            raise RuntimeFailure("invalid_choice_response")
        return {"ok": True, "verified": False, "choice": response, "artifacts": str(artifacts)}
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError, RuntimeFailure) as error:
        return _failure(error, artifacts)
