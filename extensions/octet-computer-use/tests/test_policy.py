"""Exact local policy adapter regressions; no host/UI/native authority."""
import threading
import unittest
from dataclasses import replace

from octet_computer_use.policy import (
    Capability, Decision, Effect, PolicyDenied, PolicyError, PolicyGate, Scope,
    TargetIdentity, make_action_request,
)


class Evaluator:
    def __init__(self):
        self.calls = []
        self.result = {"decision": "allow"}
        self.callback = None

    def evaluate_action(self, action, **context):
        self.calls.append((action, context))
        if self.callback:
            self.callback(action)
        return self.result


def scope(**changes):
    value = Scope("scope", "session", "test", 1,
                  TargetIdentity("desktop", "com.example.safe", "7"),
                  frozenset(item.value for item in Capability),
                  frozenset(item.value for item in Effect), 100000, 100)
    return replace(value, **changes)


def action(**changes):
    value = make_action_request(operation="click", target=scope().target,
        session_id="session", owner_id="test", extension_generation=1,
        frame_generation=1, arguments={"x": 70, "y": 50}, scope="scope",
        observation_digest="a" * 64, native_identity_digest="b" * 64)
    return replace(value, **changes)


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.now = 1
        self.evaluator = Evaluator()
        self.gate = PolicyGate(self.evaluator, scope=scope(), clock_ms=lambda: self.now)

    def evaluate(self, selected=None, **kwargs):
        return self.gate.evaluate(selected or action(), parent_request_id=7, **kwargs)

    def test_missing_scope_or_legacy_adapter_denies(self):
        self.assertEqual(PolicyGate(self.evaluator).evaluate(action(), parent_request_id=1).decision, Decision.DENY)
        class Legacy:
            def evaluate(self, *args, **kwargs):
                raise AssertionError("legacy intent must never be sent")
        gate = PolicyGate(Legacy(), scope=scope(), clock_ms=lambda: 1)
        self.assertEqual(gate.evaluate(action(), parent_request_id=1).decision, Decision.DENY)

    def test_full_private_binding_without_argument_payload(self):
        self.assertEqual(self.evaluate().decision, Decision.ALLOW)
        binding, context = self.evaluator.calls[0]
        self.assertEqual(binding, action().to_binding_wire())
        self.assertEqual(context["parent_request_id"], 7)
        self.assertNotIn("arguments", binding)
        self.assertEqual(binding["observation_digest"], "a" * 64)

    def test_one_use_and_parent_binding(self):
        decision = self.evaluate()
        self.gate.authorize(action(), decision, parent_request_id=7)
        with self.assertRaises(PolicyDenied):
            self.gate.authorize(action(), decision, parent_request_id=7)
        decision = self.evaluate()
        with self.assertRaises(PolicyDenied):
            self.gate.authorize(action(), decision, parent_request_id=8)
        with self.assertRaises(PolicyDenied):
            self.gate.authorize(action(), decision, parent_request_id=7)

    def test_other_gate_cannot_redeem_or_mint_grant(self):
        decision = self.evaluate()
        other = PolicyGate(self.evaluator, scope=scope(), clock_ms=lambda: 1)
        with self.assertRaises(PolicyDenied):
            other.authorize(action(), decision, parent_request_id=7)
        fabricated = replace(decision, authorization=replace(decision.authorization))
        with self.assertRaises(PolicyDenied):
            self.gate.authorize(action(), fabricated, parent_request_id=7)

    def test_all_binding_fields_checked(self):
        variants = [action(frame_generation=2), action(arguments_digest="c" * 64),
            action(observation_digest="c" * 64), action(native_identity_digest="c" * 64),
            action(data_classes=frozenset({"private"})), action(destination="https://example.org")]
        for changed in variants:
            with self.subTest(changed=changed):
                with self.assertRaises(PolicyDenied):
                    self.gate.authorize(changed, self.evaluate(), parent_request_id=7)

    def test_scope_expiry_owner_generation_target_and_budget(self):
        for changed in [action(owner_id="foreign"), action(extension_generation=2),
                        action(target=TargetIdentity("desktop", "com.example.other", "7")),
                        action(scope="other"), action(session_id="foreign")]:
            self.assertEqual(self.evaluate(changed).decision, Decision.DENY)
        self.assertEqual(self.evaluator.calls, [])
        self.gate = PolicyGate(self.evaluator, scope=scope(max_actions=1), clock_ms=lambda: self.now)
        self.gate.authorize(action(), self.evaluate(), parent_request_id=7)
        self.assertEqual(self.evaluate().decision, Decision.DENY)
        self.now = 100000
        self.assertEqual(self.evaluate().decision, Decision.DENY)

    def test_expiry_includes_time_spent_in_approval(self):
        self.evaluator.callback = lambda _: setattr(self, "now", 5001)
        decision = self.evaluate()
        with self.assertRaises(PolicyDenied):
            self.gate.authorize(action(), decision, parent_request_id=7)

    def test_ask_token_is_exact_expiring_and_one_use(self):
        token = "f" * 64
        self.evaluator.result = {"decision": "ask", "approval_token": token}
        self.assertEqual(self.evaluate().decision, Decision.ASK)
        self.evaluator.result = {"decision": "allow"}
        self.assertEqual(self.evaluate(action(frame_generation=2), approval_token=token).decision, Decision.DENY)
        self.assertEqual(self.evaluate(approval_token=token).decision, Decision.DENY)
        self.assertEqual(len(self.evaluator.calls), 1)

    def test_matching_ask_retry(self):
        token = "e" * 64
        self.evaluator.result = {"decision": "ask", "approval_token": token}
        self.evaluate()
        self.evaluator.result = {"decision": "allow"}
        decision = self.evaluate(approval_token=token)
        self.gate.authorize(action(), decision, parent_request_id=7)
        self.assertEqual(self.evaluate(approval_token=token).decision, Decision.DENY)

    def test_manual_credentials_and_missing_private_evidence(self):
        for selected in [action(data_classes=frozenset({"credentials"})),
                         action(observation_digest=None), action(native_identity_digest=None)]:
            self.assertEqual(self.evaluate(selected).decision, Decision.DENY)
        self.assertEqual(self.evaluator.calls, [])

    def test_classification_cannot_be_spoofed(self):
        with self.assertRaises(PolicyError):
            action(effect="observation")
        with self.assertRaises(PolicyError):
            action(operation="shell.click")

    def test_bad_host_responses_fail_closed(self):
        for result in [None, True, {}, {"decision": "allow", "approval_token": "a" * 64},
                       {"decision": "allow", "scope": "global"}]:
            self.evaluator.result = result
            self.assertEqual(self.evaluate().decision, Decision.DENY)

    def test_stop_wins_pending_host_decision(self):
        entered, release = threading.Event(), threading.Event()
        def callback(_):
            entered.set()
            self.assertTrue(release.wait(1))
        self.evaluator.callback = callback
        decisions = []
        worker = threading.Thread(target=lambda: decisions.append(self.evaluate()))
        worker.start()
        self.assertTrue(entered.wait(1))
        self.gate.revoke()
        release.set()
        worker.join(1)
        self.assertFalse(worker.is_alive())
        self.assertEqual(decisions[0].decision, Decision.DENY)

    def test_concurrent_redemption_dispatches_at_most_once(self):
        decision = self.evaluate()
        outcomes = []
        def redeem():
            try:
                self.gate.authorize(action(), decision, parent_request_id=7)
                outcomes.append("allow")
            except PolicyDenied:
                outcomes.append("deny")
        workers = [threading.Thread(target=redeem) for _ in range(8)]
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join(1)
        self.assertEqual(outcomes.count("allow"), 1)

    def test_browser_origin_is_exact_and_not_interchangeable_with_desktop(self):
        browser = TargetIdentity("browser", "browser", "window", "https://example.org:443")
        selected = scope(target=browser)
        gate = PolicyGate(self.evaluator, scope=selected, clock_ms=lambda: 1)
        original = action(target=browser)
        self.assertEqual(gate.evaluate(original, parent_request_id=1).decision, Decision.ALLOW)
        changed = action(target=replace(browser, origin="https://elsewhere.example"))
        self.assertEqual(gate.evaluate(changed, parent_request_id=1).decision, Decision.DENY)
        changed = action(target=TargetIdentity("desktop", "browser", "window"))
        self.assertEqual(gate.evaluate(changed, parent_request_id=1).decision, Decision.DENY)
