"""Offline PTY assertions for the host-owned terminal handoff (P3).

Drives the real renderer suspension/resume through a pty and checks that the
process terminal is restored at every boundary. Never touches a human terminal:
the fixture runs against the pty slave this script owns.
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
entry = "tui::view::tests::terminal_handoff_pty_fixture"


def drain(master, data):
    """Keep the pty readable so a painting renderer can never block on write."""
    while select.select([master], [], [], 0)[0]:
        chunk = os.read(master, 65536)
        if not chunk:
            return
        data.extend(chunk)


with tempfile.TemporaryDirectory(prefix="octet-handoff-") as root:
    marker = pathlib.Path(root)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    before = termios.tcgetattr(slave)
    env = dict(os.environ, OCTET_TEST_TERMINAL_HANDOFF=root,
               TERM="xterm-256color", LC_ALL="C.UTF-8")
    child = subprocess.Popen(
        [binary, "--exact", entry, "--nocapture"],
        env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    output = bytearray()
    try:
        def reach(name, seconds=20):
            deadline = time.monotonic() + seconds
            path = marker / name
            while time.monotonic() < deadline:
                if path.exists():
                    return
                if child.poll() is not None:
                    raise AssertionError(
                        f"fixture exited before {name}: {bytes(output)!r}")
                drain(master, output)
                time.sleep(0.02)
            raise AssertionError(f"fixture never reached {name}: {bytes(output)!r}")

        reach("ready")
        assert not (termios.tcgetattr(slave)[3] & termios.ICANON), \
            "the fixture never entered raw mode"

        reach("ceded")
        # Suspension restored the process terminal exactly, byte for byte.
        assert termios.tcgetattr(slave) == before, \
            "suspend did not restore the process terminal"

        # A keystroke the ceded holder would own. It must survive the cede.
        os.write(master, b"X\n")

        reach("resuming")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if not (termios.tcgetattr(slave)[3] & termios.ICANON):
                break
            drain(master, output)
            time.sleep(0.02)
        else:
            raise AssertionError("resume did not re-enter raw mode")

        os.write(master, b"R")

        # Keep draining until the fixture exits: the resumed renderer paints to
        # the pty, and a full buffer would block it before it could stop.
        deadline = time.monotonic() + 20
        while child.poll() is None:
            if time.monotonic() > deadline:
                raise AssertionError(
                    f"fixture did not exit after resume: {bytes(output)!r}")
            drain(master, output)
            time.sleep(0.02)
        drain(master, output)
        assert child.returncode == 0, \
            f"fixture failed ({child.returncode}): {bytes(output)!r}"
        assert termios.tcgetattr(slave) == before, \
            "leaving the renderer did not restore the process terminal"
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=2)
        os.close(master)
        os.close(slave)
