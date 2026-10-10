"""Real process utility/absence tests, NOT ngspice or F acceptance.

Disposable Python children exercise only POSIX lifecycle helpers. They are never
named ngspice, passed to the solver runner, or used to produce numerical data.
"""
from contextlib import contextmanager
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import threading
import unittest

from extension import ext  # Selects the actual adjacent source SDK.
from octet_extension import CancelledError, CancellationToken
from interrupt_probe import hold_until_cancelled
from solver import _stop_and_wait

HERE = Path(__file__).resolve().parent


@contextmanager
def waiting_child(ignore_term=False):
    code = "import signal,sys; "
    if ignore_term:
        code += "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
    code += "print('ready', flush=True); sys.stdin.read()"
    child = subprocess.Popen([sys.executable, "-c", code], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            if not selector.select(3.0) or child.stdout.readline() != b"ready\n":
                raise AssertionError("real utility child did not reach its explicit ready barrier")
        yield child
    finally:
        _stop_and_wait(child)
        child.stdout.close()
        child.stdin.close()


class ProcessUtilityTests(unittest.TestCase):
    def test_missing_ngspice_is_nonzero_blocked_before_events_or_protocol(self):
        with tempfile.TemporaryDirectory(prefix=".process-test-", dir=HERE) as directory:
            env = {**os.environ, "PATH": directory, "HOME": directory,
                   "PYTHONDONTWRITEBYTECODE": "1"}
            events = Path(directory) / "events.jsonl"
            for script, args in (("check_prerequisites.py", []),
                                 ("extension.py", ["--events", str(events)])):
                result = subprocess.run([sys.executable, str(HERE / script), *args], env=env,
                                        cwd=directory, capture_output=True, timeout=5, check=False)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, b"")
                self.assertEqual(result.stderr, b"BLOCKED: real ngspice executable is missing; no fallback.\n")
                self.assertFalse(events.exists())
            evidence, target = Path(directory) / "evidence", Path(directory) / "target"
            result = subprocess.run([sys.executable, str(HERE / "run_conformance.py"), "--run-host",
                                     "--target-dir", str(target), "--evidence", str(evidence)],
                                    env=env, cwd=directory, capture_output=True, timeout=5, check=False)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, b"")
            self.assertIn(b"F01 spice_acceptance: BLOCKED", result.stderr)
            self.assertIn(b"F02 spice_interrupt: BLOCKED", result.stderr)
            self.assertFalse(target.exists())
            self.assertFalse(evidence.exists())

    def test_stop_wait_reaps_a_real_utility_child(self):
        with waiting_child() as child:
            self.assertIsNone(child.poll())
            _stop_and_wait(child)
            self.assertIsNotNone(child.returncode)

    def test_stop_wait_escalates_for_real_sigterm_ignoring_child(self):
        with waiting_child(ignore_term=True) as child:
            _stop_and_wait(child)
            self.assertEqual(child.returncode, -signal.SIGKILL)

    def test_explicit_interruption_barrier_keeps_child_until_authorized_stop(self):
        with tempfile.TemporaryDirectory(prefix=".barrier-test-", dir=HERE) as directory:
            root = Path(directory)
            with waiting_child() as child:
                token = CancellationToken("utility-test-not-a-host-call")
                ready, cancelled, done = threading.Event(), threading.Event(), threading.Event()
                errors = []

                def record(event, pid):
                    self.assertEqual(pid, child.pid)
                    if event == "interrupt_ready":
                        ready.set()
                    elif event == "interrupt_cancelled":
                        cancelled.set()

                def work():
                    try:
                        hold_until_cancelled(child.pid, token, root, record)
                    except BaseException as error:
                        errors.append(error)
                    finally:
                        _stop_and_wait(child)
                        done.set()

                thread = threading.Thread(target=work)
                thread.start()
                try:
                    self.assertTrue(ready.wait(3), "native stopped-state barrier")
                    token._cancel("utility test")
                    self.assertTrue(cancelled.wait(3), "cancellation-observed barrier")
                    self.assertIsNone(child.poll(), "caller cancellation is not native settlement")
                    self.assertFalse(done.is_set())
                finally:
                    token._cancel("cleanup")
                    (root / "allow_stop").touch()
                    thread.join(timeout=7)
                self.assertFalse(thread.is_alive())
                self.assertTrue(done.is_set())
                self.assertEqual(len(errors), 1)
                self.assertIsInstance(errors[0], CancelledError)
                self.assertIsNotNone(child.poll())


if __name__ == "__main__":
    unittest.main()
