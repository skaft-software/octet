"""Deterministic private-UI and MRTR tests; no browsers/accounts/model input."""
from __future__ import annotations

from dataclasses import FrozenInstanceError
import json
import threading
import time
import unittest
from unittest import mock

from octet_mcp import interactions as interactions
from octet_mcp.interactions import make_interaction_handler, run_operation, validate_form_schema
from octet_mcp.protocol import McpError, McpTransportError
from .helpers import FakeCancellation

OWNER = {"session_id": "host-owner", "extension_instance_id": "host-instance", "process_generation": 1}
FORM = {"message": "Which display name?", "requestedSchema": {
    "type": "object", "properties": {"name": {"type": "string", "maxLength": 64}}, "required": ["name"],
}}
URL = {"mode": "url", "message": "Review the external interaction", "url": "https://example.invalid/manual?state=fixture"}


class PrivateUI:
    api_version = "0.2"

    def __init__(self, *answers, confirmed=True):
        self.answers = iter(answers)
        self.confirmed = confirmed
        self.inputs = []
        self.confirms = []

    @property
    def request_id(self):
        raise AssertionError("must never borrow a callback thread's ambient parent")

    def request_input(self, prompt, **kwargs):
        self.inputs.append((prompt, kwargs, threading.get_ident()))
        return next(self.answers)

    def confirm(self, prompt, **kwargs):
        self.confirms.append((prompt, kwargs, threading.get_ident()))
        return self.confirmed


def handler_for(ui, *, owner=None, token=None, deadline=None, active=None, private_ui=True):
    return make_interaction_handler(
        ui, owner=OWNER.copy() if owner is None else owner, parent_request_id=77,
        cancellation=token, deadline=time.monotonic() + 2 if deadline is None else deadline,
        is_active=active or (lambda owner, parent: owner == OWNER and parent == 77),
        server_label="Reviewed fixture server", private_ui=private_ui,
    )


def invoke(ui, params=FORM, **kwargs):
    handler = handler_for(ui, **kwargs)
    with handler.operation("tools/call", deadline=handler.deadline, cancellation=handler.cancellation):
        return handler("elicitation/create", params)


def needs_input(params=FORM, **kwargs):
    return {"resultType": "input_required", "inputRequests": {
        "server-input": {"method": "elicitation/create", "params": params},
    }, **kwargs}


class ElicitationTests(unittest.TestCase):
    def test_form_accept_review_modify_and_explicit_parent_on_callback_threads(self):
        ui = PrivateUI('{"name":"initial"}', '{"name":"modified"}', 'accept')
        self.assertEqual(invoke(ui), {"action": "accept", "content": {"name": "modified"}})
        self.assertEqual(len(ui.inputs), 3)
        for _, kwargs, thread in ui.inputs:
            self.assertEqual(kwargs, {"parent_request_id": 77, "secret": True})
            self.assertNotEqual(thread, threading.get_ident())
        prompt, kwargs, _ = ui.confirms[0]
        self.assertIs(kwargs["default"], False)
        self.assertEqual(kwargs["parent_request_id"], 77)
        self.assertNotIn("modified", prompt + kwargs["detail"])

    def test_form_decline_cancel_confirmation_denial(self):
        for answers, confirmed, expected in [
            (("decline",), True, "decline"), ((None,), True, "cancel"),
            (('{"name":"fixture"}', "cancel"), True, "cancel"),
            (('{"name":"fixture"}', "accept"), False, "decline"),
        ]:
            with self.subTest(answers=answers):
                self.assertEqual(invoke(PrivateUI(*answers, confirmed=confirmed)), {"action": expected})

    def test_invalid_answers_never_shared(self):
        for answer in ['{}', '{"name":1}', '{"name":"x","extra":"y"}',
                       '{"name":"x","name":"y"}', 'not JSON', 'x' * 5000]:
            with self.subTest(answer=answer[:30]):
                ui = PrivateUI(answer)
                self.assertEqual(invoke(ui), {"action": "decline"})
                self.assertEqual(ui.confirms, [])

    def test_flat_primitives_formats_and_single_select_enums(self):
        props = {
            "name": {"type": "string", "minLength": 1},
            "age": {"type": "integer", "minimum": 18, "maximum": 100},
            "amount": {"type": "number", "minimum": 0}, "enabled": {"type": "boolean"},
            "color": {"type": "string", "oneOf": [{"const": "R", "title": "Red"}]},
            "other": {"type": "string", "enum": ["B"], "enumNames": ["Blue"]},
            "mail": {"type": "string", "format": "email"},
            "date": {"type": "string", "format": "date"},
            "time": {"type": "string", "format": "date-time"},
        }
        answer = {"name": "fixture", "age": 30, "amount": 2.5, "enabled": True, "color": "R", "other": "B",
                  "mail": "test@example.invalid", "date": "2026-01-01", "time": "2026-01-01T00:00:00Z"}
        params = {"message": "Select preferences", "requestedSchema": {"type": "object", "properties": props}}
        self.assertEqual(invoke(PrivateUI(json.dumps(answer), "accept"), params)["content"], answer)
        for field, invalid in [("age", True), ("enabled", 1), ("color", "blue"), ("date", "bad"), ("mail", "bad")]:
            self.assertEqual(invoke(PrivateUI(json.dumps({**answer, field: invalid})), params), {"action": "decline"})

    def test_unsupported_schema_and_sensitive_credentials_fail_before_prompt(self):
        cases = [
            {"type": "object"}, {"type": "array", "items": {"type": "string"}},
            {"type": "string", "pattern": ".*"}, {"type": "string", "format": "password"},
            {"type": "string", "format": {}}, {"type": "string", "enum": None},
            {"type": "string", "oneOf": [{"type": "string"}]},
            {"type": "string", "description": "Enter an API key"},
            {"type": "number", "minimum": True}, {"type": "string", "maxLength": -1},
        ]
        for spec in cases:
            with self.subTest(spec=spec):
                params = {"message": "Select a value", "requestedSchema": {"type": "object", "properties": {"choice": spec}}}
                ui = PrivateUI()
                self.assertEqual(invoke(ui, params), {"action": "decline"})
                self.assertEqual(ui.inputs, [])
        for name in ("password", "clientSecret", "accessToken", "card_number", "seedPhrase", "token", "api_token", "otp", "authCode"):
            params = {"message": "Supply details", "requestedSchema": {"type": "object", "properties": {name: {"type": "string"}}}}
            self.assertEqual(invoke(PrivateUI(), params), {"action": "decline"})
        for schema in [{**FORM["requestedSchema"], "$ref": "#"}, {**FORM["requestedSchema"], "$schema": {}},
                       {**FORM["requestedSchema"], "additionalProperties": True}]:
            with self.assertRaises(McpError):
                validate_form_schema(schema)

    def test_url_only_private_input_no_navigation_and_explicit_three_way_choice(self):
        for action, expected in [("accept", "accept"), ("decline", "decline"), ("cancel", "cancel"), (None, "cancel"), ("yes", "cancel")]:
            ui = PrivateUI(action)
            with mock.patch("urllib.request.urlopen", side_effect=AssertionError("no fetch")), mock.patch("webbrowser.open", side_effect=AssertionError("no open")):
                self.assertEqual(invoke(ui, URL), {"action": expected})
            prompt, kwargs, _ = ui.inputs[0]
            self.assertIn(URL["url"], prompt)
            self.assertIn("HTTPS destination host: example.invalid", prompt)
            self.assertIs(kwargs["secret"], True)
            self.assertEqual(ui.confirms, [])
        for url in ["http://example.invalid", "file:///tmp/file", "https://user:pass@example.invalid", "https://example.invalid/\n", "https://example.invalid:bad"]:
            ui = PrivateUI()
            self.assertEqual(invoke(ui, {**URL, "url": url}), {"action": "decline"})
            self.assertEqual(ui.inputs, [])

    def test_owner_parent_deadline_immutable_and_no_unsolicited_or_reused_handler(self):
        owner = OWNER.copy()
        ui = PrivateUI("decline")
        handler = handler_for(ui, owner=owner)
        owner["session_id"] = "foreign"
        self.assertEqual(handler.owner[0], OWNER["session_id"])
        with self.assertRaises(FrozenInstanceError):
            handler.parent_request_id = 90
        self.assertEqual(handler("elicitation/create", FORM), {"action": "cancel"})
        with handler.operation("tools/call", deadline=handler.deadline, cancellation=None):
            self.assertEqual(handler("elicitation/create", FORM), {"action": "decline"})
        with self.assertRaises(McpError):
            with handler.operation("tools/call", deadline=handler.deadline, cancellation=None):
                pass
        self.assertEqual(len(ui.inputs), 1)
        for h in (handler_for(PrivateUI(), active=lambda o, p: False), handler_for(PrivateUI(), owner={**OWNER, "session_id": "foreign"})):
            with self.assertRaises(McpError):
                with h.operation("tools/call", deadline=h.deadline, cancellation=None):
                    pass

    def test_headless_missing_owner_and_unsupported_host_never_create_handler(self):
        self.assertIsNone(handler_for(PrivateUI(), private_ui=False))
        self.assertIsNone(handler_for(PrivateUI(), owner={}))
        ui = PrivateUI()
        ui.api_version = "0.1"
        self.assertIsNone(handler_for(ui))

    def test_timeout_and_cancellation_drop_late_callback_without_sharing(self):
        for cancelled in (False, True):
            release = threading.Event()
            entered = threading.Event()
            token = FakeCancellation()
            ui = PrivateUI()
            def blocked(prompt, **kwargs):
                entered.set()
                release.wait(1)
                return '{"name":"late private answer"}'
            ui.request_input = blocked
            handler = handler_for(ui, token=token, deadline=time.monotonic() + 2)
            timer = threading.Timer(.05, token.cancel) if cancelled else None
            if timer:
                timer.start()
            started = time.monotonic()
            try:
                with handler.operation("tools/call", deadline=started + .12, cancellation=token):
                    self.assertEqual(handler("elicitation/create", FORM), {"action": "cancel"})
                self.assertTrue(entered.is_set())
                self.assertLess(time.monotonic() - started, .4)
            finally:
                release.set()
                if timer:
                    timer.join()
            self.assertEqual(ui.confirms, [])

    def test_callback_fanout_slots_remain_bounded_when_ui_stalls(self):
        semaphore = threading.BoundedSemaphore(1)
        release = threading.Event()
        entered = threading.Event()
        ui = PrivateUI()
        def blocked(*args, **kwargs):
            entered.set()
            release.wait(1)
        ui.request_input = blocked
        with mock.patch.object(interactions, "_PRIVATE_CALLBACK_SLOTS", semaphore):
            h1 = handler_for(ui, deadline=time.monotonic() + .15)
            results = []
            def first():
                with h1.operation("tools/call", deadline=h1.deadline, cancellation=None):
                    results.append(h1("elicitation/create", FORM))
            thread = threading.Thread(target=first)
            thread.start()
            self.assertTrue(entered.wait(.5))
            second = PrivateUI("accept")
            self.assertEqual(invoke(second, URL), {"action": "cancel"})
            self.assertEqual(second.inputs, [])
            thread.join(.5)
            self.assertEqual(results, [{"action": "cancel"}])
            release.set()
            self.assertTrue(semaphore.acquire(timeout=.5))
            semaphore.release()


class MrtrTests(unittest.TestCase):
    def drive(self, results, *, ui=None, method="tools/call", token=None, params=None):
        calls = []
        handler = handler_for(ui, token=token) if ui else None
        responses = iter(results)
        def send(method, params, **kwargs):
            calls.append((method, params, kwargs))
            response = next(responses)
            if isinstance(response, Exception):
                raise response
            return response
        result = run_operation(send, method, params or {"name": "fixture", "arguments": {"input": "original"}},
                               handler=handler, cancellation=token, deadline=time.monotonic() + 1)
        return result, calls

    def test_form_mrtr_exact_opaque_state_and_original_arguments_new_send(self):
        state = ' \nopaque\x00{"do_not":"parse"} '
        result, calls = self.drive([needs_input(requestState=state), {"resultType": "complete", "content": [{"type": "text", "text": "done"}]}],
                                   ui=PrivateUI('{"name":"answer-only-on-wire"}', "accept"))
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0][1]["arguments"], calls[1][1]["arguments"])
        self.assertEqual(calls[1][1]["requestState"], state)
        self.assertEqual(calls[1][1]["inputResponses"]["server-input"], {"action": "accept", "content": {"name": "answer-only-on-wire"}})
        self.assertEqual(calls[0][1]["_meta"][interactions.META_CLIENT_CAPABILITIES], {"elicitation": {"form": {}, "url": {}}})
        self.assertNotIn("answer-only-on-wire", json.dumps(result))
        self.assertEqual(calls[0][2]["_deadline"], calls[1][2]["_deadline"])

    def test_state_only_empty_state_and_absent_state_does_not_inherit_prior_round(self):
        _, calls = self.drive([
            {"resultType": "input_required", "requestState": ""},
            {"resultType": "input_required", "inputRequests": {}},
            {"contents": [{"uri": "fixture://resource", "text": "complete"}]},
        ], method="resources/read", params={"uri": "fixture://resource"})
        self.assertEqual(calls[1][1]["requestState"], "")
        self.assertNotIn("requestState", calls[2][1])
        self.assertEqual(calls[2][1]["inputResponses"], {})
        self.assertEqual(calls[0][1]["_meta"][interactions.META_CLIENT_CAPABILITIES], {})

    def test_decline_cancel_are_explicit_input_responses_not_empty_success(self):
        for action in ("decline", "cancel"):
            _, calls = self.drive([needs_input(), {"content": [{"type": "text", "text": "server handled choice"}]}], ui=PrivateUI(action))
            self.assertEqual(calls[1][1]["inputResponses"]["server-input"], {"action": action})

    def test_private_form_and_url_echoes_are_withheld_from_terminal_content(self):
        for params, ui, echo in [(FORM, PrivateUI('{"name":"private-value"}', "accept"), "private-value"),
                                 (URL, PrivateUI("accept"), URL["url"])]:
            result, _ = self.drive([needs_input(params), {"content": [{"type": "text", "text": "echo: " + echo}], "structuredContent": {"value": echo}}], ui=ui)
            self.assertNotIn(echo, json.dumps(result))

    def test_json_escaped_answer_echo_is_private_too(self):
        answer = 'private"unicode-é'
        ui = PrivateUI(json.dumps({"name": answer}), "accept")
        result, _ = self.drive([needs_input(), {"content": [{"type": "text", "text": json.dumps({"value": answer})}]}], ui=ui)
        self.assertNotIn("private", result["content"][0]["text"].replace("[private input]", ""))

    def test_transport_errors_never_replay_even_after_successful_input_required(self):
        for first in (True, False):
            calls = []
            def send(method, params, **kwargs):
                calls.append(params)
                if not first and len(calls) == 1:
                    return {"resultType": "input_required", "requestState": "opaque"}
                raise McpTransportError("lost", "ambiguous fixture loss", ambiguous=True)
            with self.assertRaises(McpTransportError):
                run_operation(send, "tools/call", {}, deadline=time.monotonic() + 1)
            self.assertEqual(len(calls), 1 if first else 2)

    def test_invalid_result_types_methods_and_unsolicited_inputs_fail_closed(self):
        cases = [None, {"resultType": "future"}, {"resultType": "input_required"},
                 {"resultType": "input_required", "requestState": {}}, needs_input(),
                 {"resultType": "complete", "inputRequests": {}}, {"resultType": "complete"}]
        for response in cases:
            with self.subTest(response=response), self.assertRaises(McpError):
                self.drive([response])
        with self.assertRaises(McpError):
            self.drive([{"resultType": "input_required", "requestState": "opaque"}], method="tools/list")

    def test_fanout_unknown_methods_and_round_byte_bounds(self):
        ui = PrivateUI()
        valid = needs_input()["inputRequests"]["server-input"]
        for requests in ({str(i): valid for i in range(5)}, {"first": valid, "second": {"method": "sampling/createMessage", "params": {}}}):
            with self.assertRaises(McpError):
                self.drive([{"resultType": "input_required", "inputRequests": requests}], ui=ui)
            self.assertEqual(ui.inputs, [])
        with self.assertRaises(McpError) as error:
            self.drive([{"resultType": "input_required", "requestState": "state"}] * 5)
        self.assertEqual(error.exception.code, "interaction_round_limit")
        with mock.patch.object(interactions, "MAX_OPERATION_BYTES", 400), self.assertRaises(McpError):
            self.drive([{"resultType": "input_required", "requestState": "x" * 200}] * 3)

    def test_cancelled_or_stale_owner_never_sends_continuation(self):
        for stale in (False, True):
            active = [True]
            token = FakeCancellation()
            ui = PrivateUI("accept")
            handler = handler_for(ui, token=token, active=lambda o, p: active[0])
            calls = []
            def send(method, params, **kwargs):
                calls.append(params)
                if stale:
                    active[0] = False
                else:
                    token.cancel()
                return needs_input(URL)
            with self.assertRaises(McpError):
                run_operation(send, "tools/call", {}, handler=handler, cancellation=token, deadline=time.monotonic() + 1)
            self.assertEqual(len(calls), 1)
            self.assertEqual(ui.inputs, [])

    def test_concurrent_calls_never_share_state_or_mutated_original_arguments(self):
        barrier = threading.Barrier(2)
        results = []
        def run(name):
            calls = []
            def send(method, params, **kwargs):
                calls.append(params)
                if len(calls) == 1:
                    params["arguments"]["name"] = "transport-mutated"
                    barrier.wait(1)
                    return {"resultType": "input_required", "requestState": name}
                self.assertEqual(params["requestState"], name)
                self.assertEqual(params["arguments"], {"name": name})
                return {"content": [{"type": "text", "text": name}]}
            results.append(run_operation(send, "tools/call", {"arguments": {"name": name}}, deadline=time.monotonic() + 1))
        threads = [threading.Thread(target=run, args=(name,)) for name in ("first", "second")]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(2)
        self.assertEqual(len(results), 2)
