import importlib.util
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
