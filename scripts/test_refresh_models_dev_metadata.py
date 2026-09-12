#!/usr/bin/env python3
"""Offline regressions for pinned models.dev metadata refreshes."""
import hashlib
import io
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("refresh-models-dev-pricing.py")
spec = importlib.util.spec_from_file_location("refresh_models_dev", SCRIPT)
refresh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refresh)


class MetadataRefreshTests(unittest.TestCase):
    def fixture(self):
        model = {
            "id": "example", "name": "Example Flash", "reasoning": True,
            "reasoning_options": [{"type": "toggle"},
                                  {"type": "effort", "values": ["low", "high", "max"],
                                   "default": "high"}],
            "limit": {"context": 1000000, "output": 384000},
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "tool_call": False, "structured_output": True,
            "interleaved": {"field": "reasoning_content"},
            "cost": {"input": 0.15, "output": 0.6, "cache_read": 0.003},
        }
        return {"deepseek": {"models": {"example": model}},
                "openai": {"models": {"example": model, "gpt-5.6": model}},
                "togetherai": {"models": {"example": model}},
                "custom": {"models": {"example": model}}}

    def test_rich_assertions_preserve_exact_holes_defaults_and_provider_scope(self):
        catalog = self.fixture()
        result = refresh.capabilities_snapshot(catalog)
        self.assertEqual(set(result), {"deepseek/example", "openai/example", "together/example"})
        self.assertEqual(result["deepseek/example"], {
            k: v for k, v in catalog["deepseek"]["models"]["example"].items()
            if k in refresh.CAPABILITY_FIELDS})
        self.assertNotIn("medium", result["deepseek/example"]["reasoning_options"][1]["values"])
        self.assertFalse(result["deepseek/example"]["tool_call"])
        catalog["deepseek"]["models"]["example"]["reasoning"] = None
        self.assertIsNone(refresh.capabilities_snapshot(catalog)["deepseek/example"]["reasoning"])

    def test_pricing_remains_exact_and_unverified_schedule_stays_unknown(self):
        result = refresh.snapshot(self.fixture())
        self.assertNotIn("deepseek/example", result)
        self.assertNotIn("openai/gpt-5.6", result)
        self.assertEqual(result["openai/example"]["input"], 150000)
        self.assertEqual(result["openai/example"]["cache_read"], 3000)
        self.assertEqual(refresh.microdollars("0.0000005"), 1)
        for value in [-1, "NaN", "Infinity"]:
            with self.assertRaises(ValueError):
                refresh.microdollars(value)
        catalog = self.fixture()
        catalog["openai"]["models"]["example"]["cost"]["input"] = None
        self.assertNotIn("openai/example", refresh.snapshot(catalog))

    def test_download_is_bounded_proxy_free_and_redirects_fail_closed(self):
        class Response(io.BytesIO):
            headers = {}

        class Opener:
            def open(self, request, timeout):
                self.request, self.timeout = request, timeout
                return Response(b"x" * 33)

        opener = Opener()
        with patch.object(refresh, "MAX_SNAPSHOT_BYTES", 32), \
             patch.object(refresh, "build_opener", return_value=opener) as build:
            with self.assertRaisesRegex(ValueError, "size limit"):
                refresh.download_source()
            self.assertEqual(opener.request.full_url, refresh.API_URL)
            self.assertEqual(opener.timeout, 30)
            self.assertEqual(build.call_args.args[0].proxies, {})
            self.assertIsInstance(build.call_args.args[1], refresh.NoRedirects)
        for url in ["http://models.dev/api.json", "https://elsewhere.invalid/api.json"]:
            with self.assertRaises(refresh.HTTPError) as caught:
                refresh.NoRedirects().redirect_request(
                    refresh.Request(refresh.API_URL), None, 302, "Found", {}, url)
            caught.exception.close()

    def test_refresh_and_check_are_deterministic_and_check_never_writes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "api.json"
            source.write_text(json.dumps(self.fixture()))
            outputs = {flag: root / (flag[2:] + ".json") for flag in
                       ["--output", "--names-output", "--capabilities-output", "--source-output"]}
            command = [sys.executable, str(SCRIPT), "--source", str(source)]
            for flag, path in outputs.items():
                command.extend([flag, str(path)])
            subprocess.run(command, check=True, capture_output=True)
            before = {p: p.read_bytes() for p in outputs.values()}
            subprocess.run(command + ["--check"], check=True, capture_output=True)
            self.assertEqual(before, {p: p.read_bytes() for p in outputs.values()})
            receipt = json.loads(outputs["--source-output"].read_text())
            self.assertEqual(receipt["sha256"], hashlib.sha256(source.read_bytes()).hexdigest())
            outputs["--capabilities-output"].write_text("{}\n")
            stale = subprocess.run(command + ["--check"], capture_output=True, text=True)
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("stale models.dev snapshot", stale.stderr)
            self.assertEqual(outputs["--capabilities-output"].read_text(), "{}\n")
            before = {p: p.read_bytes() for p in outputs.values()}
            source.write_text('{"openai": {"models": {}}}')
            invalid = subprocess.run(command, capture_output=True)
            self.assertNotEqual(invalid.returncode, 0)
            self.assertEqual(before, {p: p.read_bytes() for p in outputs.values()})


if __name__ == "__main__":
    unittest.main()
