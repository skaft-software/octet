#!/usr/bin/env python3
"""Offline Unix forced-reload acceptance against an actual executable.

No build, installation, credentials or inference. Uses disposable HOME/workspace,
loopback port 9, bounded PTY captures and only processes created by this script.
Checks unchanged-image restart, atomic replacement, symlink retarget and retry
following a failed candidate probe, preserving the exact session and terminal.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import struct
import subprocess
import tempfile
import termios
import time

CAPTURE_LIMIT = 4 * 1024 * 1024


def fixture(root):
    home, workspace = root / "home", root / "workspace"
    credentials = home / ".octet/credentials"
    credentials.mkdir(parents=True)
    workspace.mkdir()
    (home / ".octet/config.toml").write_text(
        'model = "custom/Probe"\ntheme = "dark"\nreload = false\ncache_warming = "off"\n'
    )
    path = credentials / "custom.json"
    path.write_text(json.dumps({
        "base_url": "http://127.0.0.1:9/v1/", "api_key": "", "api_name": "probe",
        "headers": [], "auto_discover": False,
        "models": [{"api_name": "probe", "display_name": "Probe",
                    "context_window": 128000, "max_output_tokens": 4096}],
    }))
    path.chmod(0o600)
    return home, workspace


def trial(binary, replacement_binary, case, output):
    root = Path(tempfile.mkdtemp(prefix=f"{case}-", dir=output)).resolve()
    home, workspace = fixture(root)
    launch, old, replacement = [root / name for name in ("octet", "old-octet", "new-octet")]
    if case == "symlink":
        shutil.copy2(binary, old)
        launch.symlink_to(old)
    else:
        shutil.copy2(binary, launch)
    marker = root / "replacement-argv.txt"
    # The wrapper delegates the real compatibility probe to the exact supplied
    # replacement executable. On entry it records argv, then enters that image.
    replacement.write_text(
        "#!/bin/sh\n"
        'if [ "$1" != "--internal-reexec-probe" ]; then\n'
        f"  printf '%s\\n' \"$@\" > {shlex.quote(str(marker))}\n"
        "fi\n"
        f"exec {shlex.quote(str(replacement_binary))} \"$@\"\n"
    )
    replacement.chmod(0o755)
    env = {"HOME": str(home), "TMPDIR": str(root), "PATH": "/usr/bin:/bin",
           "TERM": "xterm-256color", "LANG": "en_US.UTF-8",
           "OCTET_COLOR_SCHEME": "dark", "OCTET_STARTUP_TRACE": "1"}
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 42, 160, 0, 0))
    original_termios = termios.tcgetattr(master)
    capture = bytearray()

    def controlling_tty():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    with (root / "stderr.log").open("wb") as errors:
        child = subprocess.Popen(
            [str(launch), "--offline", "--no-context-files", "--no-tools", "--no-extensions",
             "--session-dir", str(root / "sessions"), "--workspace", str(workspace)],
            cwd=workspace, env=env, stdin=slave, stdout=slave, stderr=errors,
            preexec_fn=controlling_tty,
        )
        os.close(slave)

        def read_once():
            if select.select([master], [], [], 0.025)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    return
                capture.extend(data)
                if b"\x1b[6n" in data:
                    os.write(master, b"\x1b[1;1R")
                if len(capture) > CAPTURE_LIMIT:
                    raise AssertionError("PTY capture limit exceeded")

        def wait_for(predicate, stage, timeout=15):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if predicate():
                    return
                if child.poll() is not None:
                    raise AssertionError(f"{stage}: early exit {child.returncode}; see {root}")
                read_once()
            raise AssertionError(f"{stage}: timed out; see {root}")

        def boot_count():
            return (root / "stderr.log").read_bytes().count(b"bootstrap.ready")

        try:
            wait_for(lambda: b"Probe" in capture and b"\x1b[?2026l" in capture, "startup")
            os.write(master, b"READY_MARKER")
            wait_for(lambda: b"READY_MARKER" in capture, "initial editable prompt", 3)
            os.write(master, b"\x15")
            sessions = {path: path.read_bytes() for path in (root / "sessions").rglob("*.jsonl")}
            assert len(sessions) == 1, sessions
            session_path = next(iter(sessions))
            if case == "retry":
                invalid = root / "invalid-octet"
                invalid.write_text("#!/bin/sh\nexit 1\n")
                invalid.chmod(0o755)
                os.replace(invalid, launch)
                start = len(capture)
                os.write(master, b"/reload --force\r")
                wait_for(lambda: b"reload blocked" in capture[start:], "failed probe refusal")
                assert boot_count() == 1, "failed probe restarted the application"
                # A refusal notice precedes the in-process fallback. Wait for
                # its completion frame before retrying, not while lifecycle
                # ownership deliberately retains Enter as an editable draft.
                def refusal_complete():
                    frame = capture[start:]
                    position = frame.find(b"reload pass #0:")
                    return position >= 0 and b"\x1b[?2026l" in frame[position:]
                wait_for(refusal_complete, "refused pass completion")
            if case == "symlink":
                link = root / "updated-link"
                link.symlink_to(replacement)
                os.replace(link, launch)
            elif case != "unchanged":
                os.replace(replacement, launch)
            os.write(master, b"/reload --force\r")
            wait_for(lambda: boot_count() >= 2, "forced app restart")
            assert boot_count() == 2, "forced reload booted more than once"
            if case != "unchanged":
                assert marker.exists(), "the replacement image was not entered"
                argv = marker.read_text().splitlines()
                assert argv.count("--resume") == 1, argv
                assert argv[-2:] == ["--resume", session_path.stem], argv
            assert set((root / "sessions").rglob("*.jsonl")) == set(sessions)
            assert session_path.read_bytes().startswith(sessions[session_path]), "session was truncated"
            if binary == replacement_binary:
                assert b"applying an explicit pass" not in capture, "generic forced-reload chatter"
            start = len(capture)
            os.write(master, b"AFTER_RELOAD_MARKER")
            wait_for(lambda: b"AFTER_RELOAD_MARKER" in capture[start:], "resumed editable prompt", 5)
            os.write(master, b"\x04")
            deadline = time.monotonic() + 10
            while child.poll() is None and time.monotonic() < deadline:
                read_once()
            child.wait(timeout=2)
            assert child.returncode == 0, child.returncode
            assert termios.tcgetattr(master) == original_termios, "terminal modes were not restored"
            return {"case": case, "passed": True, "artifacts": str(root),
                    "same_pid": True, "session": session_path.stem, "boots": boot_count()}
        finally:
            (root / "capture.pty").write_bytes(capture)
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
            os.close(master)
            # The byte-for-byte source digest in the report identifies the
            # tested build; do not retain an executable copy for every case.
            launch.unlink(missing_ok=True)
            old.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--replacement-binary", type=Path,
                        help="replacement image for atomic, symlink and retry cases; defaults to --binary")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--cases", nargs="+", choices=["unchanged", "atomic", "symlink", "retry"],
                        default=["unchanged", "atomic", "symlink", "retry"])
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    replacement_binary = args.replacement_binary.resolve(strict=True) if args.replacement_binary else binary
    output = args.output or Path(tempfile.mkdtemp(prefix="octet-reload-pty-"))
    output.mkdir(parents=True, exist_ok=True)
    report = {"binary": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "replacement_binary": str(replacement_binary),
              "replacement_sha256": hashlib.sha256(replacement_binary.read_bytes()).hexdigest(),
              "cases": [], "not_qualified": ["Windows", "live providers", "attended desktop"]}
    try:
        for case in args.cases:
            result = trial(binary, replacement_binary, case, output)
            report["cases"].append(result)
            print(json.dumps(result), flush=True)
    finally:
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
