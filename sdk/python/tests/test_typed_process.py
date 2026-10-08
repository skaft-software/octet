"""Raw executable SDK checks; production host acceptance is a separate Rust probe."""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest

TOOLS = ["typed_roundtrip", "invalid_output", "typed_wait", "typed_progress", "typed_diagnostics", "malformed_diagnostics"]
VALID = {"name": "λ😀", "enabled": True, "samples": [1.0]}


class ProcessHarness:
    fixture_name = "typed_fixture.py"
    tools = TOOLS

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="octet-python-typed-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.log = self.root / "calls.jsonl"
        self.stderr = (self.root / "stderr.log").open("w+", encoding="utf-8")
        self.addCleanup(self.stderr.close)
        self.process = subprocess.Popen(
            [sys.executable, "-u", str(Path(__file__).with_name(self.fixture_name)), str(self.log)],
            cwd=self.root, env={**os.environ, "HOME": str(self.root), "PYTHONDONTWRITEBYTECODE": "1"},
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, text=True, encoding="utf-8",
        )
        self.addCleanup(self.cleanup_process)
        self.messages = queue.Queue()
        def read():
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        self.sequence = 0

    def cleanup_process(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=3)
        self.process.stdin.close()
        self.reader.join(timeout=3)
        self.process.stdout.close()

    def send(self, method, params=None, *, notification=False):
        self.sequence += 1
        message = {"jsonrpc": "2.0", "method": method, "params": params or {}}
        if not notification:
            message["id"] = self.sequence
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        return self.sequence

    def receive(self):
        try:
            return self.messages.get(timeout=4)
        except queue.Empty:
            self.stderr.seek(0)
            self.fail("process did not respond: " + self.stderr.read())

    def initialize(self, *, progress=True):
        self.send("initialize", {"api_version": "0.4", "contributes": {"tools": self.tools}, "protocol": {
            "version": "0.4", "required_features": ["request_cancellation", "content_parts"],
            "optional_features": ["request_progress"] if progress else [],
            "limits": {"max_concurrent_requests": 1}}})
        result = self.receive()["result"]
        self.assertEqual(result["api_version"], "0.4")
        self.assertEqual(len(result["tools"]), len(self.tools))
        return result

    def call(self, name="typed_roundtrip", arguments=None):
        self.send("tool/call", {"name": name, "arguments": VALID if arguments is None else arguments})
        return self.receive()

    def events(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def barrier(self, event):
        deadline = time.monotonic() + 3
        while event not in [row["event"] for row in self.events()]:
            if time.monotonic() > deadline:
                self.fail("child never reached barrier " + event)
            time.sleep(0.005)

    def shutdown(self):
        request = self.send("shutdown")
        self.assertEqual(self.receive(), {"jsonrpc": "2.0", "id": request, "result": {}})
        self.assertEqual(self.process.wait(timeout=3), 0)
        self.assertEqual([r["event"] for r in self.events()].count("shutdown"), 1)


class TypedProcessTests(ProcessHarness, unittest.TestCase):
    def test_A01_typed_roundtrip_and_A04_optional_values(self):
        catalog = self.initialize()
        tool = next(t for t in catalog["tools"] if t["name"] == "typed_roundtrip")
        self.assertEqual(tool["parameters"], tool["output_schema"])
        for arguments in [VALID, dict(VALID, note=None), dict(VALID, note="present")]:
            result = self.call(arguments=arguments)["result"]
            self.assertEqual(result["structured_content"], dict(arguments, note=arguments.get("note")))
            self.assertEqual(result["content"], [{"type": "text", "text": "Echoed typed record."}])
        self.shutdown()

    def test_A02_invalid_input_zero_entries_and_healthy_followup(self):
        self.initialize()
        for invalid in [{}, dict(VALID, name=1), dict(VALID, extra=True), dict(VALID, samples=[float("inf")]),
                        dict(VALID, samples=[2**53]), dict(VALID, name="\ud800")]:
            self.assertEqual(self.call(arguments=invalid)["error"]["code"], -32602)
        self.assertEqual([r["event"] for r in self.events()], ["started"])
        self.assertFalse(self.call()["result"]["is_error"])
        self.shutdown()

    def test_A03_invalid_output_not_success(self):
        self.initialize()
        self.assertEqual(self.call("invalid_output")["error"]["code"], -32603)
        self.assertFalse(self.call()["result"]["is_error"])
        self.shutdown()

    def test_A05_diagnostics_and_invalid_reserved_metadata(self):
        self.initialize()
        result = self.call("typed_diagnostics")["result"]
        self.assertTrue(result["is_error"])
        self.assertNotIn("structured_content", result)
        diagnostic = result["metadata"]["octet_diagnostics_v1"][0]
        self.assertEqual(diagnostic["code"], "fixture.invalid")
        self.assertEqual(diagnostic["primary"]["span"], {"start_byte": 0, "end_byte": 1})
        self.assertEqual(diagnostic["fixes"][0]["edits"][0]["replacement"], "b")
        self.assertEqual(result["content"][1]["text"], "error[fixture.invalid]: Fixture domain failure.")
        self.assertEqual(self.call("malformed_diagnostics")["error"]["code"], -32603)
        self.shutdown()

    def test_A06_cancellation_one_terminal_same_process(self):
        self.initialize()
        request = self.send("tool/call", {"name": "typed_wait", "arguments": VALID})
        self.barrier("entered")
        self.send("$/cancelRequest", {"id": request}, notification=True)
        self.send("$/cancelRequest", {"id": request}, notification=True)
        reply = self.receive()
        self.assertEqual(reply["id"], request)
        self.assertEqual(reply["error"]["code"], -32800)
        self.assertFalse(self.call()["result"]["is_error"])
        self.assertEqual(len({r["pid"] for r in self.events()}), 1)
        self.assertEqual([r["event"] for r in self.events()].count("cancelled"), 1)
        self.shutdown()
        self.reader.join(timeout=3)
        self.assertTrue(self.messages.empty(), "duplicate terminal")

    def test_A07_negotiated_progress_is_ephemeral(self):
        self.initialize()
        request = self.send("tool/call", {"name": "typed_progress", "arguments": VALID})
        for sequence in (1, 2):
            reply = self.receive()
            self.assertEqual(reply["method"], "$/progress")
            self.assertEqual(reply["params"]["sequence"], sequence)
            self.assertEqual(reply["params"]["request_id"], request)
        result = self.receive()["result"]
        self.assertNotIn("Step", json.dumps(result))
        self.shutdown()

    def test_A07_unnegotiated_progress_is_refused(self):
        self.initialize(progress=False)
        self.assertIn("error", self.call("typed_progress"))
        self.assertFalse(self.call()["result"]["is_error"])
        self.shutdown()

    def test_A08_invalid_oversized_frames_and_EOF(self):
        self.initialize()
        for line in ["{\n", json.dumps({"jsonrpc": "2.0", "id": 80, "method": "bad", "params": "x" * (1024 * 1024)}) + "\n"]:
            self.process.stdin.write(line)
            self.process.stdin.flush()
            self.assertEqual(self.receive()["error"]["code"], -32700)
        self.assertFalse(self.call()["result"]["is_error"])
        self.process.stdin.close()
        self.assertEqual(self.process.wait(timeout=3), 0)
        self.assertEqual([r["event"] for r in self.events()], ["started", "typed_roundtrip"])


if __name__ == "__main__":
    unittest.main()
