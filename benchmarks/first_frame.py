#!/usr/bin/env python3
"""Launch-to-first-frame latency on a pseudo-terminal.

Each sample forks the binary on a fresh 200x50 PTY with an empty HOME, answers
the terminal queries a fast emulator answers (cursor position, background
colour, device attributes, keyboard and mode queries), and records the time
from fork to the first output byte and to the first appearance of MARKER, the
last text drawn in that agent's first frame. The process is then killed.
Binaries are interleaved round-robin after unrecorded warmups so every
candidate sees the same machine load. This mirrors fx's
benchmarks/first_frame.py, extended to any binary and marker.

Usage:
  benchmarks/first_frame.py --runs 100 \\
      --binary target/release/octet "esc close" \\
      --binary /path/to/fx "esc to set up later" --env FX_AUTO_UPGRADE=0
"""

import argparse
import fcntl
import os
import pty
import re
import select
import shutil
import signal
import statistics
import struct
import tempfile
import termios
import time

REPLIES = [
    (re.compile(rb"\x1b\[6n"), b"\x1b[1;1R"),
    (re.compile(rb"\x1b\]11;\?(\x07|\x1b\\)"), b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
    (re.compile(rb"\x1b\]10;\?(\x07|\x1b\\)"), b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
    (re.compile(rb"\x1b\[\?996n"), b"\x1b[?997;1n"),
    (re.compile(rb"\x1b\[\?u"), b"\x1b[?0u"),
    (re.compile(rb"\x1b\[0?c"), b"\x1b[?62;22c"),
    (re.compile(rb"\x1b\[18t"), b"\x1b[8;50;200t"),
]
DECRQM = re.compile(rb"\x1b\[\?(\d+)\$p")


def sample(binary, marker, extra_env, timeout=10.0):
    home = tempfile.mkdtemp(prefix="first-frame-")
    started = time.perf_counter()
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(home)
        os.execve(binary, [binary], {"PATH": os.environ.get("PATH", ""), "HOME": home,
                                     "TERM": "xterm-256color", **extra_env})
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 200, 0, 0))
    seen, first_byte, first_frame = b"", None, None
    try:
        while first_frame is None and time.perf_counter() - started < timeout:
            ready, _, _ = select.select([fd], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            now = time.perf_counter()
            first_byte = first_byte or now
            reply = b"".join(answer for pattern, answer in REPLIES if pattern.search(data))
            reply += b"".join(b"\x1b[?" + m.group(1) + b";2$y" for m in DECRQM.finditer(data))
            if reply:
                os.write(fd, reply)
            seen = (seen + data)[-1_000_000:]
            if marker in re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", seen):
                first_frame = now
    finally:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
        os.close(fd)
        shutil.rmtree(home, ignore_errors=True)
    if first_frame is None:
        raise SystemExit(f"{binary}: marker {marker!r} not seen within {timeout}s")
    return (first_byte - started) * 1000, (first_frame - started) * 1000


def summary(values):
    ordered = sorted(values)
    p95 = ordered[min(len(ordered) - 1, round(0.95 * (len(ordered) - 1)))]
    return (f"{statistics.median(ordered):8.2f}{statistics.mean(ordered):8.2f}"
            f"{statistics.stdev(ordered):8.2f}{p95:8.2f}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", nargs=2, action="append", required=True, metavar=("PATH", "MARKER"))
    parser.add_argument("--env", action="append", default=[], help="extra KEY=VALUE for every binary")
    parser.add_argument("--runs", type=int, default=100)
    parser.add_argument("--warmup", type=int, default=10)
    args = parser.parse_args()
    extra = dict(item.split("=", 1) for item in args.env)
    candidates = [(os.path.abspath(path), marker.encode()) for path, marker in args.binary]
    results = {path: ([], []) for path, _ in candidates}
    for run in range(args.warmup + args.runs):
        for path, marker in candidates:
            byte_ms, frame_ms = sample(path, marker, extra)
            if run >= args.warmup:
                results[path][0].append(byte_ms)
                results[path][1].append(frame_ms)
    print(f"{'binary':<40}{'':>2}{'median':>8}{'mean':>8}{'stddev':>8}{'p95':>8}")
    for path, (bytes_, frames) in results.items():
        print(f"{path[-40:]:<40}  first byte {summary(bytes_)}")
        print(f"{'':<40}  first frame{summary(frames)}")
    print(f"milliseconds from fork to output, {args.runs} runs after {args.warmup} warmups")


if __name__ == "__main__":
    main()
