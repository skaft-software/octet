#!/usr/bin/env python3
"""Provider-free Linux smoke test against an explicitly supplied octet binary.

The fixture uses an isolated home, a dummy offline model, and a PATH without
PipeWire executables. It must stop before opening audio or resolving credentials.
The supplied Python 3.14 venv must already contain the locked dependencies.
"""
from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parent.parent


def run(binary: Path, venv: Path, archive: Path | None = None) -> None:
    binary, venv = binary.resolve(strict=True), venv.resolve(strict=True)
    if archive is not None:
        archive = archive.resolve(strict=True)
    if sys.platform != "linux" or not (venv / "bin/python").is_file():
        raise ValueError("this smoke test requires Linux and a provisioned voice venv")

    with tempfile.TemporaryDirectory(prefix="octet-voice-host-smoke-") as directory:
        root = Path(directory)
        home, workspace, sessions, bin_dir = (root / n for n in ("home", "workspace", "sessions", "bin"))
        for path in (home / ".octet/credentials", home / ".local/share/octet-voice", workspace, sessions, bin_dir):
            path.mkdir(parents=True)
        (home / ".local/share/octet-voice/venv").symlink_to(venv)
        (bin_dir / "python3").symlink_to(Path(sys.executable).resolve())
        if any(shutil.which(name, path=str(bin_dir)) for name in ("pw-record", "pw-play")):
            raise RuntimeError("smoke-test PATH must not contain PipeWire audio executables")

        credential = home / ".octet/credentials/custom.json"
        credential.write_text(json.dumps({
            "base_url": "http://127.0.0.1:9/v1/", "api_key": "", "api_name": "probe",
            "headers": [], "models": [{"api_name": "probe", "context_window": 32768}],
            "auto_discover": False,
        }), encoding="utf-8")
        credential.chmod(0o600)
        if archive is None:
            archive = root / "octet-voice-0.10.0.tar.gz"
            subprocess.run([sys.executable, str(ROOT / "build-package.py"), str(root / "stage"),
                            "--archive", str(archive)], check=True, timeout=30)
        env = {"HOME": str(home), "PATH": str(bin_dir), "TERM": "xterm-256color",
               "LANG": "C.UTF-8", "OCTET_COLOR_SCHEME": "dark", "OCTET_EXTENSIONS": "octet-voice"}
        installed = subprocess.run([str(binary), "extension", "install", "--path", str(archive)],
                                   env=env, cwd=workspace, text=True, capture_output=True, timeout=30)
        if installed.returncode:
            raise RuntimeError(f"isolated archive installation failed: {installed.stderr}")
        if not (home / ".octet/extensions/octet-voice/extension.toml").is_file():
            raise RuntimeError("installed extension manifest was not found")
        if (home / ".octet/config.toml").exists():
            raise RuntimeError("installation unexpectedly wrote persistent enablement config")

        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        os.set_blocking(master, False)

        def child_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        args = [str(binary), "--offline", "--no-context-files", "--no-tools", "--model", "custom/probe",
                "--theme", "dark", "--tui-mode", "inline", "--workspace", str(workspace),
                "--session-dir", str(sessions)]
        try:
            process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave, cwd=workspace,
                                       env=env, preexec_fn=child_terminal, close_fds=True)
        except BaseException:
            os.close(master)
            raise
        finally:
            os.close(slave)
        output = bytearray()

        def until(needles, label, timeout=30, refresh=None):
            deadline, last_refresh = time.monotonic() + timeout, 0
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        output.extend(os.read(master, 65536))
                    except OSError:
                        break
                if any(needle in output for needle in needles):
                    return
                if process.poll() is not None:
                    break
                if refresh and time.monotonic() - last_refresh >= 0.1:
                    os.write(master, refresh)
                    last_refresh = time.monotonic()
            raise AssertionError(f"{label} not observed; exit={process.poll()}; output={bytes(output)[-6000:]!r}")

        try:
            until((b"custom/probe",), "offline model")
            output.clear()
            os.write(master, b"/voic")
            until((b"/voice",), "registered voice command", refresh=b"\x7fc")
            os.write(master, b"e status\r")
            until((b'"state": "stopped"',), "stopped voice status")
            print("PASS: isolated archive installed and explicitly enabled; /voice status is stopped")
            output.clear()
            os.write(master, b"\x1b[27u")
            until((b"custom/probe",), "composer after status")
            output.clear()
            os.write(master, b"/voice start\r")
            until((b"Install PipeWire's pw-record and pw-play executables",
                   b"PipeWire runtime directory unavailable"), "missing-audio prerequisite")
            if b"did not offer voice" in output:
                raise AssertionError("host did not negotiate required voice features")
            print("PASS: voice features negotiated and remote UI opened; audio prerequisite stopped start")
            os.write(master, b"\x1b[27u\x04")
            process.wait(timeout=5)
            if process.returncode:
                raise RuntimeError(f"octet exited with status {process.returncode}")
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
            os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True, type=Path, help="explicit octet binary")
    parser.add_argument("--venv", required=True, type=Path, help="preprovisioned voice Python 3.14 venv")
    parser.add_argument("--archive", type=Path, help="reviewed voice bundle to test instead of building one")
    args = parser.parse_args()
    run(args.host, args.venv, args.archive)


if __name__ == "__main__":
    main()
