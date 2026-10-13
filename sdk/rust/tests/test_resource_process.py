"""Real Rust SDK child; production-host acceptance lives in host-check."""
import json
from pathlib import Path
import queue
import tempfile
import time
import unittest
from test_process import Peer, ROOT, offer

BINARY = ROOT / "sdk/rust/target/debug/examples/resource-probe"
OWNER = {"session_id": "session", "extension_instance_id": "instance", "process_generation": 1}
FEATURES = ["resource_refs_v1", "operation_descriptors_v1"]
LIMITS = {"max_records": 256, "max_registrations_per_parent": 32}


class ResourceProcess(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.workspace = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)
        self.peer = Peer([str(BINARY), str(self.workspace)])
        self.addCleanup(self.peer.close)
        self.serial = 0

    def initialize(self, features=True):
        params = offer()
        params["contributes"]["tools"] = ["create", "add", "combine", "release_saved", "pair"]
        params["workspace"] = str(self.workspace)
        if features:
            params["protocol"]["optional_features"] += FEATURES
            params["protocol"]["limits"]["resource_refs_v1"] = LIMITS
        self.peer.send("initialize", params, 1)
        return self.peer.receive()

    def frame(self):
        frame = self.peer.frames.get(timeout=3)
        self.assertIsNotNone(frame, bytes(self.peer.diagnostics))
        return json.loads(frame)

    def reply(self, request, result=None, error=None):
        value = {"jsonrpc": "2.0", "id": request["id"]}
        value["error" if error else "result"] = error if error else result
        self.peer.raw(json.dumps(value).encode() + b"\n")

    def send_call(self, name, arguments, request_id=2, owner=OWNER):
        self.peer.send("tool/call", {"name": name, "arguments": arguments, "context": {"resource_owner": owner}}, request_id)

    def create(self, name="ordinary", request_id=2):
        self.send_call("create", {"name": name}, request_id)
        request = self.frame()
        self.assertEqual(request["method"], "resource/register")
        self.assertEqual(request["params"], {"parent_request_id": request_id, "type": "fixture.Counter"})
        self.serial += 1
        reference = {"$resource": f"opaque-{self.serial}", "type": "fixture.Counter"}
        self.reply(request, reference)
        return reference, self.peer.receive()

    def dispose(self, reference, request_id=50):
        self.peer.send("resource/dispose", {"resources": [reference], "reason": "retired"}, request_id)
        result = self.peer.receive()
        self.assertEqual(result["id"], request_id)
        return result["result"]["results"][0]

    def events(self):
        path = self.workspace / "resources.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines(keepends=True) if line.endswith("\n")] if path.exists() else []

    def wait_event(self, kind):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if any(event["kind"] == kind for event in self.events()):
                return
            time.sleep(0.002)
        self.fail(f"missing {kind}: {self.events()}")

    def test_nominal_catalog_persistent_native_and_all_slots(self):
        selection = self.initialize()["result"]
        self.assertEqual(selection["protocol"]["limits"]["resource_refs_v1"], LIMITS)
        tools = {tool["name"]: tool for tool in selection["tools"]}
        self.assertEqual(tools["create"]["output_schema"]["properties"]["counter"]["properties"]["type"]["enum"], ["fixture.Counter"])
        self.assertEqual(tools["add"]["operation"]["receiver"], "/counter")
        self.assertEqual([slot["path"] for slot in tools["combine"]["operation"]["resource_inputs"]], ["/first", "/second/counter"])
        reference, created = self.create()
        self.assertEqual(created["result"]["structured_content"], {"counter": reference})
        self.send_call("add", {"counter": reference, "delta": 7})
        self.assertEqual(self.peer.receive()["result"]["structured_content"], {"value": 7})
        self.send_call("combine", {"first": reference, "second": {"counter": reference}})
        self.assertEqual(self.peer.receive()["result"]["structured_content"], {"value": 14})
        self.assertEqual(self.dispose(reference)["status"], "completed")
        self.peer.shutdown()
        events = self.events()
        self.assertEqual(len({event["pid"] for event in events}), 1)
        self.assertTrue(all(event["thread"] == "octet-native-dispose" for event in events if event["kind"] == "dispose"))

    def test_forged_wrong_nominal_owner_and_secondary_slot_zero_entry(self):
        self.initialize()
        reference, _ = self.create()
        before = len(self.events())
        invalid = [{**reference, "$resource": "forged"}, {**reference, "type": "other.Type"}, {**reference, "extra": 1}]
        for value in invalid:
            self.send_call("add", {"counter": value, "delta": 1})
            self.assertEqual(self.peer.receive()["error"]["code"], -32602)
        self.send_call("combine", {"first": reference, "second": {"counter": invalid[0]}})
        self.assertEqual(self.peer.receive()["error"]["code"], -32602)
        for key, value in [("session_id", "foreign"), ("extension_instance_id", "other-instance"), ("process_generation", 2)]:
            self.send_call("add", {"counter": reference, "delta": 1}, owner={**OWNER, key: value})
            self.assertEqual(self.peer.receive()["error"]["code"], -32602)
        self.assertEqual(len(self.events()), before)
        self.peer.shutdown()

    def test_failed_output_retires_provisional_before_disposal(self):
        self.initialize()
        for mode in ["invalid-output", "error"]:
            reference, result = self.create(mode)
            if mode == "invalid-output":
                self.assertEqual(result["error"]["code"], -32603)
            else:
                self.assertTrue(result["result"]["is_error"])
            before = len(self.events())
            self.send_call("add", {"counter": reference, "delta": 1})
            self.assertEqual(self.peer.receive()["error"]["code"], -32602)
            self.assertEqual(len(self.events()), before)
            self.assertEqual(self.dispose(reference)["status"], "completed")
        self.peer.shutdown()

    def test_cleanup_failure_and_panic_never_resurrect(self):
        self.initialize()
        for mode in ["fail-dispose", "panic-dispose"]:
            reference, _ = self.create(mode)
            self.assertEqual(self.dispose(reference)["status"], "failed")
            self.send_call("add", {"counter": reference, "delta": 1})
            self.assertEqual(self.peer.receive()["error"]["code"], -32602)
            self.assertEqual(self.dispose(reference)["status"], "failed")
        self.peer.shutdown()
        self.assertEqual(len([event for event in self.events() if event["kind"] == "dispose"]), 2)

    def test_cancel_keeps_native_pinned_until_execution_settles(self):
        self.initialize()
        reference, _ = self.create()
        self.send_call("add", {"counter": reference, "delta": 1, "mode": "cancel"}, 20)
        self.wait_event("holding")
        self.peer.send("$/cancelRequest", {"id": 20})
        self.peer.send("resource/dispose", {"resources": [reference], "reason": "retired"}, 21)
        with self.assertRaises(queue.Empty):
            self.peer.frames.get(timeout=0.05)
        self.assertFalse(any(event["kind"] == "dispose" for event in self.events()))
        (self.workspace / "settle").touch()
        self.assertEqual(self.peer.receive()["error"]["code"], -32800)
        self.assertEqual(self.peer.receive()["result"]["results"][0]["status"], "completed")
        kinds = [event["kind"] for event in self.events()]
        self.assertLess(kinds.index("settled"), kinds.index("dispose"))
        self.peer.shutdown()

    def test_reverse_release_busy_and_separate_disposal(self):
        self.initialize()
        reference, _ = self.create()
        self.send_call("add", {"counter": reference, "delta": 1, "mode": "release"}, 10)
        request = self.frame()
        self.assertEqual(request["method"], "resource/release")
        self.assertEqual(request["params"], {"parent_request_id": 10, "resource": reference})
        self.reply(request, error={"code": -32602, "message": "resource_busy", "data": {"code": "resource_busy"}})
        self.assertEqual(self.peer.receive()["error"]["message"], "resource_busy")
        self.send_call("release_saved", {}, 11)
        request = self.frame()
        self.reply(request, {"retired": True, "cleanup": "pending"})
        self.assertEqual(self.peer.receive()["result"]["structured_content"], {"value": 1})
        self.assertFalse(any(event["kind"] == "dispose" for event in self.events()))
        self.assertEqual(self.dispose(reference)["status"], "completed")
        self.peer.shutdown()

    def test_cancel_reverse_wait_disposes_unpublished_and_ignores_late_reply(self):
        self.initialize()
        self.send_call("create", {"name": "cancel-before-register-reply"}, 30)
        request = self.frame()
        self.peer.send("$/cancelRequest", {"id": 30})
        cancellation = self.frame()
        self.assertEqual(cancellation["method"], "$/cancelRequest")
        self.assertEqual(cancellation["params"]["id"], request["id"])
        self.assertEqual(self.peer.receive()["error"]["code"], -32800)
        reference = {"$resource": "late", "type": "fixture.Counter"}
        self.reply(request, reference)
        self.reply(request, reference)  # Duplicate/late replies cannot grant or settle a different call.
        self.assertEqual(self.dispose(reference)["status"], "failed")
        self.assertEqual(len([event for event in self.events() if event["kind"] == "dispose"]), 1)
        self.assertIn("result", self.create()[1])
        self.peer.shutdown()

    def test_unnegotiated_resource_catalog_refused(self):
        self.assertEqual(self.initialize(False)["error"]["code"], -32000)
        self.assertEqual(self.events(), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
