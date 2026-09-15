#!/usr/bin/env python3
"""Bounded source and wire checks for the Cline API 0.3 migration adapter."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
FIXTURE = ROOT / "fixtures" / "representative"
EXTENSION = ROOT / "extension.py"
sys.path.insert(0, str(ROOT))

from cline_import import (  # noqa: E402
    AdapterError,
    MAX_CONFIG_BYTES,
    MAX_SKILL_BYTES,
    detect,
    import_setup,
)


CONFIG_PATH = "globalStorage/saoudrizwan.claude-dev/settings/cline_settings.json"
MCP_PATH = "cline_mcp_settings.json"


def canonical(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def contract_offer(max_frame_bytes: int = 1_048_576) -> dict[str, object]:
    return {
        "schema": "octet.extension.api/0.3",
        "encoding": "octet-canonical-json-v1",
        "required_capabilities": ["content_parts", "core", "request_cancellation", "tool_call"],
        "optional_capabilities": ["migration.adapter.v1"],
        "required_methods": ["$/cancelRequest", "initialize", "shutdown", "tool/call"],
        "optional_methods": ["migration/detect", "migration/import"],
        "limits": {
            "max_frame_bytes": max_frame_bytes,
            "max_concurrent_requests": 4,
            "max_tools": 8,
        },
    }


def initialize(max_frame_bytes: int = 1_048_576) -> dict[str, object]:
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "api_version": "0.3",
            "octet_version": "0.7.6",
            "extension": {},
            "workspace": str(ROOT),
            "capabilities": {},
            "contributes": {"tools": []},
            "flag_values": [],
            "host": {},
            "contract": contract_offer(max_frame_bytes),
        },
    }


class ClineImportTests(unittest.TestCase):
    def test_representative_settings_skills_mcp_and_provenance(self) -> None:
        detected = detect(str(FIXTURE))
        self.assertTrue(detected["detected"])
        self.assertEqual(detected["config_paths"], [CONFIG_PATH, MCP_PATH])

        result = import_setup(str(FIXTURE), detected["config_paths"])
        self.assertEqual(
            [(item["provider"], item["model"]) for item in result["models"]],
            [("OpenAI", "gpt-4o"), ("Anthropic", "claude-3-5-sonnet"), ("openai", "gpt-4o-mini")],
        )
        self.assertEqual(
            [item["path"] for item in result["skills"]],
            [".cline/skills/review/SKILL.md", ".clinerules/quality.md"],
        )
        self.assertEqual(result["mcp_servers"], [{
            "path": MCP_PATH,
            "name": "local-tools",
            "command": "node",
            "args": ["server.js", "--safe"],
        }])
        self.assertTrue(any("credentials" in item["reason"] for item in result["diagnostics"]))
        self.assertTrue(any("outside model selection" in item["reason"] for item in result["diagnostics"]))
        self.assertTrue(any("environment" in item["reason"] for item in result["diagnostics"]))
        self.assertTrue(any("working" in item["reason"] for item in result["diagnostics"]))
        self.assertTrue(any("approval" in item["reason"] for item in result["diagnostics"]))
        self.assertTrue(any("local stdio" in item["reason"] for item in result["diagnostics"]))
        self.assertNotIn("fixture-secret-must-not-be-imported", json.dumps(result))

    def test_duplicate_json_is_diagnostic_and_not_imported(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            target = root / "cline_settings.json"
            target.write_bytes((ROOT / "fixtures" / "malformed" / "duplicate-settings.json").read_bytes())
            result = import_setup(str(root), ["cline_settings.json"])
            self.assertEqual(result["models"], [])
            self.assertEqual(result["diagnostics"][0]["path"], "cline_settings.json")
            self.assertEqual(result["diagnostics"][0]["severity"], "error")
            self.assertIn("valid JSON", result["diagnostics"][0]["reason"])

    def test_unsupported_and_nonportable_mcp_data_is_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / "cline_mcp_settings.json").write_text(
                json.dumps({
                    "mcpServers": {
                        "http": {"type": "sse", "sse_url": "https://example.invalid"},
                        "secret": {"command": "node", "args": ["--token=secret-value"]},
                        "header": {"command": "node", "args": ["-H", "safe-header"]},
                        "environment": {"command": "node", "args": ["--env", "FOO=bar"]},
                        "unsafe": {"command": "env", "args": []},
                        "bad": {"command": "node", "args": [1]},
                    }
                }),
                encoding="utf-8",
            )
            result = import_setup(str(root), ["cline_mcp_settings.json"])
            self.assertEqual(result["mcp_servers"], [])
            reasons = " ".join(item["reason"] for item in result["diagnostics"])
            self.assertIn("local stdio", reasons)
            self.assertIn("credential-like", reasons)
            self.assertIn("arguments", reasons)
            self.assertIn("unsafe environment command", reasons)
            self.assertNotIn("secret-value", json.dumps(result))

    def test_invalid_mcp_argument_reports_a_bounded_value_free_diagnostic(self) -> None:
        for argument in (1, False, None, {}, "", "private\x00value"):
            with self.subTest(argument=argument), tempfile.TemporaryDirectory() as directory:
                # The adapter rejects lexical symlinks, including macOS /var.
                root = Path(directory).resolve()
                (root / MCP_PATH).write_text(
                    json.dumps({"mcpServers": {"fixture": {"command": "node", "args": [argument]}}}),
                    encoding="utf-8",
                )
                result = import_setup(str(root), [MCP_PATH])
                self.assertEqual(result["mcp_servers"], [])
                self.assertIn({
                    "path": MCP_PATH,
                    "severity": "warning",
                    "reason": "An MCP server has invalid or non-string arguments and was not imported.",
                }, result["diagnostics"])
                self.assertNotIn("private", json.dumps(result))

    def test_read_only_and_oversized_sources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            config = root / "cline_settings.json"
            config.write_text('{"apiProvider":"OpenAI","apiModelId":"model"}', encoding="utf-8")
            skill = root / ".cline" / "skills" / "large" / "SKILL.md"
            skill.parent.mkdir(parents=True)
            skill.write_bytes(b"x" * (MAX_SKILL_BYTES + 1))
            before = {path: (path.read_bytes(), path.stat().st_mtime_ns) for path in (config, skill)}

            detected = detect(str(root))
            self.assertIn("cline_settings.json", detected["config_paths"])
            result = import_setup(str(root), detected["config_paths"])
            self.assertEqual(len(result["models"]), 1)
            self.assertEqual(result["skills"], [])
            self.assertTrue(any("could not be read" in item["reason"] for item in result["diagnostics"]))
            self.assertEqual(before, {path: (path.read_bytes(), path.stat().st_mtime_ns) for path in (config, skill)})

            oversized = root / "oversized.json"
            oversized.write_bytes(b"{}" + b" " * MAX_CONFIG_BYTES)
            # It is not an authorized candidate, so the adapter must not read it.
            self.assertNotIn("oversized.json", detect(str(root))["config_paths"])

    def test_malformed_source_root_and_config_path_bounds(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            with self.assertRaises(AdapterError):
                detect(str(root / "missing"))
            with self.assertRaises(AdapterError):
                import_setup(str(root), ["../cline_settings.json"])
            with self.assertRaises(AdapterError):
                import_setup(str(root), ["cline_settings.json", "cline_settings.json"])

    @unittest.skipUnless(hasattr(os, "symlink"), "symlink support is unavailable")
    def test_symlinked_source_and_candidates_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryDirectory() as outside_directory:
            root = Path(directory).resolve()
            outside = Path(outside_directory).resolve()
            (outside / "cline_settings.json").write_text('{}', encoding="utf-8")
            link = root / "cline_settings.json"
            try:
                link.symlink_to(outside / "cline_settings.json")
                root_link = root.parent / (root.name + "-link")
                root_link.symlink_to(root, target_is_directory=True)
            except OSError as error:
                self.skipTest(f"symlinks unavailable: {error}")
            try:
                detected = detect(str(root))
                self.assertEqual(detected["config_paths"], [])
                self.assertTrue(any(item["path"] == "cline_settings.json" for item in detected["diagnostics"]))
                with self.assertRaises(AdapterError):
                    detect(str(root_link))
            finally:
                root_link.unlink(missing_ok=True)

    @unittest.skipUnless(hasattr(os, "symlink"), "symlink support is unavailable")
    def test_symlinked_skill_entries_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryDirectory() as outside_directory:
            root = Path(directory).resolve()
            outside = Path(outside_directory).resolve()
            target = outside / "skills" / "review" / "SKILL.md"
            target.parent.mkdir(parents=True)
            target.write_text("do not import this", encoding="utf-8")
            linked_directory = root / ".cline" / "skills" / "linked"
            linked_directory.parent.mkdir(parents=True)
            linked_file = root / ".cline" / "skills" / "file" / "SKILL.md"
            linked_file.parent.mkdir(parents=True)
            try:
                linked_directory.symlink_to(target.parent.parent, target_is_directory=True)
                linked_file.symlink_to(target)
            except OSError as error:
                self.skipTest(f"symlinks unavailable: {error}")
            result = import_setup(str(root), [])
            self.assertEqual(result["skills"], [])
            diagnostic_paths = {item["path"] for item in result["diagnostics"]}
            self.assertIn(".cline/skills/linked", diagnostic_paths)
            self.assertIn(".cline/skills/file/SKILL.md", diagnostic_paths)
            self.assertNotIn("do not import this", json.dumps(result))

    def test_initialize_applies_negotiated_frame_limit_to_response(self) -> None:
        completed = subprocess.run(
            [sys.executable, str(EXTENSION)],
            cwd=ROOT,
            input=canonical(initialize(128)) + b"\n",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        responses = [json.loads(line) for line in completed.stdout.splitlines()]
        self.assertEqual(responses[0]["id"], 1)
        self.assertEqual(responses[0]["error"]["message"], "extension resource exhausted")

    def test_process_negotiates_migration_and_rejects_tools(self) -> None:
        messages = [
            initialize(),
            {"jsonrpc": "2.0", "id": 2, "method": "migration/detect", "params": {"source_root": str(FIXTURE)}},
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "migration/import",
                "params": {"source_root": str(FIXTURE), "config_paths": [CONFIG_PATH, MCP_PATH]},
            },
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tool/call",
                "params": {"name": "anything", "arguments": {}, "context": {}},
            },
            {"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 3}},
            {
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": 3, "reason": "user stopped"},
            },
            {"jsonrpc": "2.0", "id": 5, "method": "shutdown", "params": {}},
        ]
        completed = subprocess.run(
            [sys.executable, str(EXTENSION)],
            cwd=ROOT,
            input=b"".join(canonical(message) + b"\n" for message in messages),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        responses = [json.loads(line) for line in completed.stdout.splitlines()]
        self.assertEqual([response.get("id") for response in responses], [1, 2, 3, 4, 5])
        self.assertEqual(responses[0]["result"]["contract"]["methods"], ["$/cancelRequest", "initialize", "shutdown", "tool/call", "migration/detect", "migration/import"])
        self.assertEqual(responses[1]["result"]["config_paths"], [CONFIG_PATH, MCP_PATH])
        self.assertEqual(responses[3]["error"]["message"], "unknown or unnegotiated method")
        self.assertEqual(responses[4]["result"], {"terminal": "shutdown"})
        self.assertNotIn(b"Traceback", completed.stderr)

    def test_malformed_frame_terminates_stream(self) -> None:
        malformed = b'{"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}\n'
        shutdown = canonical({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}})
        completed = subprocess.run(
            [sys.executable, str(EXTENSION)],
            cwd=ROOT,
            input=canonical(initialize()) + b"\n" + malformed + shutdown + b"\n",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        responses = [json.loads(line) for line in completed.stdout.splitlines()]
        self.assertEqual([response.get("id") for response in responses], [1])
        self.assertIn(b"parse_error", completed.stderr)


if __name__ == "__main__":
    unittest.main()
