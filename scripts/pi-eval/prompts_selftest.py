#!/usr/bin/env python3
"""Credential-free unit checks for the real-CLI prompt comparison oracle."""
from __future__ import annotations

from copy import deepcopy
from types import SimpleNamespace
import unittest

import prompts


def observation(user=prompts.PROMPT, system=prompts.BASE, tools=None):
    return {"messages": [{"role": "system", "text": system},
                         {"role": "user", "text": user}], "tools": tools or []}


class PromptOracleTests(unittest.TestCase):
    def test_missing_startup_is_not_a_pass(self):
        self.assertTrue(prompts.check("startup", [observation()]))
        self.assertEqual(prompts.check("startup", [observation(system=prompts.BASE + "\nPARITY_STARTED")]), [])

    def test_dropped_input_transform_is_a_failure(self):
        self.assertTrue(prompts.check("input-transform", [observation()]))
        transformed = observation(user=prompts.PROMPT + "\nPARITY_TRANSFORMED")
        self.assertEqual(prompts.check("input-transform", [transformed]), [])

    def test_chain_is_ordered_and_visible_to_before_agent_start(self):
        user = prompts.PROMPT + "\nPARITY_FIRST\nPARITY_SECOND"
        self.assertTrue(prompts.check("input-chain", [observation(user=user)]))
        self.assertEqual(prompts.check("input-chain", [observation(user=user, system=prompts.BASE + "\n" + user)]), [])

    def test_handled_input_requires_no_provider_io(self):
        self.assertEqual(prompts.check("input-handled", []), [])
        self.assertTrue(prompts.check("input-handled", [observation()]))
        self.assertTrue(prompts.check("startup", []))

    def test_fleet_cannot_be_truncated(self):
        self.assertTrue(prompts.check("large-fleet", [observation(tools=[{}] * 116)]))
        self.assertEqual(prompts.check("large-fleet", [observation(tools=[{}] * 117)]), [])

    def test_tool_roundtrip_requires_the_actual_result(self):
        first = observation()
        second = observation()
        self.assertTrue(prompts.check("tool-roundtrip", [first]))
        self.assertTrue(prompts.check("tool-roundtrip", [first, second]))
        second["messages"].append({"role": "tool", "text": "PARITY_TOOL_RESULT:payload"})
        self.assertEqual(prompts.check("tool-roundtrip", [first, second]), [])

    def test_only_default_false_strictness_is_normalized_without_mutating_wire(self):
        function = {"name": "parity_echo", "description": "echo", "parameters": {"type": "object"}}
        raw = {"tools": [{"type": "function", "function": function}]}
        original = deepcopy(raw)
        explicit = deepcopy(raw)
        explicit["tools"][0]["function"]["strict"] = False
        observe = lambda body: prompts.observe([SimpleNamespace(body=body)])
        self.assertEqual(observe(raw), observe(explicit))
        self.assertEqual(raw, original)
        explicit["tools"][0]["function"]["strict"] = True
        self.assertNotEqual(observe(raw), observe(explicit))
        explicit["tools"][0]["function"]["strict"] = False
        explicit["tools"][0]["function"]["parameters"]["additionalProperties"] = False
        self.assertNotEqual(observe(raw), observe(explicit))

    def test_assistant_tool_names_and_arguments_remain_part_of_comparison(self):
        body = {"messages": [{"role": "assistant", "tool_calls": [
            {"id": "generated", "type": "function", "function": {
                "name": "parity_echo", "arguments": '{"value": "payload"}'}}]}]}
        observed = prompts.observe([SimpleNamespace(body=body)])
        self.assertEqual(observed[0]["messages"][0]["tool_calls"], [
            {"name": "parity_echo", "arguments": {"value": "payload"}}])
        changed = deepcopy(body)
        changed["messages"][0]["tool_calls"][0]["function"]["arguments"] = '{"value":"wrong"}'
        self.assertNotEqual(observed, prompts.observe([SimpleNamespace(body=changed)]))


if __name__ == "__main__":
    unittest.main()
