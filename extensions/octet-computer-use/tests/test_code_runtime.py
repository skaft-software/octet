"""Containment rejection tests. No code worker or OS sandbox setup is run."""
import unittest
from unittest.mock import Mock, patch

from octet_computer_use import code_runtime as runtime


def claimed_capabilities():
    return runtime.SandboxCapabilities(genuine=True, os_isolated=True,
        network_isolated=True, filesystem_isolated=True, process_isolated=True,
        resource_limits=True, no_new_privileges=True, platform="linux")


class CodeRuntimeTests(unittest.TestCase):
    def test_primitives_are_not_qualification(self):
        with patch.object(runtime.platform, "system", return_value="Linux"), \
             patch.object(runtime.os, "unshare", Mock(), create=True):
            self.assertFalse(runtime._detected_capabilities().available)
            self.assertFalse(runtime.select_sandbox().available)

    def test_capability_flags_cannot_enable_launcher(self):
        flags = claimed_capabilities()
        with patch.object(runtime.multiprocessing, "get_context", side_effect=AssertionError("no fork")), \
             patch.object(runtime, "_child_execute", side_effect=AssertionError("no model execution")):
            for sandbox in (runtime.ProcessSandbox(capabilities=flags),
                            runtime.OSSandbox(capabilities=flags),
                            runtime.select_sandbox(capabilities=flags)):
                self.assertFalse(sandbox.available)
                # Even mutating metadata must never enable the in-package launcher.
                sandbox.capabilities = flags
                result = sandbox.run(runtime.SandboxRequest("value = 1"))
                self.assertEqual(result.status, "sandbox_unavailable")

    def test_default_runtime_never_forwards_source(self):
        with patch.object(runtime, "_call_sandbox", side_effect=AssertionError("source forwarded")):
            self.assertEqual(runtime.CodeRuntime().execute("value = 1").status, "sandbox_unavailable")

    def test_injected_sandbox_is_not_qualification(self):
        sandbox = Mock(capabilities=claimed_capabilities(), available=True)
        result = runtime.CodeRuntime(sandbox=sandbox).execute("value = 1")
        self.assertEqual(result.status, "sandbox_unavailable")
        sandbox.run.assert_not_called()
        sandbox.execute.assert_not_called()

    def test_worker_entry_is_also_inert(self):
        sender, receiver = Mock(), Mock()
        with patch.object(runtime, "_setup_linux_sandbox", side_effect=AssertionError("no setup")), \
             patch.object(runtime, "_child_execute", side_effect=AssertionError("no execution")):
            runtime._sandbox_worker(b'{"source":"value = 1"}', sender, receiver, 1024)
        self.assertIn(b"sandbox_unavailable", sender.send_bytes.call_args.args[0])
        sender.close.assert_called_once()
        receiver.close.assert_called_once()

    def test_hostile_source_is_rejected_without_execution(self):
        for source in ("import os", "open('/tmp/should-not-exist', 'w')", "__import__('os')",
                       "value = (1).__class__", "while True: pass"):
            with self.subTest(source=source):
                self.assertEqual(runtime.CodeRuntime().execute(source).status, "rejected")

    def test_relaxed_restrictions_are_rejected(self):
        selected = runtime.CodeRuntime(restrictions=runtime.SandboxRestrictions(allow_network=True))
        self.assertEqual(selected.execute("value = 1").status, "rejected")

    def test_cancellation_before_source_admission(self):
        cancellation = runtime.CancellationToken()
        cancellation.cancel("fixture")
        self.assertEqual(runtime.CodeRuntime().execute("value = 1", cancellation=cancellation).status,
                         "cancelled")

    def test_all_platforms_fail_closed_without_claiming_native_evidence(self):
        for system in ("Darwin", "Windows", "Linux"):
            with patch.object(runtime.platform, "system", return_value=system):
                self.assertFalse(runtime._detected_capabilities().available)
                self.assertEqual(runtime.CodeRuntime().execute("value = 1").status,
                                 "sandbox_unavailable")
