#!/usr/bin/env python3
"""Matched offline PTY startup/resume measurements with disposable synthetic homes.

Unix only, Python standard library. No credentials, extensions, live providers,
or user sessions. Each cell discards one warmup, then alternates executable order.
Readiness ends at the synchronized frame fence, after complete native history.
"""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import pty
import re
import select
import statistics
import struct
import subprocess
import tempfile
import termios
import time

PHASE = re.compile(r"octet-startup: ([\w.]+) elapsed=(\d+)us")
END = b"\x1b[?2026l"
MODEL = "custom/bench/model-00000"


def fixture(root, models, turns):
    root = root.resolve()
    home, workspace, sessions = [root / name for name in ("home", "workspace", "sessions")]
    credentials = home / ".octet/credentials"
    credentials.mkdir(parents=True)
    credentials.chmod(0o700)
    workspace.mkdir()
    sessions.mkdir()
    (home / ".octet/config.toml").write_text(f'model = "{MODEL}"\ntheme = "dark"\n')
    provider = {
        "label": "Bench", "base_url": "http://127.0.0.1:9/v1/",
        "auth": {"kind": "none"}, "auto_discover": False,
        "models": [{"api_name": f"model-{i:05}"} for i in range(models)],
    }
    path = credentials / "custom.json"
    path.write_text(json.dumps({"version": 1, "providers": {"bench": provider}}))
    path.chmod(0o600)
    if turns:
        # Session-store namespace uses the physically resolved workspace path.
        key = 0xcbf29ce484222325
        for byte in str(workspace).encode():
            key = ((key ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
        directory = sessions / f"{key:012x}"
        directory.mkdir()
        transcript = directory / "synthetic.jsonl"
        with transcript.open("w") as stream:
            parent = None
            for i in range(turns * 2):
                if i % 2 == 0:
                    value = {"type": "message", "User": {"content": [{"Text": f"USER_{i:06} inspect this change"}]}}
                else:
                    text = (f"ANSWER_{i:06}\n\n## Analysis\n\n"
                            + "A **verified** result with `code` and a useful explanation. " * 12
                            + '\n\n```rust\nfn example() { println!("hello"); }\n```\n')
                    value = {"type": "message", "Assistant": {
                        "content": [{"Text": text}], "model": MODEL, "protocol": "open_ai_chat"}}
                record = {"type": "entry", "id": str(i), "parent": parent, "value": value}
                if i % 2 == 0:
                    record["metadata"] = {"prompt_model": MODEL, "prompt_color": "#5a36d6"}
                stream.write(json.dumps(record) + "\n")
                parent = str(i)
            stream.write(json.dumps({"type": "head", "id": parent, "total_cost_microdollars": 0}) + "\n")
        transcript.chmod(0o600)
    return home, workspace, sessions


def environment(home):
    # Do not inherit provider/auth, extension, config, or telemetry variables.
    return {"HOME": str(home), "PATH": "/usr/bin:/bin", "TERM": "xterm-256color",
            "COLORTERM": "truecolor", "LANG": "C.UTF-8", "OCTET_COLOR_SCHEME": "dark",
            "OCTET_STARTUP_TRACE": "1"}


def validate_history(output, turns):
    actual = re.findall(rb"(?:USER|ANSWER)_[0-9]{6,}", output)
    expected = [f"{'USER' if i % 2 == 0 else 'ANSWER'}_{i:06}".encode() for i in range(turns * 2)]
    if actual != expected:
        raise AssertionError("native history missing, duplicated, or reordered")


def drain_until_exit(child, master, timeout=3):
    deadline = time.monotonic() + timeout
    while child.poll() is None and time.monotonic() < deadline:
        if select.select([master], [], [], 0.01)[0]:
            try:
                os.read(master, 65536)
            except OSError:
                pass  # Unix PTYs may report EIO after their slave exits.
    child.wait(timeout=timeout)


def trial(binary, paths, turns, timeout):
    home, workspace, sessions = paths
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    original_termios = termios.tcgetattr(master)
    args = [str(binary), "--offline", "--no-context-files", "--no-tools", "--color", "always", "--mouse", "auto",
            "--workspace", str(workspace), "--session-dir", str(sessions)]
    if turns:
        args += ["--continue"]

    def controlling_tty():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    output = bytearray()
    ready = None
    scan_tail = b""
    marker_seen = False
    started = time.monotonic_ns()
    with tempfile.TemporaryFile() as errors:
        try:
            child = subprocess.Popen(args, cwd=workspace, env=environment(home), stdin=slave,
                                     stdout=slave, stderr=errors, preexec_fn=controlling_tty)
        except BaseException:
            os.close(master)
            raise
        finally:
            os.close(slave)
        try:
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    errors.seek(0)
                    raise RuntimeError(f"startup exited {child.returncode}: {errors.read().decode(errors='replace')}")
                if select.select([master], [], [], 0.01)[0]:
                    chunk = os.read(master, 65536)
                    output.extend(chunk)
                    # Scan only new bytes and a bounded overlap, not the growing
                    # history buffer: the observer must not introduce O(N²) work.
                    scan = scan_tail + chunk
                    query_offset = scan.rfind(b"\x1b[6n")
                    if query_offset >= 0 and query_offset + 4 > len(scan_tail):
                        os.write(master, b"\x1b[1;1R")
                    marker = scan.find(b"model-00000")
                    if marker >= 0:
                        marker_seen = True
                        scan = scan[marker:]
                    if marker_seen and END in scan:
                        ready = (time.monotonic_ns() - started) / 1e6
                        break
                    scan_tail = scan[-128:]
            if ready is None:
                errors.seek(0)
                raise TimeoutError(f"no ready frame: {errors.read().decode(errors='replace')}; tail={output[-400:]!r}")
            ready_native_bytes = len(output)
            validate_history(output, turns)
            if termios.tcgetattr(master)[3] & (termios.ICANON | termios.ECHO):
                raise AssertionError("composer edit could be terminal line-discipline echo")
            edit_started = time.monotonic_ns()
            os.write(master, b"EDITABLE_MARKER")
            deadline = time.monotonic() + 3
            edit_scan = b""
            while b"EDITABLE_MARKER" not in edit_scan and time.monotonic() < deadline:
                if select.select([master], [], [], 0.01)[0]:
                    edit_scan = edit_scan[-128:] + os.read(master, 65536)
            if b"EDITABLE_MARKER" not in edit_scan:
                raise AssertionError("composer did not present edit")
            edit_echo_ms = (time.monotonic_ns() - edit_started) / 1e6
            os.write(master, b"\x04")
            drain_until_exit(child, master)
            if child.returncode:
                raise AssertionError(f"shutdown exit {child.returncode}")
            if termios.tcgetattr(master) != original_termios:
                raise AssertionError("shutdown did not restore terminal modes")
            errors.seek(0)
            phases = {name: int(value) / 1000 for name, value in PHASE.findall(errors.read().decode())}
            return {"ready_frame_ms": ready, "edit_echo_ms": edit_echo_ms,
                    "phase_ms": phases, "ready_native_bytes": ready_native_bytes, "correctness_passed": True}
        finally:
            try:
                if child.poll() is None:
                    child.kill()
                    drain_until_exit(child, master)
            finally:
                os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--trials", type=int, default=7)
    parser.add_argument("--models", type=int, nargs="+", default=[1, 100, 1000])
    parser.add_argument("--turns", type=int, nargs="+", default=[0, 1000, 5000])
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    if (args.trials < 1 or any(n < 1 or n > 1024 for n in args.models)
            or any(n < 0 for n in args.turns) or not math.isfinite(args.timeout) or args.timeout <= 0):
        parser.error("require positive trials/timeout, 1..1024 models, and nonnegative turns")
    binaries = {"before": args.before.resolve(), "after": args.after.resolve()}
    results = {"schema": "octet.startup-resume.v1", "system": platform.platform(),
               "machine": platform.machine(), "python": platform.python_version(),
               "viewport": [120, 40], "mouse": "auto", "theme": "dark", "warmups": 1,
               "trials": args.trials, "sha256": {
                   key: hashlib.sha256(path.read_bytes()).hexdigest() for key, path in binaries.items()}, "cells": []}
    cases = [(n, 0) for n in args.models] + [(1, n) for n in args.turns if n]
    for cell, (models, turns) in enumerate(cases):
        with tempfile.TemporaryDirectory(prefix="octet-startup-resume-") as tmp:
            row = {"models": models, "turns": turns, "samples": {key: [] for key in binaries}}
            paths = {name: fixture(Path(tmp) / f"{cell}-{name}", models, turns) for name in binaries}
            for i in range(args.trials + 1):
                order = list(binaries) if i % 2 == 0 else list(reversed(binaries))
                for name in order:
                    value = trial(binaries[name], paths[name], turns, args.timeout)
                    if i:
                        row["samples"][name].append(value)
            row["median_ready_ms"] = {key: statistics.median(value["ready_frame_ms"] for value in samples)
                                      for key, samples in row["samples"].items()}
            results["cells"].append(row)
            print(json.dumps(row), flush=True)
    print(json.dumps(results), flush=True)


if __name__ == "__main__":
    main()
