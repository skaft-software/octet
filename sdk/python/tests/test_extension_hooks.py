"""Retained API 0.2 enrichment gates; no implicit API 0.1/0.3 upgrade."""
import unittest

from octet_extension import Extension, RpcError, persistence_metadata, post_mutation_rescan
from test_extension_v02 import RunningExtension, initialize_v02, rpc_request


class EnrichmentHookTests(unittest.TestCase):
    def test_progress_decoration_negotiation_bounds_correlation_and_terminal_fence(self):
        extension = Extension(api_version="0.2")

        @extension.tool(name="decorate", description="Decorate progress")
        def decorate(args):
            for label, detail in [
                ("", None), ("é" * 129, None), ("safe", "é" * 2049),
                ("unsafe\x1b[31m", None), ("safe", "bad\nline"),
                ("bad\x85", None),
            ]:
                with self.assertRaises(ValueError):
                    extension.progress_decoration(label, detail)
            self.assertEqual(extension.progress_decoration("é" * 128, "é" * 2048), 1)
            self.assertEqual(extension.progress_decoration("replacement"), 2)
            return "immutable-result"

        host = RunningExtension(extension)
        initialized = host.start(initialize_v02(tools=["decorate"], optional=["request_progress", "progress_decoration"]))
        self.assertIn("progress_decoration", initialized["result"]["protocol"]["features"])
        try:
            with self.assertRaises(RpcError):
                extension.progress_decoration("no-parent")
            host.reader.feed(rpc_request(2, "tool/call", {"name": "decorate", "arguments": {}, "context": {}}))
            result = host.writer.wait_for(lambda message: message.get("id") == 2)
            self.assertEqual(result["result"]["content"], [{"type": "text", "text": "immutable-result"}])
            messages = host.writer.matching(lambda message: message.get("method") == "$/progress")
            self.assertEqual([message["params"]["sequence"] for message in messages], [1, 2])
            self.assertTrue(all(message["params"]["request_id"] == 2 for message in messages))
            self.assertEqual(messages[1]["params"]["event"], {"type": "decoration", "label": "replacement"})
            with self.assertRaises(RpcError):
                extension.progress_decoration("late", request_id=2)
        finally:
            host.shutdown()

    def test_progress_decoration_is_not_implicitly_negotiated(self):
        extension = Extension(api_version="0.2")

        @extension.tool(name="decorate", description="Missing feature")
        def decorate(args):
            with self.assertRaises(RpcError):
                extension.progress_decoration("unnegotiated")
            return "done"

        host = RunningExtension(extension)
        host.start(initialize_v02(tools=["decorate"], optional=["request_progress"]))
        try:
            host.reader.feed(rpc_request(2, "tool/call", {"name": "decorate", "arguments": {}}))
            self.assertIn("result", host.writer.wait_for(lambda message: message.get("id") == 2))
            self.assertFalse(host.writer.matching(lambda message: message.get("method") == "$/progress"))
        finally:
            host.shutdown()

    def test_before_persistence_preserves_private_default_and_cannot_supply_host_provenance(self):
        extension = Extension(api_version="0.2")
        self.assertEqual(persistence_metadata({"note": "private"}), {"public": False, "value": {"note": "private"}})
        observed = []

        @extension.hook("before_persistence")
        def annotate(payload, context):
            observed.append((payload, context))
            return {"persistence_metadata": persistence_metadata({"note": "public"}, public=True)}

        host = RunningExtension(extension)
        host.start(initialize_v02(hooks=["before_persistence"]))
        try:
            payload = {"text_bytes": 42, "tool_call_count": 0}
            host.reader.feed(rpc_request(2, "hook/run", {"hook": "before_persistence", "payload": payload, "context": {}}))
            result = host.writer.wait_for(lambda message: message.get("id") == 2)["result"]
            self.assertEqual(result["persistence_metadata"], {"public": True, "value": {"note": "public"}})
            self.assertEqual(observed, [(payload, {})])
            for invalid in [
                {"value": {}, "namespace": "another.extension"},
                {"value": {}, "provenance": {"extension": "another"}},
                {"value": {}, "public": "yes"}, {},
            ]:
                with self.assertRaises(RpcError):
                    extension._hook_result("before_persistence", {"persistence_metadata": invalid})
        finally:
            host.shutdown()

    def test_post_mutation_shapes_and_frozen_api_01_registration(self):
        for hook in ["before_persistence", "post_mutation", "provider_retry"]:
            with self.subTest(hook=hook), self.assertRaises(ValueError):
                Extension(api_version="0.1").hook(hook)(lambda payload: {})
        extension = Extension(api_version="0.2")
        disposition = post_mutation_rescan(["resource:settings"])
        result = extension._hook_result("post_mutation", {"post_mutation": disposition})
        self.assertEqual(result["post_mutation"], disposition)
        for resources in [[], ["resource:settings"] * 33]:
            with self.assertRaises(ValueError):
                post_mutation_rescan(resources)
        for invalid in [
            {"action": "rescan_all"}, {"action": "request_rescan", "resource_ids": []},
            {"action": "no_rescan", "path": "/private"},
        ]:
            with self.assertRaises(RpcError):
                extension._hook_result("post_mutation", {"post_mutation": invalid})


if __name__ == "__main__":
    unittest.main()
