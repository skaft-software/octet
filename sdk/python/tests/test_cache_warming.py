"""API 0.4 typed cache-refresh advice; no provider or network calls."""

import io
import threading
import unittest

from octet_extension import (
    CacheWarmingDecisionPayload,
    Extension,
    RpcError,
    cache_warming_decision,
)
from test_extension_v02 import RunningExtension, initialize_v02, rpc_request


def initialize_v04(*, offered=True):
    request = initialize_v02(
        hooks=["cache_warming_decision"],
        optional=["cache_warming_decision"] if offered else [],
    )
    request["params"]["api_version"] = "0.4"
    request["params"]["protocol"]["version"] = "0.4"
    return request


PAYLOAD: CacheWarmingDecisionPayload = {
    "model": "cache-model",
    "decision": {
        "phase": "idle",
        "warm_cost_microdollars": 100,
        "miss_cost_microdollars": 1_000,
        "continuation_probability": 0.15,
        "expected_savings_microdollars": 50,
        "economics_available": True,
        "action": "warm",
    },
}
OWNER_CONTEXT = {"resource_owner": {
    "session_id": "owner-one", "extension_instance_id": "instance-one",
    "process_generation": 3,
}}


class CacheWarmingTests(unittest.TestCase):
    def test_typed_decorator_and_helper_round_trip_all_actions_and_owner(self):
        extension = Extension(api_version="0.4", stderr=io.StringIO())
        actions = iter(["warm", "stop", None])
        observed = []

        @extension.cache_warming_decision
        def advise(payload, context):
            observed.append((payload, context))
            return next(actions)

        host = RunningExtension(extension)
        initialized = host.start(initialize_v04())
        self.assertIn("cache_warming_decision", initialized["result"]["protocol"]["features"])
        try:
            for request_id, action in enumerate(["warm", "stop", None], start=2):
                host.reader.feed(rpc_request(request_id, "hook/run", {
                    "hook": "cache_warming_decision", "payload": PAYLOAD,
                    "context": OWNER_CONTEXT,
                }))
                result = host.writer.wait_for(lambda message: message.get("id") == request_id)["result"]
                self.assertEqual(result["cache_warming_decision"], action)
                self.assertEqual(result["disposition"], {"action": "continue"})
                self.assertEqual(result["context"], [])
            self.assertEqual(observed, [(PAYLOAD, OWNER_CONTEXT)] * 3)
            self.assertEqual(cache_warming_decision("stop"), {"cache_warming_decision": "stop"})
        finally:
            host.shutdown()

    def test_generic_hook_and_no_opinion_remain_supported(self):
        extension = Extension(api_version="0.4", stderr=io.StringIO())
        responses = iter([cache_warming_decision("warm"), {}, None])

        @extension.hook("cache_warming_decision")
        def advise(payload):
            return next(responses)

        host = RunningExtension(extension)
        host.start(initialize_v04())
        try:
            for request_id, expected in enumerate(["warm", None, None], start=2):
                host.reader.feed(rpc_request(request_id, "hook/run", {
                    "hook": "cache_warming_decision", "payload": PAYLOAD,
                }))
                result = host.writer.wait_for(lambda message: message.get("id") == request_id)["result"]
                self.assertEqual(result.get("cache_warming_decision"), expected)
        finally:
            host.shutdown()

    def test_invalid_actions_and_legacy_registration_fail_closed(self):
        for version in ["0.1", "0.2", "0.3"]:
            extension = Extension(api_version=version)
            with self.subTest(version=version), self.assertRaises(ValueError):
                extension.hook("cache_warming_decision")(lambda payload: {})
            with self.subTest(version=version), self.assertRaises(ValueError):
                extension.cache_warming_decision(lambda payload: None)
        extension = Extension(api_version="0.4")
        extension._features = frozenset(["cache_warming_decision"])
        for action in ["retry", True, 1, ["warm"], {"action": "warm"}]:
            with self.subTest(action=action):
                with self.assertRaises(ValueError):
                    cache_warming_decision(action)
                with self.assertRaises(RpcError) as error:
                    extension._hook_result("cache_warming_decision", {"cache_warming_decision": action})
                self.assertEqual(error.exception.message, "invalid cache_warming_decision action")
        extension._features = frozenset()
        with self.assertRaises(RpcError):
            extension._hook_result("cache_warming_decision", cache_warming_decision("warm"))

    def test_unnegotiated_hook_never_invokes_extension_code(self):
        extension = Extension(api_version="0.4", stderr=io.StringIO())
        invoked = []

        @extension.cache_warming_decision
        def advise(payload):
            invoked.append(payload)
            return "warm"

        host = RunningExtension(extension)
        host.start(initialize_v04(offered=False))
        try:
            host.reader.feed(rpc_request(2, "hook/run", {
                "hook": "cache_warming_decision", "payload": PAYLOAD,
            }))
            response = host.writer.wait_for(lambda message: message.get("id") == 2)
            self.assertEqual(response["error"]["code"], -32601)
            self.assertEqual(invoked, [])
        finally:
            host.shutdown()

    def test_handler_failure_is_secret_safe(self):
        diagnostics = io.StringIO()
        extension = Extension(api_version="0.4", stderr=diagnostics)

        @extension.cache_warming_decision
        def advise(payload):
            raise RuntimeError("secret-provider-message")

        host = RunningExtension(extension)
        host.start(initialize_v04())
        try:
            host.reader.feed(rpc_request(2, "hook/run", {
                "hook": "cache_warming_decision", "payload": PAYLOAD,
            }))
            response = host.writer.wait_for(lambda message: message.get("id") == 2)
            self.assertEqual(response["error"], {"code": -32603, "message": "internal error"})
            self.assertNotIn("secret-provider-message", diagnostics.getvalue())
        finally:
            host.shutdown()

    def test_hook_uses_ordinary_cooperative_cancellation(self):
        extension = Extension(api_version="0.4", stderr=io.StringIO())
        started = threading.Event()

        @extension.cache_warming_decision
        def advise(payload):
            started.set()
            extension.cancellation.wait(2.0)
            extension.cancellation.raise_if_cancelled()
            return "warm"

        host = RunningExtension(extension)
        host.start(initialize_v04())
        try:
            host.reader.feed(rpc_request(2, "hook/run", {
                "hook": "cache_warming_decision", "payload": PAYLOAD,
            }))
            self.assertTrue(started.wait(1.0))
            host.reader.feed({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 2}})
            response = host.writer.wait_for(lambda message: message.get("id") == 2)
            self.assertEqual(response["error"]["code"], -32800)
            self.assertNotIn("result", response)
        finally:
            host.shutdown()


if __name__ == "__main__":
    unittest.main()
