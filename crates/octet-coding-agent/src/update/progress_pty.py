"""Offline subprocess/PTY assertions for the updater's production presentation.

Invoked by Rust's library tests against their synthetic subprocess entry point;
never calls a real update endpoint, installer, package manager, or installed octet.
"""
import fcntl
import os
import pathlib
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

binary = sys.argv[1]
entry = "update::progress::tests::progress_subprocess_fixture"
argv = [binary, "--exact", entry, "--nocapture"]


def environment(mode, term="xterm-256color", locale="C.UTF-8"):
    env = dict(os.environ, OCTET_TEST_UPDATE_UI=mode, TERM=term, LC_ALL=locale)
    return env


with tempfile.TemporaryDirectory(prefix="octet-update-ui-") as root:
    root = pathlib.Path(root)
    fake = root / "curl"
    fake.write_text("""#!/usr/bin/env python3
import os, pathlib, sys
assert sys.argv[sys.argv.index('--output') + 1] == '-'
mode = os.environ['OCTET_TEST_UPDATE_UI']
marker = pathlib.Path(os.environ['OCTET_TEST_UPDATE_MARKER'])
if mode == 'fetch-empty':
    sys.stdout.buffer.write(b'')
elif mode == 'fetch-large':
    sys.stdout.buffer.write(b'x' * 262145)
else:
    sys.stdout.write('test "$OCTET_UPDATE_PARENT_UI" = 1 || exit 3\\ntouch "' + str(marker) + '"\\n')
if mode == 'fetch-failed':
    raise SystemExit(22)
""")
    fake.chmod(0o700)
    for mode in ["fetch-failed", "fetch-empty", "fetch-large", "fetch-ok"]:
        marker = root / mode
        env = environment(mode)
        env["PATH"] = str(root) + os.pathsep + env["PATH"]
        env["OCTET_TEST_UPDATE_MARKER"] = str(marker)
        result = subprocess.run(argv, env=env, capture_output=True, timeout=5)
        assert result.returncode == 0, (mode, result.stdout, result.stderr)
        assert marker.exists() == (mode == "fetch-ok"), mode
        assert b"\x1b" not in result.stderr and b"\r" not in result.stderr

    for term in ["dumb", "xterm-256color", ""]:
        result = subprocess.run(argv, env=environment("command", term), capture_output=True, timeout=5)
        assert result.returncode == 0, (result.stdout, result.stderr)
        assert b"probe stdout\n" in result.stdout
        assert b"partial line\n" in result.stderr
        assert b"\x1b" not in result.stderr and b"\r" not in result.stderr
        assert b"%" not in result.stderr

    for term, locale, initial_width in [
        ("xterm-256color", "C.UTF-8", 80),
        ("xterm-256color", "C", 40),
        ("xterm-256color", "C.UTF-8", 12),
        ("dumb", "C", 80),
    ]:
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, initial_width, 0, 0))
        child = subprocess.Popen(argv, env=environment("command", term, locale),
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=slave,
                                 start_new_session=True)
        data = bytearray()
        resized_at = None
        deadline = time.monotonic() + 5
        try:
            while child.poll() is None:
                assert time.monotonic() < deadline, "updater progress subprocess stalled"
                if select.select([master], [], [], 0.03)[0]:
                    data.extend(os.read(master, 65536))
                if initial_width == 80 and term != "dumb" and resized_at is None and b"partial line" in data:
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 40, 0, 0))
                    resized_at = len(data)
            while select.select([master], [], [], 0.02)[0]:
                data.extend(os.read(master, 65536))
            output = child.communicate(timeout=1)[0]
            assert child.returncode == 0, (output, bytes(data))
            assert b"probe stdout\n" in output
            assert b"partial line\r\n" in data, "activity erased/interleaved a partial diagnostic"
            assert b"%" not in data, "unknown work acquired a false percentage"
            if term == "dumb":
                assert b"\x1b" not in data
            else:
                assert data.count(b"\r\x1b[J") >= 4, "activity did not remain responsive"
                assert b"\x1b[?25" not in data, "updater changed cursor visibility"
                if initial_width >= 40:
                    assert b"v0.7.5" in data
                if locale == "C":
                    assert bytes(data).isascii()
                    assert b"########" in data
                if resized_at is not None:
                    frames = bytes(data[resized_at:]).split(b"\r\x1b[J")[1:]
                    frames = [frame.split(b"\r")[0].split(b"\n")[0] for frame in frames]
                    assert any(b"Installing fixture" in frame for frame in frames), frames
                    assert all(len(frame.decode()) <= 39 for frame in frames), frames
            assert termios.tcgetattr(slave) == before
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=2)
            os.close(master)
            os.close(slave)

    for mode in ["orphan-pipe", "closed-pipes", "backpressure"]:
        master, slave = pty.openpty()
        child = subprocess.Popen(argv, env=environment(mode), stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=slave, start_new_session=True)
        started = time.monotonic()
        data = bytearray()
        try:
            while child.poll() is None and time.monotonic() - started < 1.5:
                if select.select([master], [], [], 0.03)[0]:
                    data.extend(os.read(master, 65536))
                if mode == "backpressure" and data.count(b"\r\x1b[J") >= 4:
                    break
            if mode == "backpressure":
                assert data.count(b"\r\x1b[J") >= 4, "stdout backpressure froze progress"
            else:
                assert child.poll() == 0, (mode, bytes(data))
                if mode == "closed-pipes":
                    assert time.monotonic() - started >= 0.4, "EOF claimed completion before child exit"
        finally:
            # The orphan-pipe fixture deliberately retains a descriptor in an
            # owned descendant; terminate the entire synthetic session afterward.
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.communicate(timeout=2)
            os.close(master)
            os.close(slave)

    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    child = subprocess.Popen(argv, env=environment("interrupt"), stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=slave, start_new_session=True)
    try:
        assert select.select([master], [], [], 3)[0], "no prompt activity before interrupt"
        os.read(master, 65536)
        os.killpg(child.pid, signal.SIGINT)
        child.communicate(timeout=2)
        assert child.returncode != 0
        assert termios.tcgetattr(slave) == before
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=2)
        os.close(master)
        os.close(slave)

print("updater progress subprocess/PTY checks passed")
