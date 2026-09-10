from __future__ import annotations

import json
from pathlib import Path
import queue
import subprocess
import threading
import time
import unittest
from unittest import mock

from octet_mcp.config import BridgeConfig
from octet_mcp.runtime import build_runtime

from .helpers import FIXTURES, ROOT


class RuntimeProtocolTests(unittest.TestCase):
    def setUp(self):
        self.process = subprocess.Popen(
            [
                str(ROOT / "octet-mcp"),
                "--config",
                str(FIXTURES / "configs" / "real-local.json"),
            ],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.messages = queue.Queue()
        self.stderr = []
        self.stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self.stderr_thread = threading.Thread(target=self._read_stderr, daemon=True)
        self.stdout_thread.start()
        self.stderr_thread.start()

    def tearDown(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=3)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()
        self.stdout_thread.join(timeout=1)
        self.stderr_thread.join(timeout=1)

    def _read_stdout(self):
        assert self.process.stdout is not None
        for line in self.process.stdout:
            try:
                self.messages.put(json.loads(line))
            except json.JSONDecodeError as error:
                self.messages.put(error)

    def _read_stderr(self):
        assert self.process.stderr is not None
        for line in self.process.stderr:
            if len(self.stderr) < 128:
                self.stderr.append(line)

    def send(self, value):
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(value, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def receive(self, timeout=4):
        value = self.messages.get(timeout=timeout)
        if isinstance(value, Exception):
            raise value
        return value

    def test_api_02_dynamic_catalog_tool_call_presentation_and_shutdown(self):
        with self.assertRaises(queue.Empty):
            self.messages.get(timeout=0.15)

        self.send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "api_version": "0.2",
                    "octet_version": "0.7.0",
                    "extension": {
                        "name": "octet-mcp",
                        "version": "0.1.0",
                        "manifest_path": str(ROOT / "extension.toml"),
                        "source": "explicit",
                    },
                    "workspace": str(ROOT),
                    "capabilities": {
                        "filesystem": "unrestricted",
                        "process": True,
                        "network": False,
                    },
                    "contributes": {
                        "tools": [],
                        "commands": ["mcp"],
                        "hooks": ["before_prompt"],
                        "ui": ["status"],
                        "presentation": True,
                    },
                    "host": {
                        "session_id": "fixture-session",
                        "session_name": None,
                        "model": "fixture-model",
                        "reasoning": None,
                        "active_skills": [],
                    },
                    "protocol": {
                        "version": "0.2",
                        "required_features": ["request_cancellation", "content_parts"],
                        "optional_features": [
                            "request_progress",
                            "artifacts",
                            "policy_intents",
                            "dynamic_tools",
                            "lifecycle_events",
                        ],
                        "limits": {"max_concurrent_requests": 4},
                    },
                },
            }
        )
        initialized = self.receive()
        self.assertEqual(initialized["id"], 1)
        self.assertEqual(initialized["result"]["tools"], [])
        self.assertIn("dynamic_tools", initialized["result"]["protocol"]["features"])
        self.assertEqual(
            initialized["result"]["protocol"]["lifecycle_events"],
            ["session/settled", "session/started"],
        )
        self.send({"jsonrpc": "2.0", "method": "session/started", "params": {"session_id": "fixture-session"}})

        registered = None
        presentations = []
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and registered is None:
            message = self.receive()
            if message.get("method") == "presentation/update":
                presentations.append(message["params"]["snapshot"])
            elif message.get("method") == "tools/register":
                registered = message
        self.assertIsNotNone(registered)
        names = sorted(tool["name"] for tool in registered["params"]["tools"])
        self.assertEqual(len(names), 3)
        self.send(
            {
                "jsonrpc": "2.0",
                "id": registered["id"],
                "result": {"revision": 1, "tools": names},
            }
        )

        echo_name = next(name for name in names if "fixture_echo" in name)
        self.send(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tool/call",
                "params": {
                    "name": echo_name,
                    "arguments": {"value": "wire"},
                    "catalog_revision": 1,
                    "context": {
                        "workspace": str(ROOT),
                        "resource_owner": {
                            "session_id": "fixture-owner",
                            "extension_instance_id": "fixture-instance",
                            "process_generation": 1,
                        },
                        "host": {},
                    },
                },
            }
        )
        tool_response = None
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and tool_response is None:
            message = self.receive()
            if message.get("method") == "presentation/update":
                presentations.append(message["params"]["snapshot"])
            elif message.get("id") == 2:
                tool_response = message
        self.assertIsNotNone(tool_response)
        self.assertEqual(tool_response["result"]["structured_content"], {"echo": "wire"})
        self.assertEqual(tool_response["result"]["content"][0]["type"], "text")
        self.assertTrue(presentations)
        self.assertTrue(all(set(item) <= {"revision", "status", "activities", "collection", "actions"} for item in presentations))
        self.assertTrue(any(item.get("collection", {}).get("nodes") for item in presentations))

        self.send({"jsonrpc": "2.0", "method": "session/settled", "params": {
            "session_id": "fixture-session", "outcome": "completed", "duration_ms": 1,
        }})
        self.send({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}})
        shutdown = None
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline and shutdown is None:
            message = self.receive()
            if message.get("id") == 3:
                shutdown = message
        self.assertEqual(shutdown["result"], {})
        self.process.wait(timeout=4)
        self.assertEqual(self.process.returncode, 0, "".join(self.stderr))


class RuntimeOwnerDispatchTests(unittest.TestCase):
    def runtime(self):
        with mock.patch("octet_mcp.runtime.load_config", return_value=BridgeConfig.empty()):
            extension, manager = build_runtime()
        self.addCleanup(manager.shutdown)
        return extension, manager

    def test_session_fences_are_ordered_and_bypass_saturated_handler_admission(self):
        extension, manager = self.runtime()
        extension._features = frozenset({"lifecycle_events"})
        extension._admission = mock.Mock()
        extension._admission.acquire.return_value = False
        extension._executor = mock.Mock()
        extension._submit_notification("session/started", {"session_id": "host-id"})
        self.assertEqual(manager._remote_session_id, "host-id")
        extension._submit_notification("session/settled", {"session_id": "host-id"})
        extension._submit_notification("session/started", {"session_id": "host-id"})
        self.assertTrue(manager._remote_session_settled)
        extension._admission.acquire.assert_not_called()
        extension._executor.submit.assert_not_called()

    def test_unnegotiated_lifecycle_cannot_authorize_remote_work(self):
        extension, manager = self.runtime()
        extension._submit_notification("session/started", {"session_id": "host-id"})
        self.assertIsNone(manager._remote_session_id)

    def test_before_prompt_activates_its_host_owner_but_observational_commands_are_inert(self):
        extension, manager = self.runtime()
        owner_context = {"resource_owner": {
            "session_id": "durable-id", "extension_instance_id": "instance", "process_generation": 1,
        }, "host": {"session_id": "host-id"}}
        with mock.patch.object(manager, "activate_owner", return_value=True) as activate:
            result = extension._hooks["before_prompt"]({"prompt": "untrusted owner-b"}, owner_context)
            self.assertEqual(result, {"disposition": {"action": "continue"}})
            for arguments in ([], ["status"], ["list"], ["snapshot"], ["show", "remote"]):
                extension._commands["mcp"].handler(arguments, owner_context)
        self.assertEqual(activate.call_args_list, [mock.call(owner_context)])
        with mock.patch.object(manager, "activate_owner", side_effect=ValueError("stale")):
            self.assertEqual(extension._hooks["before_prompt"]({}, {}), result)

    def test_command_passes_host_context_without_accepting_argument_owner(self):
        extension, manager = self.runtime()
        owner_context = {"resource_owner": {
            "session_id": "durable-id", "extension_instance_id": "instance", "process_generation": 1,
        }, "host": {"session_id": "host-id"}}
        with mock.patch.object(manager, "execute_command", return_value={"text": "ok"}) as execute:
            result = extension._commands["mcp"].handler(["restart", "remote"], owner_context)
        self.assertEqual(result, {"text": "ok"})
        execute.assert_called_once_with(["restart", "remote"], owner_context)


if __name__ == "__main__":
    unittest.main()
