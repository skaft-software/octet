"""Offline harness contracts, not evidence that a release executable passed."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
from types import SimpleNamespace
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PATH = ROOT / "scripts/check-release-journey.py"
SPEC = importlib.util.spec_from_file_location("release_journey", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ReleaseJourneyTests(unittest.TestCase):
    def test_tool_sse_has_exact_arguments_and_completion_boundary(self):
        arguments = {"oldText": "one\ntwo", "newText": "ONE\nTWO", "path": "lines.txt"}
        events = MODULE.completion(tool="edit", arguments=arguments).decode().split("\n\n")
        first = json.loads(events[0].removeprefix("data: "))
        call = first["choices"][0]["delta"]["tool_calls"][0]
        self.assertEqual(json.loads(call["function"]["arguments"]), arguments)
        self.assertIn('"finish_reason": "tool_calls"', events[1])
        self.assertEqual(events[2], "data: [DONE]")
        second = MODULE.completion(tool="edit", arguments=arguments).decode().split("\n\n")[0]
        second_call = json.loads(second.removeprefix("data: "))["choices"][0]["delta"]["tool_calls"][0]
        self.assertNotEqual(call["id"], second_call["id"])

    def test_expected_markers_must_come_from_tool_results_not_assistant_arguments(self):
        markers = ("1: one\n2: two\n", "hash=original")
        text = "".join(markers)
        MODULE.require_tool_result({"messages": [{"role": "tool", "content": text}]}, *markers)
        for message in [
            {"role": "assistant", "content": text},
            {"role": "tool", "content": text.replace("original", "normalized")},
            {"role": "tool", "content": "error read failed"},
        ]:
            with self.subTest(message=message), self.assertRaisesRegex(RuntimeError, "tool result"):
                MODULE.require_tool_result({"messages": [message]}, *markers)

    def test_loopback_provider_consumes_only_scripted_requests(self):
        provider = MODULE.Provider()
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            self.assertTrue(provider.url.startswith("http://127.0.0.1:"))
            with opener.open(provider.url + "models", timeout=3) as response:
                self.assertEqual(json.load(response)["data"][0]["id"], MODULE.MODEL)
            reply = MODULE.completion("done")
            provider.enqueue(reply)
            request = urllib.request.Request(provider.url + "chat/completions", data=b'{"model":"fixture-model"}')
            with opener.open(request, timeout=3) as response:
                self.assertEqual(response.read(), reply)
            with self.assertRaises(urllib.error.HTTPError) as raised:
                opener.open(request, timeout=3)
            self.assertEqual(raised.exception.code, 500)
            raised.exception.close()
            self.assertEqual(provider.errors, ["unscripted provider request"])
            self.assertEqual(len(provider.requests), 2)
        finally:
            provider.close()

    def test_failure_report_preserves_partial_results_and_binary_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "binary"
            binary.write_bytes(b"\x7fELFfixture bytes; qualify is mocked, nothing is executed")
            report = Path(directory) / "report.json"

            def fail(_binary, _host, _version, result):
                result["passed"].append("partial")
                raise RuntimeError("fixture boundary failed")

            arguments = [str(PATH), "--binary", str(binary), "--host-binary", str(binary),
                         "--expected-version", "0.9.2", "--report", str(report)]
            with patch.object(sys, "argv", arguments), patch.object(MODULE, "os", SimpleNamespace(name="posix")), \
                    patch.object(MODULE, "qualify", side_effect=fail), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(MODULE.main(), 1)
            result = json.loads(report.read_text())
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["passed"], ["partial"])
            self.assertEqual(result["binaries"]["octet"], hashlib.sha256(binary.read_bytes()).hexdigest())
            self.assertIn("live_provider", result["not_run"])

    def test_windows_refuses_before_starting_any_binary(self):
        arguments = [str(PATH), "--binary", "unused", "--host-binary", "unused", "--expected-version", "0.9.2"]
        with patch.object(sys, "argv", arguments), patch.object(MODULE, "os", SimpleNamespace(name="nt")), \
                patch.object(MODULE, "qualify") as qualify, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                MODULE.main()
            self.assertEqual(raised.exception.code, 2)
            qualify.assert_not_called()

    def test_windows_pe_is_rejected_even_when_invoked_from_posix_wsl(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "windows.exe"
            binary.write_bytes(b"MZfixture: must never be executed")
            arguments = [str(PATH), "--binary", str(binary), "--host-binary", str(binary),
                         "--expected-version", "0.9.2"]
            with patch.object(sys, "argv", arguments), patch.object(MODULE, "os", SimpleNamespace(name="posix")), \
                    patch.object(MODULE, "qualify") as qualify, contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as raised:
                    MODULE.main()
                self.assertEqual(raised.exception.code, 2)
                qualify.assert_not_called()

    @unittest.skipUnless(os.name == "posix", "POSIX harness uses owned sessions/killpg; Windows needs an isolated OS account")
    def test_wait_boundary_does_not_wait_for_process_exit_and_cleanup_reaps_child(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "started"
            code = "from pathlib import Path; import time; Path('started').write_text('yes'); time.sleep(10)"
            with MODULE.child([sys.executable, "-c", code], {"PATH": os.defpath}, Path(directory)) as child:
                start = time.monotonic()
                child.wait_until(marker.exists)
                self.assertLess(time.monotonic() - start, 5)
                child.stop(signal.SIGINT)
                self.assertNotEqual(child.result()[0], 0)
            self.assertIsNotNone(child.process.poll())

    @unittest.skipUnless(os.name == "posix", "POSIX child capture uses sessions; Windows has separate native gates")
    def test_excessive_output_fails_and_owned_child_is_reaped(self):
        code = "import time; print('x' * 100, flush=True); time.sleep(10)"
        with tempfile.TemporaryDirectory() as directory, patch.object(MODULE, "LIMIT", 16):
            with MODULE.child([sys.executable, "-c", code], {"PATH": os.defpath}, Path(directory)) as child:
                with self.assertRaisesRegex(RuntimeError, "exceeded"):
                    child.result()
            self.assertIsNotNone(child.process.poll())

    def test_binary_journey_gates_uploaded_posix_payloads(self):
        release = (ROOT / ".github/workflows/release-octet.yml").read_text()
        build = release.split("\n  build:\n", 1)[1].split("\n  build-windows:\n", 1)[0]
        self.assertIn('--binary "$RUNNER_TEMP/$package/octet"', build)
        self.assertIn('--host-binary "$RUNNER_TEMP/$package/octet-host"', build)
        self.assertLess(build.index('tar -xzf "$archive"'), build.index("check-release-journey.py"))
        self.assertLess(build.index("check-release-journey.py"), build.index("uses: actions/upload-artifact"))
        self.assertNotIn("continue-on-error", build)
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        smoke = ci.split("\n  smoke-install:\n", 1)[1]
        self.assertIn('"$RUNNER_TEMP/octet-install/bin/octet"', smoke)
        self.assertIn("check-release-journey.py", smoke)
        rc = ci.split("\n  release-candidate:\n", 1)[1].split("\n  msrv:\n", 1)[0]
        self.assertIn('--binary "$package/octet"', rc)
        self.assertLess(rc.index("check-release-journey.py"), rc.index("uses: actions/upload-artifact"))


if __name__ == "__main__":
    unittest.main()
