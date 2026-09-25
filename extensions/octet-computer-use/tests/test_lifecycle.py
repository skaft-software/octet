"""Lifecycle safety tests with value-only adapters; no native objects."""
import unittest
from unittest.mock import Mock

from octet_computer_use.lifecycle import (
    ActionSpec, CancellationToken, LifecycleError, LifecycleSession, OwnerIdentity,
    PolicyDecision, TargetIdentity, TargetObservation,
)


OWNER = OwnerIdentity("session", "fixture", 1)
TARGET = TargetIdentity("com.example.safe", "1", frame_generation=1)
OBSERVATION = TargetObservation(OWNER, TARGET, "observation-1", 1,
    100, 100, 1, (1, 0, 0, 1, 0, 0), "a" * 64, structured={"fixture": True})


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.adapter = Mock(observe=Mock(return_value=OBSERVATION),
                            release_input=Mock(return_value=True), spec=["observe", "release_input", "perform_action"])
        self.session = LifecycleSession(OWNER, TARGET, self.adapter)

    def test_default_policy_does_not_act(self):
        self.session.observe(OWNER)
        action = ActionSpec("click", TARGET, OBSERVATION.observation_id, {"x": 1, "y": 1})
        result = self.session.execute_group(OWNER, (action,))
        self.assertEqual(result.status, "denied")
        self.adapter.perform_action.assert_not_called()

    def test_stop_cannot_be_reopened_by_late_observation(self):
        def stop_then_complete(*args, **kwargs):
            self.session.settle(OWNER, reason="fixture-stop")
            return OBSERVATION
        self.adapter.observe.side_effect = stop_then_complete
        with self.assertRaises(LifecycleError):
            self.session.observe(OWNER)
        self.assertIsNone(self.session.last_observation)
        self.assertEqual(len(self.session._history), 0)
        self.assertTrue(self.session.settled)

    def test_non_boolean_cleanup_does_not_claim_release(self):
        for result in (None, False, {"released": True}, "true", 1):
            with self.subTest(result=result):
                self.adapter.release_input.return_value = result
                session = LifecycleSession(OWNER, TARGET, self.adapter)
                self.assertEqual(session.stop(OWNER)["state"], "degraded")

    def test_truthy_approval_objects_do_not_allow(self):
        self.session.approval = lambda *args, **kwargs: {"approved": False}
        decision = PolicyDecision.require_approval("fixture")
        action = ActionSpec("click", TARGET, OBSERVATION.observation_id, {"x": 1, "y": 1})
        self.assertFalse(self.session._approval_decision(action, decision,
            token=CancellationToken(), timeout=1))

    def test_foreign_owner_stop_does_not_release_current_input(self):
        with self.assertRaises(LifecycleError):
            self.session.stop(OwnerIdentity("other", "fixture", 1))
        self.adapter.release_input.assert_not_called()
