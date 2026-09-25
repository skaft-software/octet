#!/usr/bin/env python3
"""Offline regressions for the pinned OpenRouter image-model catalog."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("refresh-openrouter-image-models.py")
spec = importlib.util.spec_from_file_location("refresh_openrouter_images", SCRIPT)
refresh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refresh)


def record(model_id, output, input_=None, pricing=None, name=None):
    return {
        "id": model_id,
        "name": name or model_id,
        "architecture": {
            "input_modalities": input_ if input_ is not None else ["text"],
            "output_modalities": output,
        },
        "pricing": pricing if pricing is not None else {
            "prompt": "0.000002",
            "completion": "0.000012",
            "input_cache_read": "0.0000002",
            "input_cache_write": "0.000000375",
        },
    }


class ImageCatalogRefreshTests(unittest.TestCase):
    def payload(self):
        return {
            "data": [
                record("b/text-only", ["text"]),
                record("c/image-input", ["image"], input_=["image"]),
                record("a/image-output", ["image", "text"], input_=["text", "image", "text"]),
                record("d/dynamic", ["image"], pricing={"prompt": "-1", "completion": "-1"}),
                record("e/no-pricing", ["image"], pricing={}),
            ]
        }

    def test_catalog_filters_sorts_and_converts_prices(self):
        catalog = refresh.image_catalog(self.payload())
        self.assertEqual(catalog["version"], 1)
        self.assertEqual(
            [model["id"] for model in catalog["models"]],
            ["a/image-output", "c/image-input", "d/dynamic", "e/no-pricing"],
        )
        first = catalog["models"][0]
        self.assertEqual(first["api"], "openrouter-images")
        self.assertEqual(first["provider"], "openrouter")
        self.assertEqual(first["base_url"], "https://openrouter.ai/api/v1/")
        # Duplicate input modalities collapse, source order is preserved.
        self.assertEqual(first["input"], ["text", "image"])
        self.assertEqual(first["output"], ["image", "text"])
        # dollars/token -> microdollars per million tokens
        self.assertEqual(
            first["cost"],
            {
                "input": 2_000_000,
                "output": 12_000_000,
                "cache_read": 200_000,
                "cache_write": 375_000,
            },
        )
        # A route with no input modalities defaults to text, like upstream.
        self.assertEqual(catalog["models"][1]["input"], ["image"])

    def test_dynamic_or_missing_rates_are_unpriced_but_explicit_free_rates_are_zero(self):
        catalog = refresh.image_catalog(self.payload())
        for model_id in ("d/dynamic", "e/no-pricing"):
            model = next(model for model in catalog["models"] if model["id"] == model_id)
            self.assertIsNone(model["cost"])
        rates = {
            "prompt": "0.000002", "completion": "0.000012",
            "input_cache_read": "0.0000002", "input_cache_write": "0.000000375",
        }
        for key in rates:
            with self.subTest(missing=key):
                missing = {field: value for field, value in rates.items() if field != key}
                self.assertIsNone(refresh.model_cost(missing))
                self.assertIsNone(refresh.model_cost({**rates, key: None}))
                self.assertIsNone(refresh.model_cost({**rates, key: ""}))
        self.assertIsNone(refresh.model_cost(None))
        self.assertEqual(
            refresh.model_cost(dict.fromkeys(rates, "0")),
            {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
        )
        with self.assertRaises(ValueError):
            refresh.image_catalog({"data": [record("f/bad", ["image"], pricing={"prompt": "nope"})]})

    def test_empty_or_missing_model_lists_fail_closed(self):
        for payload in ([], {"data": []}, {}, {"data": [record("b/text-only", ["text"])]}):
            with self.assertRaises(SystemExit):
                refresh.image_catalog(payload)

    def test_source_is_required_and_no_fetch_path_exists(self):
        # The generator must never fetch: running without --source fails fast.
        result = subprocess.run(
            [sys.executable, str(SCRIPT)],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--source", result.stderr)
        self.assertFalse(hasattr(refresh, "download_source"))

    def test_refresh_and_check_are_deterministic_and_check_never_writes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "models.json"
            source.write_text(json.dumps(self.payload()))
            output = root / "catalog.json"
            source_output = root / "source.json"
            command = [
                sys.executable, str(SCRIPT),
                "--source", str(source),
                "--output", str(output),
                "--source-output", str(source_output),
            ]
            subprocess.run(command, check=True, capture_output=True)
            before = {path: path.read_bytes() for path in (output, source_output)}
            subprocess.run(command + ["--check"], check=True, capture_output=True)
            self.assertEqual(before, {path: path.read_bytes() for path in (output, source_output)})
            receipt = json.loads(source_output.read_text())
            self.assertEqual(
                receipt["sha256"], hashlib.sha256(source.read_bytes()).hexdigest()
            )
            self.assertEqual(receipt["url"], refresh.MODELS_URL)
            output.write_text("{}\n")
            stale = subprocess.run(command + ["--check"], capture_output=True, text=True)
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("stale OpenRouter image catalog", stale.stderr)
            self.assertEqual(output.read_text(), "{}\n")
            before = {path: path.read_bytes() for path in (output, source_output)}
            source.write_text('{"data": [{"id": "x", "name": "X"}]}')
            invalid = subprocess.run(command, capture_output=True)
            self.assertNotEqual(invalid.returncode, 0)
            self.assertEqual(before, {path: path.read_bytes() for path in (output, source_output)})


if __name__ == "__main__":
    unittest.main()
