#!/usr/bin/env python3
"""Offline fullscreen PTY bursts; reports native admission -> publication -> write.

Use a debug or release binary. No inference is submitted. Physical display and kernel/input
queue time are not measured by the in-process trace. Raw samples are retained.
"""
import argparse
import base64
import collections
import fcntl
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time
import zlib

spec = importlib.util.spec_from_file_location("startup", Path(__file__).with_name("bench-startup-resume.py"))
startup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(startup)
TRACE = re.compile(r"octet-latency: kind=(\w+) sequence=(\d+) updated_us=(\d+) painted_us=(\d+) dropped=(\d+)")


def summarize(samples):
    result = {}
    for kind in sorted({s["kind"] for s in samples}):
        group = [s for s in samples if s["kind"] == kind]
        result[kind] = {"count": len(group)}
        for phase in ("updated_us", "painted_us"):
            values = sorted(s[phase] / 1000 for s in group)
            result[kind][phase.replace("_us", "_ms")] = {
                f"p{percent}": values[math.ceil(len(values) * percent / 100) - 1]
                for percent in (50, 95)
            }
    return result


def mixed_fixture(root, turns):
    """Durable tool cards and inline PNGs, without providers or external assets."""
    home, workspace, sessions = startup.fixture(root, 100, turns)

    def chunk(kind, data):
        return struct.pack("!I", len(data)) + kind + data + struct.pack("!I", zlib.crc32(kind + data))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack("!2I5B", 64, 128, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress((b"\0" + b"\x50\x90\xd0" * 64) * 128))
           + chunk(b"IEND", b""))
    path = next(sessions.glob("*/*.jsonl"))
    messages = [json.loads(line)["value"] for line in path.read_text().splitlines()[:-1]]
    entries = []

    def append(value, metadata=None):
        record = dict(type="entry", id=str(len(entries)),
                      parent=str(len(entries) - 1) if entries else None, value=value)
        if metadata:
            record["metadata"] = metadata
        entries.append(record)

    for turn in range(turns):
        append(messages[2 * turn], {"prompt_model": startup.MODEL, "prompt_color": "#5a36d6"})
        call = f"tool-{turn}"
        append({"type": "message", "Assistant": {
            "model": startup.MODEL, "protocol": "open_ai_chat",
            "content": [{"ToolCall": {"id": call, "name": "read", "arguments_json":
                         json.dumps({"path": f"fixture-{turn}.txt"})}}]}})
        content = [{"Text": "tool output line\n" * 12}]
        if turn % 10 == 9:
            content.append({"Media": {"Image": {"source": {"Inline": base64.b64encode(png).decode()},
                                               "media_type": "image/png", "detail": None}}})
        append({"type": "message", "User": {"content": [{"ToolResult": {
            "tool_call_id": call, "tool_name": "read", "content": content, "is_error": False}}]}})
        append(messages[2 * turn + 1])
    path.write_text("".join(json.dumps(record) + "\n" for record in entries)
                    + json.dumps({"type": "head", "id": entries[-1]["id"], "total_cost_microdollars": 0}) + "\n")
    return home, workspace, sessions


def scroll_hint(output):
    plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output)
    matches = re.findall(rb"(\d+) rows back", plain)
    return int(matches[-1]) if matches else None


def run(binary, turns, rounds):
    with tempfile.TemporaryDirectory(prefix="octet-interaction-") as tmp:
        home, workspace, sessions = mixed_fixture(Path(tmp), turns)
        # Explicit context metadata makes the synthetic catalog ready offline.
        catalog = home / ".octet/credentials/custom.json"
        data = json.loads(catalog.read_text())
        for model in data["providers"]["bench"]["models"]:
            model["context_window"] = 32768
        catalog.write_text(json.dumps(data))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        original = termios.tcgetattr(master)
        env = startup.environment(home)
        env["OCTET_STARTUP_TRACE"] = "latency"
        env["TERM"] = "xterm-ghostty"  # exercise Kitty-capable image-bearing frames
        env["TERM_PROGRAM"] = "ghostty"
        args = [str(binary), "--offline", "--no-context-files", "--no-tools", "--color", "always",
                "--mouse", "app", "--show-images", "--workspace", str(workspace), "--session-dir", str(sessions), "--continue"]

        def controlling_tty():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        with tempfile.TemporaryFile() as errors:
            child = subprocess.Popen(args, cwd=workspace, env=env, stdin=slave, stdout=slave,
                                     stderr=errors, preexec_fn=controlling_tty)
            os.close(slave)
            tail = bytearray()
            motion_output = bytearray()
            measuring_motion = False
            kitty_commands = 0

            def drain(duration):
                nonlocal kitty_commands
                end = time.monotonic() + duration
                while time.monotonic() < end:
                    if child.poll() is not None:
                        raise AssertionError(f"unexpected exit {child.returncode}")
                    if select.select([master], [], [], max(0, end - time.monotonic()))[0]:
                        chunk = os.read(master, 65536)
                        kitty_commands += chunk.count(b"\x1b_G")
                        tail.extend(chunk)
                        if measuring_motion:
                            motion_output.extend(chunk)
                        if b"\x1b[6n" in tail:
                            os.write(master, b"\x1b[1;1R")
                            tail.clear()
                        del tail[:-8192]

            expected = collections.Counter()

            def send(data, kind=None, count=1, delay=0.004):
                os.write(master, data)
                if kind:
                    expected[kind] += count
                drain(delay)

            try:
                deadline = time.monotonic() + 60
                while b"model-00000" not in tail or startup.END not in tail:
                    if time.monotonic() >= deadline:
                        errors.seek(0)
                        raise TimeoutError(errors.read().decode(errors="replace") + repr(tail))
                    drain(0.05)
                if termios.tcgetattr(master)[3] & (termios.ICANON | termios.ECHO):
                    raise AssertionError("terminal echo is enabled")
                drain(0.2)
                # Row accounting is separate from latency bursts: allow each probe
                # to settle, and read the native hint rather than infer motion from traces.
                motion = []
                measuring_motion = True
                for count, delay in [(1, .05), (64, 0), (64, .001), (64, .004)]:
                    motion_output.clear()
                    before = motion[-1]["scroll_after"] if motion else 0
                    for _ in range(count):
                        send(b"\x1b[<64;20;12M", "wheel", delay=delay)
                    drain(.3)
                    after = scroll_hint(motion_output)
                    if after is None:
                        raise AssertionError("scroll probe produced no hint")
                    motion.append(dict(events=count, gap_ms=delay * 1000, scroll_after=after,
                                       rows_moved=after - before, rows_per_event=(after - before) / count))
                measuring_motion = False
                # Return to tail without submitting any input.
                send(b"\x1b[F", "key", delay=.2)
                for _ in range(rounds):
                    # Burst input has a fixed schedule, not a per-frame acknowledgement.
                    for _ in range(8):
                        send(b"\x1b[<64;20;12M", "wheel")
                    for _ in range(8):
                        send(b"\x1b[<65;20;12M", "wheel")
                    send(b"\x1b[<0;10;10M", "selection")
                    for column in range(12, 24, 2):
                        send(f"\x1b[<32;{column};10M".encode(), "selection")
                    send(b"\x1b[<0;24;10m", "selection")
                    for byte in b"/model":
                        send(bytes([byte]), "typing")
                    for _ in range(4):
                        send(b"\x1b[B", "arrow")
                        send(b"\x1b[A", "arrow")
                    for _ in range(6):
                        send(b"\x7f", "typing")
                    # Composer/history arrows as well as popup navigation.
                    send(b"draft", "typing", 5)
                    for _ in range(4):
                        send(b"\x1b[D", "arrow")
                        send(b"\x1b[C", "arrow")
                    send(b"\x15", "key")  # clear line, no command submission
                    drain(0.03)
                send(b"LATENCY_DONE", "typing", 12, delay=1)
                if b"LATENCY_DONE" not in tail:
                    raise AssertionError("final composer marker missing")
                os.write(master, b"\x04")
                startup.drain_until_exit(child, master)
                if child.returncode != 0 or termios.tcgetattr(master) != original:
                    raise AssertionError("shutdown or terminal restoration failed")
                errors.seek(0)
                samples = [dict(kind=k, sequence=int(s), updated_us=int(u), painted_us=int(p), dropped=int(d))
                           for k, s, u, p, d in TRACE.findall(errors.read().decode())]
                observed = collections.Counter(s["kind"] for s in samples)
                if observed != expected:
                    raise AssertionError(f"missing/extra traces: {observed} != {expected}")
                if any(s["dropped"] or s["updated_us"] > s["painted_us"] for s in samples):
                    raise AssertionError("trace overflow or invalid timing")
                if turns >= 10 and not kitty_commands:
                    raise AssertionError("image-bearing fixture emitted no Kitty image commands")
                return samples, motion, kitty_commands
            finally:
                if child.poll() is None:
                    child.kill()
                    startup.drain_until_exit(child, master)
                os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--turns", type=int, default=1000)
    parser.add_argument("--rounds", type=int, default=20)
    args = parser.parse_args()
    if args.turns < 1 or args.rounds < 1:
        parser.error("turns and rounds must be positive")
    binary = args.binary.resolve()
    samples, motion, kitty_commands = run(binary, args.turns, args.rounds)
    print(json.dumps({"schema": "octet.interaction-latency.v1", "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                      "system": platform.platform(), "turns": args.turns, "rounds": args.rounds,
                      "viewport": [120, 40], "fixture": "tools-and-png", "kitty_commands": kitty_commands, "motion": motion,
                      "summary": summarize(samples), "samples": samples}))


if __name__ == "__main__":
    main()
