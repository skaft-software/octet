"""Contract tests for the optional Jev action chooser.

The security-relevant behavior is what matters here: the request carries only
allow-listed fields, an out-of-set answer fails closed, and the API key never
appears in a result or an error. These use a stubbed chooser, so no key and no
network are required.
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from octet_computer_use import jev
from octet_computer_use.jev import (
    ABSTAIN,
    REOBSERVE,
    Candidate,
    Choice,
    JevContractError,
    JevUnavailable,
    build_request,
    choose_action,
    status,
)


class _StubAnswer:
    def __init__(self, identifier, confidence, probabilities):
        self.choice = identifier
        self.confidence = confidence
        self.probabilities = probabilities


class _StubResponse:
    def __init__(self, answer):
        self.answers = {"next_action": answer}


class _StubClient:
    def __init__(self, answer):
        self._answer = answer
        self.seen = None

    def system_one(self, state, questions):
        self.seen = (state, questions)
        return _StubResponse(self._answer)


def _stub(answer):
    client = _StubClient(answer)
    jev._client = lambda key, model=None: client
    return client


class JevRequestShapeTests(unittest.TestCase):
    def test_request_only_carries_allow_listed_fields(self):
        request = build_request(
            goal="open the calculator",
            candidates=[Candidate("calc", "Open the Calculator app")],
            capture_id="cap-1",
            regions=[{"id": "b1", "role": "button", "label": "7"}],
            history=["typed 6"],
        )
        self.assertLessEqual(
            set(request["state"]),
            {"goal", "candidates", "capture_id", "regions", "history"},
        )
        # Driver tool names and arguments must never reach the model.
        rendered = str(request)
        for forbidden in ("cua-driver", "get_window_state", "press_key", "click"):
            self.assertNotIn(forbidden, rendered)

    def test_reserved_options_are_always_offered(self):
        request = build_request(
            goal="x", candidates=[Candidate("a", "do a")]
        )
        criteria = request["state"]["candidates"]
        self.assertIn(REOBSERVE, criteria)
        self.assertIn(ABSTAIN, criteria)

    def test_caller_cannot_redefine_a_reserved_id(self):
        # A caller offering only a reserved id has offered no real action, so the
        # request is refused rather than letting the caller rename reobserve.
        with self.assertRaises(ValueError):
            build_request(goal="x", candidates=[Candidate(REOBSERVE, "hijack")])

    def test_reserved_id_among_real_candidates_keeps_its_own_description(self):
        request = build_request(
            goal="x",
            candidates=[Candidate("a", "do a"), Candidate(ABSTAIN, "hijack")],
        )
        criteria = request["state"]["candidates"]
        self.assertIn("Take no action right now", criteria[ABSTAIN])

    def test_empty_goal_is_rejected(self):
        with self.assertRaises(ValueError):
            build_request(goal="  ", candidates=[Candidate("a", "do a")])


class JevChoiceTests(unittest.TestCase):
    def setUp(self):
        self._original = jev._client

    def tearDown(self):
        jev._client = self._original

    def test_valid_choice_is_returned(self):
        _stub(_StubAnswer("calc", 0.9, {"calc": 0.9, REOBSERVE: 0.1}))
        choice = choose_action(
            goal="open calc", candidates=[Candidate("calc", "Open Calculator")], api_key="k"
        )
        self.assertEqual(choice.identifier, "calc")
        self.assertAlmostEqual(choice.confidence, 0.9)

    def test_out_of_set_answer_fails_closed(self):
        _stub(_StubAnswer("rm_rf", 0.99, {"rm_rf": 0.99}))
        with self.assertRaises(JevContractError):
            choose_action(
                goal="open calc", candidates=[Candidate("calc", "Open Calculator")], api_key="k"
            )

    def test_reserved_reobserve_is_accepted(self):
        _stub(_StubAnswer(REOBSERVE, 0.4, {REOBSERVE: 0.4}))
        choice = choose_action(
            goal="x", candidates=[Candidate("a", "do a")], api_key="k"
        )
        self.assertEqual(choice.identifier, REOBSERVE)

    def test_missing_key_is_unavailable(self):
        with self.assertRaises(JevUnavailable):
            choose_action(goal="x", candidates=[Candidate("a", "do a")], api_key="")


class JevKeyStorageTests(unittest.TestCase):
    def test_key_is_stored_private_and_round_trips(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            path = jev.store_key("ts-secret-value", home=home)
            self.assertIsNotNone(path)
            mode = path.stat().st_mode & 0o777
            self.assertEqual(mode, 0o600, "the key file must be owner-only")
            self.assertEqual(jev.resolve_key(home=home), "ts-secret-value")

    def test_environment_key_takes_precedence_over_stored(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            jev.store_key("stored-value", home=home)
            os.environ[jev.API_KEY_ENV] = "env-value"
            try:
                self.assertEqual(jev.resolve_key(home=home), "env-value")
            finally:
                os.environ.pop(jev.API_KEY_ENV, None)

    def test_empty_key_is_not_stored(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.assertIsNone(jev.store_key("   ", home=home))
            self.assertIsNone(jev.store_key("", home=home))

    def test_clear_removes_the_stored_key(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            jev.store_key("value", home=home)
            jev.clear_key(home=home)
            self.assertIsNone(jev.resolve_key(home=home))

    def test_status_reports_source_but_never_the_key(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            jev.store_key("ts-super-secret", home=home)
            report = jev.status(home=home)
            self.assertEqual(report["api_key_source"], "stored")
            self.assertNotIn("ts-super-secret", str(report))


class JevSecretHygieneTests(unittest.TestCase):
    def setUp(self):
        self._original = jev._client

    def tearDown(self):
        jev._client = self._original

    def test_key_never_appears_in_an_error(self):
        class Boom:
            def system_one(self, *args, **kwargs):
                raise RuntimeError("request failed with KEY=super-secret")

        jev._client = lambda key, model=None: Boom()
        with self.assertRaises(JevUnavailable) as caught:
            choose_action(goal="x", candidates=[Candidate("a", "do a")], api_key="super-secret")
        self.assertNotIn("super-secret", str(caught.exception))

    def test_status_reports_no_key_material(self):
        report = status()
        self.assertIn("usable", report)
        # The status payload is safe to show in the UI: it is booleans and a note.
        self.assertNotIn("api_key", report)
        self.assertNotIn("TYPESAFE_API_KEY=", str(report))


if __name__ == "__main__":
    unittest.main()
