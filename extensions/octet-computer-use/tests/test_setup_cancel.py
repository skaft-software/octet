"""Provisioning cancellation owns and reaps its subprocesses (no driver/index)."""
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from octet_extension import CancellationToken, CancelledError
from octet_computer_use import driver


class SetupCancellationTests(unittest.TestCase):
    def test_cancelled_request_does_not_launch_a_probe_or_installer(self):
        token = CancellationToken("setup-test")
        token._cancel("test")
        with patch("octet_extension.current_cancellation", return_value=token), \
                patch.object(driver.subprocess, "run") as run, \
                patch.object(driver.subprocess, "Popen") as spawn:
            with self.assertRaises(CancelledError):
                driver._run(["unused-installer"])
        run.assert_not_called()
        spawn.assert_not_called()

    def test_request_process_preserves_utf8_output_and_detached_stdin(self):
        token = CancellationToken("setup-test")
        with patch("octet_extension.current_cancellation", return_value=token):
            result = driver._run(
                [sys.executable, "-c", "import sys; print('✓'); print(sys.stdin.read())"],
                env=driver._install_environment(), timeout=5,
            )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "✓\n\n")
        self.assertEqual(result.stderr, "")

    @unittest.skipUnless(os.name == "nt", "native Windows job/process-tree check")
    def test_cancel_and_timeout_reap_the_parent_and_its_child(self):
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        from ctypes import wintypes
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel.WaitForSingleObject.restype = wintypes.DWORD
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.CloseHandle.restype = wintypes.BOOL
        for cancel in (True, False):
            with self.subTest(cancel=cancel), tempfile.TemporaryDirectory() as temporary:
                ready = Path(temporary) / "ready.json"
                token = CancellationToken("setup-test")
                outcome = {}
                handles = []
                script = (
                    "import json, os, pathlib, subprocess, sys, time; "
                    "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)']); "
                    "ready = pathlib.Path(sys.argv[1]); stage = ready.with_suffix('.tmp'); "
                    "stage.write_text(json.dumps([os.getpid(), child.pid])); stage.replace(ready); "
                    "time.sleep(60)"
                )

                def run():
                    try:
                        driver._run([sys.executable, "-c", script, str(ready)],
                                    timeout=10 if cancel else 2)
                    except Exception as error:
                        outcome["error"] = error

                with patch("octet_extension.current_cancellation", return_value=token):
                    worker = threading.Thread(target=run, daemon=True)
                    worker.start()
                    try:
                        deadline = time.monotonic() + 5
                        while not ready.exists() and worker.is_alive() and time.monotonic() < deadline:
                            time.sleep(0.01)
                        self.assertTrue(ready.exists(), outcome)
                        # Pin handles before cancellation; PID reuse cannot turn this
                        # into a check of, or a signal to, an unrelated process.
                        for pid in json.loads(ready.read_text()):
                            handle = kernel.OpenProcess(0x100000, False, pid)  # SYNCHRONIZE
                            self.assertTrue(handle, ctypes.get_last_error())
                            handles.append(handle)
                        if cancel:
                            token._cancel("test")
                        worker.join(timeout=5)
                        self.assertFalse(worker.is_alive())
                        error = outcome.get("error")
                        if cancel:
                            self.assertIsInstance(error, CancelledError)
                        else:
                            self.assertIsInstance(error, driver.ProvisionError)
                            self.assertIn("timed out", str(error))
                        for handle in handles:
                            self.assertEqual(kernel.WaitForSingleObject(handle, 1000), 0)
                    finally:
                        token._cancel("test cleanup")
                        worker.join(timeout=5)
                        for handle in handles:
                            kernel.CloseHandle(handle)
