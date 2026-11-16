import base64
import importlib.util
import json
import tempfile
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "latency", Path(__file__).resolve().parents[1] / "bench-interaction-latency.py")
latency = importlib.util.module_from_spec(spec)
spec.loader.exec_module(latency)


class LatencySummaryTests(unittest.TestCase):
    def test_percentiles_are_nearest_rank_and_separate_event_kinds(self):
        samples = [dict(kind="wheel", updated_us=n * 100, painted_us=n * 1000)
                   for n in range(1, 21)]
        samples.append(dict(kind="arrow", updated_us=500, painted_us=2000))
        result = latency.summarize(samples)
        self.assertEqual(result["wheel"]["count"], 20)
        self.assertEqual(result["wheel"]["painted_ms"], {"p50": 10, "p95": 19})
        self.assertEqual(result["arrow"]["updated_ms"], {"p50": .5, "p95": .5})

    def test_empty_trace_has_no_fabricated_samples(self):
        self.assertEqual(latency.summarize([]), {})

    def test_scroll_hint_reads_last_ansi_decorated_offset(self):
        self.assertEqual(latency.scroll_hint(b"\x1b[2m3 rows back\x1b[0m\x1b[H64 rows back"), 64)
        self.assertIsNone(latency.scroll_hint(b"no scroll hint"))

    def test_mixed_fixture_has_paired_tool_cards_and_valid_png_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, _, sessions = latency.mixed_fixture(Path(tmp), 20)
            records = [json.loads(line) for line in next(sessions.glob("*/*.jsonl")).read_text().splitlines()]
        self.assertEqual(len(records), 81)
        self.assertEqual(records[-1]["id"], records[-2]["id"])
        images = []
        for turn in range(20):
            call = records[turn * 4 + 1]["value"]["Assistant"]["content"][0]["ToolCall"]
            result = records[turn * 4 + 2]["value"]["User"]["content"][0]["ToolResult"]
            self.assertEqual(call["id"], result["tool_call_id"])
            for part in result["content"]:
                if "Media" in part:
                    images.append(base64.b64decode(part["Media"]["Image"]["source"]["Inline"]))
        self.assertEqual(len(images), 2)
        self.assertTrue(all(image.startswith(b"\x89PNG\r\n\x1a\n") for image in images))
