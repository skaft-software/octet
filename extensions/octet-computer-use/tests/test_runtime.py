"""Real entrypoint/lifecycle/policy/backend composition with mocked native only."""
import io
import json
import re
import threading
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import Mock

try:  # package discovery (CI) or flat discovery (README), which differ here
    from .test_backend_macos import MockNative, TARGET, OWNER
    from .test_policy import Evaluator, scope
except ImportError:  # pragma: no cover - exercised only by flat discovery
    from test_backend_macos import MockNative, TARGET, OWNER
    from test_policy import Evaluator, scope

from main import ComputerUseExtension
from octet_computer_use.backend_macos import MacOSBackend, MacOSBackendError
from octet_computer_use.lifecycle import OwnerIdentity
from octet_computer_use.policy import PolicyDenied, PolicyGate
from octet_computer_use import protocol
from octet_computer_use.protocol import ProtocolFailure, canonical_bytes
from octet_computer_use.runtime import MacOSRuntime


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.native = MockNative()
        self.evaluator = Evaluator()
        self.gate = PolicyGate(self.evaluator, scope=scope(), clock_ms=lambda: 1)
        self.factory = Mock(side_effect=lambda: MacOSBackend(
            opt_in=True, allow_input=True, native=self.native))
        self.runtime = MacOSRuntime(enabled=True, owner=OwnerIdentity.from_value(OWNER),
            native_target=TARGET, policy_gate=self.gate, backend_factory=self.factory)
        self.server = ComputerUseExtension(io.BytesIO(), io.BytesIO(), runtime=self.runtime)
        self.context = {"session_id": "session", "owner_id": "test",
            "extension_generation": 1, "frame_generation": 1, "scope": "scope",
            "target": scope().target.to_wire(), "parent_request_id": 7}

    def call(self, operation="observe", arguments=None, event=None):
        result = self.server.handle_tool_call({"name": "computer_use",
            "arguments": {"operation": operation, "arguments": arguments or {}},
            "context": self.context}, event)
        canonical_bytes(result)  # Exercise exact API 0.3 output compatibility.
        return result

    def click_count(self):
        return sum(name == "click" for name, _ in self.native.calls)

    def test_lazy_factory_and_observe_click(self):
        self.factory.assert_not_called()
        observed = self.call()
        self.assertFalse(observed["is_error"], observed)
        self.factory.assert_called_once()
        clicked = self.call("click", {"x": 70, "y": 50})
        self.assertFalse(clicked["is_error"], clicked)
        self.assertEqual(self.click_count(), 1)
        self.assertIsNotNone(self.runtime.session.last_observation)
        binding = self.evaluator.calls[-1][0]
        self.assertIn("observation_digest", binding)
        self.assertIn("native_identity_digest", binding)
        self.assertEqual(binding["operation"], "click")

    def test_denial_before_native_factory(self):
        self.evaluator.result = {"decision": "deny"}
        self.assertTrue(self.call()["is_error"])
        self.factory.assert_not_called()
        self.assertEqual(self.native.calls, [])

    def test_unknown_action_arguments_rejected_before_policy(self):
        self.call()
        calls = len(self.evaluator.calls)
        self.assertTrue(self.call("click", {"x": 70, "y": 50, "approval": True})["is_error"])
        self.assertEqual(len(self.evaluator.calls), calls)
        self.assertEqual(self.click_count(), 0)

    def test_missing_or_stale_observation_never_dispatches(self):
        self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.factory.assert_not_called()
        self.call()
        self.context["frame_generation"] = 2
        self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.assertEqual(self.click_count(), 0)

    def test_change_during_policy_revalidation_prevents_input(self):
        self.call()
        def changed(action):
            if action["operation"] == "click":
                self.native.window = replace(self.native.window, title="Changed")
        self.evaluator.callback = changed
        result = self.call("click", {"x": 70, "y": 50})
        self.assertTrue(result["is_error"], result)
        self.assertEqual(self.click_count(), 0)
        self.assertTrue(self.runtime.session.settled)

    def test_native_identity_change_before_action_never_dispatches(self):
        self.call()
        self.native.window = replace(self.native.window,
                                     identity=replace(TARGET, process_start_token="new"))
        self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.assertEqual(self.click_count(), 0)

    def test_credentials_remain_manual_even_when_evaluator_allows(self):
        self.call()
        self.context["trusted_data_classes"] = ["credentials"]
        self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.assertEqual(self.click_count(), 0)

    def test_native_sensitive_control_is_not_host_approval(self):
        self.native.tree = replace(self.native.tree,
            nodes=(replace(self.native.tree.nodes[0], sensitive=True),))
        self.call()
        self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.assertEqual(self.click_count(), 0)

    def test_cancellation_during_policy_stops_and_invalidates(self):
        self.call()
        event = threading.Event()
        self.evaluator.callback = lambda _: event.set()
        self.assertTrue(self.call("click", {"x": 70, "y": 50}, event)["is_error"])
        self.assertEqual(self.click_count(), 0)
        self.assertTrue(self.runtime.session.settled)
        self.assertIsNone(self.runtime.session.last_observation)
        self.assertTrue(self.call()["is_error"])

    def test_trusted_takeover_and_eof_are_terminal(self):
        self.call()
        self.runtime.takeover()
        self.assertTrue(self.runtime.session.settled)
        self.assertTrue(self.call()["is_error"])
        self.server.run()  # Empty BytesIO = process EOF, idempotent cleanup.
        self.assertIsNone(self.runtime.session.last_observation)

    def test_eof_releases_without_prior_takeover(self):
        self.call()
        self.server.run()
        self.assertTrue(self.runtime.session.settled)
        self.assertTrue(any(name == "release_all" for name, _ in self.native.calls))

    def test_lost_ack_is_unknown_and_never_replayed(self):
        self.call()
        def lost_ack(*args, **kwargs):
            self.native.calls.append(("click", None))
            raise OSError("private backend detail")
        self.native.click = lost_ack
        result = self.call("click", {"x": 70, "y": 50})
        self.assertTrue(result["is_error"], result)
        self.assertEqual(result["structured_content"]["result"]["status"], "unknown_effect")
        self.assertNotIn("private backend detail", repr(result))
        self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.assertEqual(self.click_count(), 1)

    def test_failed_input_release_is_degraded(self):
        self.call()
        self.native.release_all = Mock(side_effect=OSError("fixture"))
        status = self.runtime.stop()
        self.assertEqual(status["state"], "degraded")
        self.assertTrue(status["safe_fallback_required"])

    def test_unsupported_ops_and_model_authority_are_never_dispatched(self):
        for operation in ("start", "double_click", "drag", "move", "screenshot", "wait"):
            self.assertTrue(self.call(operation)["is_error"])
        for operation in ("stop", "takeover", "code"):
            with self.assertRaises(ProtocolFailure):
                self.call(operation)
        self.factory.assert_not_called()
        self.assertEqual(self.evaluator.calls, [])

    def test_foreign_host_context_rejected_before_factory(self):
        self.context["owner_id"] = "foreign"
        self.assertTrue(self.call()["is_error"])
        self.factory.assert_not_called()

    def test_standalone_missing_authority_is_inert(self):
        self.server = ComputerUseExtension(io.BytesIO(), io.BytesIO())
        self.assertTrue(self.call()["is_error"])
        self.factory.assert_not_called()

    def test_confirmation_window_change_rechecked_in_backend(self):
        backend = self.factory()
        observation = backend.observe(TARGET, owner=OWNER)
        def confirmation(_):
            self.native.window = replace(self.native.window,
                geometry=replace(self.native.window.geometry, x=100))
            return True
        with self.assertRaises(MacOSBackendError):
            backend.click(TARGET, "ax:root", owner=OWNER, observation=observation,
                          confirmation=confirmation)
        self.assertEqual(self.click_count(), 0)

    def test_backend_stop_is_terminal(self):
        backend = self.factory()
        backend.observe(TARGET, owner=OWNER)
        self.assertTrue(backend.stop(owner=OWNER)["input_released"])
        with self.assertRaises(MacOSBackendError):
            backend.observe(TARGET, owner=OWNER)

    def test_keypress_translates_only_navigation_keys(self):
        self.native.tree = replace(self.native.tree,
            nodes=(replace(self.native.tree.nodes[0], focused=True),))
        self.native.press_key = Mock()
        self.call()
        result = self.call("keypress", {"key": "ArrowLeft"})
        self.assertFalse(result["is_error"], result)
        self.assertEqual(self.native.press_key.call_args.args, ("Left",))

    def test_type_targets_one_focused_editable_control_without_echo(self):
        self.native.tree = replace(self.native.tree, nodes=(replace(self.native.tree.nodes[0],
            role="AXTextField", focused=True, editable=True),))
        self.native.set_text = Mock()
        self.call()
        result = self.call("type", {"text": "fixture-private-input"})
        self.assertFalse(result["is_error"], result)
        self.native.set_text.assert_called_once()
        self.assertNotIn("fixture-private-input", repr(result))
        self.assertNotIn("fixture-private-input", repr(self.evaluator.calls))

    def test_scroll_is_bounded_and_reobserved_not_task_verified(self):
        self.native.scroll = Mock()
        self.call()
        result = self.call("scroll", {"delta_y": 1})
        self.assertFalse(result["is_error"], result)
        self.native.scroll.assert_called_once()
        verification = result["structured_content"]["result"]["verification"]
        self.assertFalse(verification["trusted"])
        self.assertIn("has not been verified", verification["detail"])

    def test_response_loss_revokes_runtime(self):
        self.call()
        self.server.stdout = Mock(write=Mock(side_effect=BrokenPipeError()))
        with self.assertRaises(BrokenPipeError):
            self.server.send({"result": "lost"})
        self.assertTrue(self.runtime.session.settled)
        self.assertTrue(self.call()["is_error"])

    def test_malformed_host_generation_is_not_authority(self):
        self.context["frame_generation"] = True
        with self.assertRaises(ProtocolFailure):
            self.call()
        self.factory.assert_not_called()

    def test_truthy_native_cleanup_is_not_release_acknowledgement(self):
        self.call()
        self.native.release_all = Mock(return_value={"released": True})
        status = self.runtime.stop()
        self.assertEqual(status["state"], "degraded")

    def test_missing_release_ack_after_input_is_unknown_and_terminal(self):
        self.call()
        self.native.release_all = Mock(return_value=None)
        result = self.call("click", {"x": 70, "y": 50})
        self.assertTrue(result["is_error"])
        self.assertEqual(result["structured_content"]["result"]["status"], "unknown_effect")
        self.assertTrue(self.runtime.session.settled)

    def test_changed_sensitive_focus_after_confirmation_never_keys(self):
        self.native.tree = replace(self.native.tree,
            nodes=(replace(self.native.tree.nodes[0], focused=True),))
        self.native.press_key = Mock()
        backend = self.factory()
        observation = backend.observe(TARGET, owner=OWNER)
        def confirmation(_):
            self.native.tree = replace(self.native.tree,
                nodes=(replace(self.native.tree.nodes[0], sensitive=True),))
            return True
        with self.assertRaises(MacOSBackendError):
            backend.press(TARGET, "Enter", observation=observation, owner=OWNER,
                          confirmation=confirmation)
        self.native.press_key.assert_not_called()

    def test_cancelled_request_uses_api_03_terminal_error(self):
        import json
        from main import ActiveCall
        active = ActiveCall()
        active.cancelled.set()
        self.server.active[1] = active
        self.server._run_tool(1, active, {"name": "computer_use",
            "arguments": {"operation": "observe", "arguments": {}}, "context": self.context})
        response = json.loads(self.server.stdout.getvalue())
        self.assertEqual(response["error"]["code"], -32800)
        self.assertEqual(response["error"]["message"], "request cancelled")
        self.factory.assert_not_called()

    def test_expired_scope_does_not_even_recapture_native_evidence(self):
        self.call()
        before = list(self.native.calls)
        self.gate._clock_ms = lambda: 100000
        self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.assertEqual(self.native.calls, before)

    def test_grant_expiring_inside_native_revalidation_dispatches_no_input(self):
        self.call()
        now = [1]
        self.gate._clock_ms = lambda: now[0]
        tree = self.native.accessibility_tree
        captures = [0]
        def capture(*args, **kwargs):
            captures[0] += 1
            if captures[0] == 3:  # lifecycle, runtime, then final native revalidation
                now[0] = 6001
            return tree(*args, **kwargs)
        self.native.accessibility_tree = capture
        result = self.call("click", {"x": 70, "y": 50})
        self.assertTrue(result["is_error"])
        self.assertEqual(self.click_count(), 0)

    def test_unknown_or_replaced_scope_identifier_dispatches_nothing(self):
        # A host scope identifier the selected runtime never accepted is denied
        # before policy evaluation, before the native factory, and before input.
        self.context["scope"] = "replaced-scope"
        self.assertTrue(self.call()["is_error"])
        self.factory.assert_not_called()
        self.assertEqual(self.evaluator.calls, [])
        self.assertEqual(self.native.calls, [])

        # A replaced scope binding cannot be reused to compose a new runtime:
        # the owner generation in the scope must match the supplied owner.
        replaced = scope(scope_id="scope-2", extension_generation=2,
                         owner_id="test")
        with self.assertRaises(PolicyDenied):
            MacOSRuntime(enabled=True, owner=OwnerIdentity.from_value(OWNER),
                native_target=TARGET, policy_gate=PolicyGate(Evaluator(), scope=replaced),
                backend_factory=self.factory)
        with self.assertRaises(PolicyDenied):
            MacOSRuntime(enabled=True,
                owner=OwnerIdentity("session", "test", 2), native_target=TARGET,
                policy_gate=PolicyGate(Evaluator(), scope=scope()),
                backend_factory=self.factory)

    def test_stopped_binding_cannot_redeem_a_captured_grant_or_frame(self):
        observed = self.call()
        self.assertFalse(observed["is_error"], observed)
        frame = self.runtime.session.last_observation.target.frame_generation
        grants = len(self.evaluator.calls)
        self.runtime.stop("host_stop")
        self.gate._clock_ms = lambda: 1  # Stop, not expiry, is what must deny.

        for scope_id in ("scope", "replaced-scope"):
            self.context["scope"] = scope_id
            self.context["frame_generation"] = frame
            self.assertTrue(self.call("click", {"x": 70, "y": 50})["is_error"])
        self.context["scope"] = "scope"
        self.assertTrue(self.call()["is_error"])
        self.assertEqual(self.evaluator.calls.__len__(), grants)
        self.assertEqual(self.click_count(), 0)
        self.assertIsNone(self.runtime.session.last_observation)

    def test_stop_and_takeover_are_trusted_entry_points_not_tool_arguments(self):
        for field in ("stop", "takeover", "release_input"):
            hostile = dict(self.context, **{field: True})
            with self.assertRaises(ProtocolFailure):
                self.server.handle_tool_call({"name": "computer_use",
                    "arguments": {"operation": "observe", "arguments": {}},
                    "context": hostile})
        self.factory.assert_not_called()

        self.call()
        taken = self.runtime.takeover()
        self.assertTrue(taken["settled"])
        self.assertEqual(taken["settlement_reason"], "host_takeover")
        self.assertIn(taken["state"], {"paused", "degraded"})
        self.assertTrue(self.runtime.session.settled)
        self.assertTrue(any(name == "release_all" for name, _ in self.native.calls))
        self.assertTrue(self.call()["is_error"])
        self.assertEqual(self.click_count(), 0)
