#!/usr/bin/env python3
import importlib.util
import json
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


EXTENSION_DIR = Path(__file__).resolve().parents[1]
EXTENSION = EXTENSION_DIR / "extension.py"
FIXTURES = EXTENSION_DIR / "tests" / "fixtures"


spec = importlib.util.spec_from_file_location("octet_import_aider", EXTENSION)
assert spec is not None and spec.loader is not None
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"


def initialize_request(request_id=1):
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "initialize",
        "params": {
            "api_version": "0.4",
            "octet_version": "0.7.6",
            "extension": {"name": "qualification-host"},
            "workspace": str(FIXTURES.resolve()),
            "capabilities": {},
            "contributes": {"tools": []},
            "flag_values": [],
            "host": {},
            "contract": {
                "schema": "octet.extension.api/0.3",
                "encoding": "octet-canonical-json-v1",
                "required_capabilities": ["content_parts", "core", "request_cancellation", "tool_call"],
                "optional_capabilities": ["migration.adapter.v1"],
                "required_methods": ["$/cancelRequest", "initialize", "shutdown", "tool/call"],
                "optional_methods": ["migration/detect", "migration/import"],
                "limits": {
                    "max_frame_bytes": 1_048_576,
                    "max_concurrent_requests": 64,
                    "max_tools": 256,
                },
            },
        },
    }


def run_protocol(messages):
    payload = b"".join(canonical(message) for message in messages)
    completed = subprocess.run(
        [sys.executable, str(EXTENSION)],
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(completed.stderr.decode("utf-8", "replace"))
    return [json.loads(line) for line in completed.stdout.splitlines() if line]


def snapshot(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_file() and not path.is_symlink():
            result[path.relative_to(root).as_posix()] = (path.read_bytes(), stat.S_IMODE(path.stat().st_mode))
    return result


class ProtocolTests(unittest.TestCase):
    def test_detect_import_and_shutdown_are_canonical_and_source_only(self):
        source = FIXTURES / "basic"
        before = snapshot(source)
        messages = [
            initialize_request(),
            {"jsonrpc": "2.0", "id": 2, "method": "migration/detect", "params": {"source_root": str(source.resolve())}},
        ]
        # The import request is intentionally supplied from detect's expected bounded fixture set.
        paths = [
            ".aider.chat.history.md",
            ".aider.conf.yml",
            ".aider.extra.yaml",
            ".aider.md",
            ".aider.model.settings.yml",
            ".aiderignore",
        ]
        messages.extend([
            {"jsonrpc": "2.0", "id": 3, "method": "migration/import", "params": {"source_root": str(source.resolve()), "config_paths": paths}},
            {"jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": {}},
        ])
        responses = run_protocol(messages)
        self.assertEqual([response["id"] for response in responses], [1, 2, 3, 4])
        self.assertEqual(responses[0]["result"]["tools"], [])
        self.assertEqual(responses[0]["result"]["contract"]["capabilities"][-1], "migration.adapter.v1")
        self.assertEqual(responses[0]["result"]["contract"]["methods"][-2:], ["migration/detect", "migration/import"])
        detection = responses[1]["result"]
        self.assertTrue(detection["detected"])
        self.assertNotIn(".aider.model.metadata.json", detection["config_paths"])
        self.assertNotIn(".aider.tags.cache.v3", detection["config_paths"])
        imported = responses[2]["result"]
        self.assertTrue(any(model["model"] == "claude-3-5-sonnet" for model in imported["models"]))
        self.assertTrue(any(model["provider"] == "ollama" for model in imported["models"]))
        self.assertEqual(len(imported["mcp_servers"]), 1)
        self.assertEqual(imported["mcp_servers"][0]["name"], "local-docs")
        self.assertTrue(any("history" in diagnostic["reason"].lower() for diagnostic in imported["diagnostics"]))
        wire = json.dumps(responses, ensure_ascii=False)
        self.assertNotIn("OPENAI_API_KEY", wire)
        self.assertEqual(before, snapshot(source))

    def test_initialize_version_is_checked_before_exact_object(self):
        request = initialize_request()
        request["params"]["api_version"] = "0.2"
        request["params"]["unexpected"] = True
        responses = run_protocol([request])
        self.assertEqual(responses[0]["error"]["code"], -32010)

    def test_unsafe_tool_call_is_rejected(self):
        messages = [
            initialize_request(),
            {"jsonrpc": "2.0", "id": 2, "method": "tool/call", "params": {"name": "not-advertised", "arguments": {}, "context": {}}},
        ]
        responses = run_protocol(messages)
        self.assertEqual(responses[-1]["error"]["code"], -32602)


class SafeYamlTests(unittest.TestCase):
    def test_yaml_rejects_unsafe_syntax(self):
        rejected = [
            "key:\n\tvalue\n",
            "model: one\nmodel: two\n",
            "model: &selected one\nother: *selected\n",
            "---\nmodel: one\n...\n",
            "key: <<\n",
            "%YAML 1.2\nmodel: one\n",
        ]
        for text in rejected:
            with self.subTest(text=text):
                with self.assertRaises(adapter.YamlError):
                    adapter.SafeYaml(text).parse()

    def test_yaml_depth_and_node_bounds(self):
        nested = ""
        for index in range(adapter.MAX_YAML_DEPTH + 2):
            nested += ("  " * index) + "key:\n"
        nested += ("  " * (adapter.MAX_YAML_DEPTH + 2)) + "value\n"
        with self.assertRaises(adapter.YamlError):
            adapter.SafeYaml(nested).parse()
        many = "\n".join(f"key{index}: value" for index in range(adapter.MAX_YAML_NODES + 1))
        with self.assertRaises(adapter.YamlError):
            adapter.SafeYaml(many).parse()


class BoundaryTests(unittest.TestCase):
    def test_metadata_and_paths_are_not_importable(self):
        with self.assertRaises(adapter.ProtocolFailure):
            adapter.validate_paths([".aider.model.metadata.json"])
        self.assertFalse(adapter.allowed_file(".aider.tags.cache.v4"))
        self.assertFalse(adapter.safe_relative("name\u202e.yml"))
        with self.assertRaises(adapter.ProtocolFailure):
            adapter.validate_source_root("/tmp/name\u202e")

    def test_adversarial_fixtures_are_bounded(self):
        expected_errors = {"tabs", "duplicate", "anchors", "documents"}
        for directory in sorted((FIXTURES / "adversarial").iterdir()):
            if not directory.is_dir():
                continue
            with self.subTest(directory=directory.name):
                paths, discovery_diagnostics = adapter.discover(str(directory.resolve()))
                result = adapter.import_source(str(directory.resolve()), paths)
                if directory.name in expected_errors:
                    self.assertTrue(result["diagnostics"] or discovery_diagnostics)
                    self.assertEqual(result["models"], [])
                if directory.name == "credentials":
                    wire = json.dumps(result, ensure_ascii=False)
                    self.assertNotIn("sk-never-import-this-value", wire)
                    self.assertTrue(any(model["model"] == "gpt-4o" for model in result["models"]))
                if directory.name in {"mcp-unsafe", "mcp-remote"}:
                    self.assertEqual(result["mcp_servers"], [])
                    self.assertTrue(result["diagnostics"])

    def test_symlink_is_not_opened(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outside = root / "outside.yml"
            outside.write_text("model: openai/gpt-4o\n", encoding="utf-8")
            link = root / ".aider.conf.yml"
            try:
                link.symlink_to(outside)
            except (OSError, NotImplementedError):
                self.skipTest("symlinks unavailable")
            paths, diagnostics = adapter.discover(str(root.resolve()))
            self.assertNotIn(".aider.conf.yml", paths)
            self.assertTrue(diagnostics)


if __name__ == "__main__":
    unittest.main()
