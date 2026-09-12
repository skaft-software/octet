#!/usr/bin/env python3
"""Exercise both production installer version gates with offline package fixtures."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

installer = Path(sys.argv[1])
fixtures = Path(sys.argv[2])
source = installer.read_text()
helper = source.split("<<'PYVERSION'\n", 1)[1].split("\nPYVERSION", 1)[0]


def stopped(pid):
    result = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, timeout=2)
    return result.returncode != 0 or not result.stdout.strip() or result.stdout.strip().startswith(b"Z")


with tempfile.TemporaryDirectory(prefix="version-probe-", dir=fixtures) as temporary:
    root = Path(temporary)
    for phase in ("pre", "final"):
        for behavior in ("success", "wrong", "nonzero", "oversize", "stderr-oversize", "no-newline", "extra-newline", "hang", "descendant"):
            home = root / (phase + "-" + behavior)
            home.mkdir()
            pidfile = home / "descendant.pid"
            env = dict(os.environ, HOME=str(home), OCTET_INSTALL_DIR=str(home / "bin"),
                       OCTET_NO_MODIFY_PATH="1", TERM="dumb",
                       OCTET_TEST_VERSION_PHASE=phase, OCTET_TEST_VERSION_BEHAVIOR=behavior,
                       OCTET_TEST_DESCENDANT_PID=str(pidfile))
            start = time.monotonic()
            result = subprocess.run(["sh", str(installer)], env=env, stdin=subprocess.DEVNULL,
                                    capture_output=True, timeout=10)
            elapsed = time.monotonic() - start
            assert elapsed < 8, (phase, behavior, elapsed)
            if behavior == "success":
                assert result.returncode == 0, result.stderr
                assert b"is installed." in result.stderr
            else:
                assert result.returncode != 0, (phase, behavior)
                assert b"is installed." not in result.stderr
                assert b"octet installation failed" in result.stderr
                assert b"probe private diagnostic" not in result.stdout + result.stderr
                assert b"x" * 1024 not in result.stdout + result.stderr
                assert (home / "bin/octet").exists() == (phase == "final")
                if pidfile.exists():
                    assert stopped(int(pidfile.read_text())), (phase, behavior, "surviving child")

    # Exercise cancellation of the exact Python helper while an isolated probe
    # and descendant hold pipes. No installer or caller process group is killed.
    probe = root / "cancel-probe"
    pidfile = root / "cancel.pid"
    probe.write_text('#!/bin/sh\nsleep 30 &\necho "$!" > "$OCTET_TEST_DESCENDANT_PID"\nwait\n')
    probe.chmod(0o755)
    for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        pidfile.unlink(missing_ok=True)
        process = subprocess.Popen([sys.executable, "-c", helper, str(probe), "0.7.6"],
                                   env=dict(os.environ, OCTET_TEST_DESCENDANT_PID=str(pidfile)),
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 3
            while not pidfile.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            assert pidfile.exists()
            process.send_signal(number)
            output, errors = process.communicate(timeout=3)
            assert process.returncode == 128 + number, (number, errors)
            assert output == errors == b"", (output, errors)
            assert stopped(int(pidfile.read_text())), "cancelled probe left a descendant"
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=2)

print("installer version gates: 18 production-path cases and 3 cancellation/descendant cleanup cases passed")
