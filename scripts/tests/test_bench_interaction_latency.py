import base64
import importlib.util
import json
import tempfile
import subprocess
import sys
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
        self.assertEqual(result["wheel"]["painted_ms"], {"p50": 10, "p95": 19, "p99": 20})
        self.assertEqual(result["arrow"]["updated_ms"], {"p50": .5, "p95": .5, "p99": .5})

    def test_empty_trace_has_no_fabricated_samples(self):
        self.assertEqual(latency.summarize([]), {})

    def test_scroll_offset_uses_completed_geometry_not_the_button_label(self):
        self.assertEqual(latency.scroll_offset(
            "octet-frame: sequence=1 scroll=3\noctet-frame: sequence=2 scroll=64\n"), 64)
        self.assertIsNone(latency.scroll_offset("[ ↓ Jump to bottom ]"))

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
        self.assertEqual(len(images), 3)
        self.assertTrue(all(image.startswith(b"\x89PNG\r\n\x1a\n") for image in images))


    def test_frame_work_and_percentiles_keep_spikes_visible(self):
        fields = "laid_out mapped wrapped highlights geometry allocations allocated_bytes capture_us accept_us viewport_us geometry_us".split()
        frames = [dict.fromkeys(fields, 0) | {"duration_us": n * 1000} for n in range(1, 101)]
        frames[-1]["mapped"] = 500
        result = latency.frame_summary(frames)
        self.assertEqual(result["p99_ms"], 99)
        self.assertEqual(result["work_max"]["mapped"], 500)
        self.assertGreater(result["variance_ms2"], 0)


    def test_sampler_signal_requires_explicit_scaling_mode(self):
        result = subprocess.run([sys.executable, str(Path(latency.__file__)), "/unused",
                                 "--profile-signal", "SIGUSR2"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--profile-signal requires --messages", result.stderr)
