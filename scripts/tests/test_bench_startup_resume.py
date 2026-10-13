"""Synthetic startup/resume benchmark contract tests; no processes or network."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "bench_startup_resume", Path(__file__).resolve().parents[1] / "bench-startup-resume.py")
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


class StartupResumeContractTests(unittest.TestCase):
    def test_environment_excludes_credentials_config_and_extensions(self):
        with patch.dict(os.environ, {
            "HOME": "/private-home", "OPENAI_API_KEY": "not-a-real-secret",
            "OCTET_CONFIG": "/private-config", "OCTET_EXTENSIONS": "private-extension",
            "RUST_LOG": "trace", "OCTET_TELEMETRY": "/private-telemetry",
        }):
            environment = BENCH.environment(Path("/synthetic-home"))
        self.assertEqual(environment["HOME"], "/synthetic-home")
        for name in ("OPENAI_API_KEY", "OCTET_CONFIG", "OCTET_EXTENSIONS", "RUST_LOG", "OCTET_TELEMETRY"):
            self.assertNotIn(name, environment)

    def test_fixture_retains_full_parent_chain_and_saved_prompt_colors(self):
        with tempfile.TemporaryDirectory() as root:
            home, workspace, sessions = BENCH.fixture(Path(root), 5, 2)
            self.assertEqual(workspace, workspace.resolve())
            credential = home / ".octet/credentials/custom.json"
            self.assertEqual(credential.stat().st_mode & 0o777, 0o600)
            provider = json.loads(credential.read_text())["providers"]["bench"]
            self.assertEqual(provider["auth"], {"kind": "none"})
            self.assertFalse(provider["auto_discover"])
            self.assertEqual([model["api_name"] for model in provider["models"]],
                             [f"model-{i:05}" for i in range(5)])
            transcript, = sessions.glob("*/synthetic.jsonl")
            records = [json.loads(line) for line in transcript.read_text().splitlines()]
            self.assertEqual(len(records), 5)
            for i, record in enumerate(records[:-1]):
                self.assertEqual(record["id"], str(i))
                self.assertEqual(record["parent"], str(i - 1) if i else None)
                if i % 2 == 0:
                    self.assertEqual(record["metadata"], {
                        "prompt_model": BENCH.MODEL, "prompt_color": "#5a36d6"})
                else:
                    self.assertEqual(record["value"]["Assistant"]["model"], BENCH.MODEL)
            self.assertEqual(records[-1]["type"], "head")
            self.assertEqual(records[-1]["id"], "3")
            self.assertEqual(transcript.stat().st_mode & 0o777, 0o600)

    def test_all_native_markers_must_be_present_once_in_order(self):
        expected = b"USER_000000\nANSWER_000001\nUSER_000002\nANSWER_000003"
        BENCH.validate_history(expected, 2)
        BENCH.validate_history(b"empty fresh frame", 0)
        for invalid in (
            expected.replace(b"ANSWER_000001", b"missing"),
            expected + b"\nUSER_000000",
            b"ANSWER_000001\nUSER_000000\nUSER_000002\nANSWER_000003",
        ):
            with self.subTest(output=invalid), self.assertRaises(AssertionError):
                BENCH.validate_history(invalid, 2)


if __name__ == "__main__":
    unittest.main()
