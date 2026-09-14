#!/usr/bin/env python3
"""Process-level checks for the API 0.3 minimal extension example."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).parent
EXTENSION = ROOT / "extension.py"

REQUIRED_CAPABILITIES = [
    "content_parts",
    "core",
    "request_cancellation",
    "tool_call",
]
OPTIONAL_CAPABILITIES = [
    "lifecycle_events",
    "migration.adapter.v1",
    "provider_auth",
    "provider_catalog",
    "provider_stream",
    "session_lifecycle",
]
REQUIRED_METHODS = ["$/cancelRequest", "initialize", "shutdown", "tool/call"]
OPTIONAL_METHODS = [
    "hook/run",
    "migration/detect",
    "migration/import",
    "provider/auth/request",
    "provider/auth/revoke",
    "provider/cancel",
    "provider/event",
    "provider/stream",
    "providers/complete",
    "providers/register",
    "providers/unregister",
    "providers/update",
    "session/create",
    "session/fork",
    "session/reload",
    "session/switch",
]


def canonical(value):
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def host_offer():
    return {
        "schema": "octet.extension.api/0.3",
        "encoding": "octet-canonical-json-v1",
        "required_capabilities": list(REQUIRED_CAPABILITIES),
        "optional_capabilities": list(OPTIONAL_CAPABILITIES),
        "required_methods": list(REQUIRED_METHODS),
        "optional_methods": list(OPTIONAL_METHODS),
        "limits": {
            "max_frame_bytes": 1_048_576,
            "max_concurrent_requests": 4,
            "max_tools": 256,
        },
    }


def initialize(api_version="0.3", contract=None):
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "api_version": api_version,
            "octet_version": "0.7.6",
            "extension": {
                "name": "api-v03-minimal",
                "version": "0.1.0",
                "manifest_path": str(EXTENSION / "extension.toml"),
                "source": "explicit",
            },
            "workspace": str(ROOT),
            "capabilities": {"filesystem": "none", "process": False, "network": False},
            "contributes": {
                "tools": ["echo"],
                "commands": [],
                "shortcuts": [],
                "hooks": [],
                "ui": [],
                "context": False,
                "tool_renderers": [],
                "notifications": False,
                "confirmations": False,
            },
            "flag_values": [],
            "host": {},
            "contract": host_offer() if contract is None else contract,
        },
    }


class MinimalExtensionTests(unittest.TestCase):
    def launch(self, **environment):
        env = dict(os.environ)
        env.pop("OCTET_EXTENSION_API_VERSION", None)
        env.update(environment)
        return subprocess.Popen(
            [str(EXTENSION)],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )

    def send(self, process, value):
        assert process.stdin is not None
        process.stdin.write(canonical(value) + b"\n")
        process.stdin.flush()

    def receive(self, process):
        assert process.stdout is not None
        line = process.stdout.readline()
        self.assertTrue(line, "extension closed stdout before replying")
        self.assertTrue(line.endswith(b"\n"), line)
        value = json.loads(line)
        self.assertEqual(line, canonical(value) + b"\n")
        return value

    def error_code(self, response, request_id):
        self.assertEqual(response["id"], request_id)
        self.assertIn("error", response)
        error = response["error"]
        self.assertIn("code", error)
        self.assertIn("message", error)
        return error["code"]

    def finish(self, process):
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=2)
        if process.stdin is not None and not process.stdin.closed:
            process.stdin.close()
        stdout, stderr = process.communicate(timeout=2)
        return stdout, stderr

    def test_negotiation_tool_unknown_envelope_and_shutdown(self):
        process = self.launch()
        try:
            self.send(process, initialize())
            response = self.receive(process)
            result = response["result"]
            self.assertEqual(result["api_version"], "0.3")
            self.assertEqual(
                result["contract"]["capabilities"], REQUIRED_CAPABILITIES
            )
            self.assertEqual(result["contract"]["methods"], REQUIRED_METHODS)
            self.assertEqual(result["tools"][0]["name"], "echo")
            self.assertEqual(result["tools"][0]["output_schema"]["type"], "object")

            self.send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tool/call",
                    "params": {"name": "echo", "arguments": {}, "context": {}},
                    "extra": True,
                },
            )
            self.assertEqual(self.error_code(self.receive(process), 2), -32_600)

            self.send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/register",
                    "params": {},
                },
            )
            self.assertEqual(self.error_code(self.receive(process), 3), -32_601)

            self.send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tool/call",
                    "params": {
                        "name": "echo",
                        "arguments": {"text": "hello from API 0.3"},
                        "context": {},
                    },
                },
            )
            tool_result = self.receive(process)["result"]
            self.assertEqual(tool_result["content"][0]["text"], "hello from API 0.3")
            self.assertEqual(tool_result["structured_content"], {"text": "hello from API 0.3"})
            self.assertEqual(tool_result["metadata"], {"delay_ms": 0})

            self.send(
                process,
                {"jsonrpc": "2.0", "id": 5, "method": "shutdown", "params": {}},
            )
            self.assertEqual(self.receive(process)["result"], {"terminal": "shutdown"})
            self.assertEqual(process.wait(timeout=2), 0)
        finally:
            _stdout, stderr = self.finish(process)
            self.assertNotIn(b"Traceback", stderr)

    def test_cooperative_cancellation_and_shutdown(self):
        process = self.launch()
        try:
            self.send(process, initialize())
            self.receive(process)
            self.send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tool/call",
                    "params": {
                        "name": "echo",
                        "arguments": {"text": "cancel me", "delay_ms": 5_000},
                        "context": {},
                    },
                },
            )
            self.send(
                process,
                {
                    "jsonrpc": "2.0",
                    "method": "$/cancelRequest",
                    "params": {"id": 2, "reason": "user"},
                },
            )
            cancelled = self.receive(process)
            self.assertEqual(self.error_code(cancelled, 2), -32_800)
            self.assertEqual(cancelled["error"]["message"], "request cancelled")

            self.send(
                process,
                {"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}},
            )
            self.assertEqual(self.receive(process)["result"]["terminal"], "shutdown")
            self.assertEqual(process.wait(timeout=2), 0)
        finally:
            _stdout, stderr = self.finish(process)
            self.assertNotIn(b"Traceback", stderr)

    def test_version_and_unknown_capability_fail_explicitly(self):
        process = self.launch()
        try:
            request = initialize(api_version="0.2")
            request["params"] = {"api_version": "0.2"}
            self.send(process, request)
            response = self.receive(process)
            self.assertEqual(self.error_code(response, 1), -32_010)
            self.assertEqual(response["error"]["message"], "extension API version mismatch")
            self.assertEqual(process.wait(timeout=2), 0)
        finally:
            self.finish(process)

        contract = host_offer()
        contract["optional_capabilities"] = ["unknown-feature"]
        process = self.launch()
        try:
            self.send(process, initialize(contract=contract))
            response = self.receive(process)
            self.assertEqual(self.error_code(response, 1), -32_011)
            self.assertEqual(
                response["error"]["message"], "extension capability mismatch"
            )
            self.assertEqual(process.wait(timeout=2), 0)
        finally:
            self.finish(process)

    def test_noncanonical_duplicate_key_frame_is_rejected(self):
        process = self.launch()
        try:
            assert process.stdin is not None
            process.stdin.write(
                b'{"jsonrpc":"2.0","id":1,"id":1,"method":"initialize","params":{}}\n'
            )
            process.stdin.flush()
            self.assertEqual(process.stdout.readline(), b"")
            self.assertEqual(process.wait(timeout=2), 0)
        finally:
            _stdout, stderr = self.finish(process)
            self.assertIn(b"parse_error", stderr)

    def test_environment_version_mismatch_is_rejected_before_work(self):
        process = self.launch(OCTET_EXTENSION_API_VERSION="0.2")
        try:
            self.assertEqual(process.wait(timeout=2), 2)
        finally:
            _stdout, stderr = self.finish(process)
            self.assertIn(b"version mismatch", stderr)


if __name__ == "__main__":
    unittest.main()
