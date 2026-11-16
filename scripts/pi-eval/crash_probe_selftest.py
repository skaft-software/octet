#!/usr/bin/env python3
"""Offline checks of crash evidence classification, not compatibility claims."""
import signal
import sys
import tempfile
import unittest
from pathlib import Path

from crash_probe import supervise


class CrashProbeTests(unittest.TestCase):
    def probe(self, code, timeout=3):
        with tempfile.TemporaryDirectory() as directory:
            return supervise([sys.executable, "-c", code], Path(directory), timeout)

    def test_eof_is_not_a_signal_or_oom(self):
        report = self.probe("import sys; sys.stderr.write('fixture failure\\n'); sys.exit(7)")
        self.assertEqual(report["host_exit_code"], 7)
        self.assertIsNone(report["host_signal"])
        self.assertIsNone(report["supervisor_action"])
        self.assertIn("fixture failure", report["stderr_tail"])

    def test_signal_is_recorded_as_signal(self):
        report = self.probe("import os, signal; os.kill(os.getpid(), signal.SIGTERM)")
        self.assertIsNone(report["host_exit_code"])
        self.assertEqual(report["host_signal"], signal.Signals(signal.SIGTERM).name)
        self.assertIsNone(report["supervisor_action"])

    def test_supervisor_kill_is_not_an_unexplained_crash(self):
        report = self.probe("import time; time.sleep(10)", timeout=0.05)
        self.assertEqual(report["supervisor_action"], "deadline")
        self.assertEqual(report["host_signal"], "SIGKILL")


if __name__ == "__main__":
    unittest.main()
