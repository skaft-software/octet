"""Budget-gate contracts; synthetic results only, no binary or PTY is used."""
import importlib.util
import io
import json
import math
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "benchmarks"))
SPEC = importlib.util.spec_from_file_location(
    "budget_gate", HERE.parents[1] / "benchmarks/budget_gate.py")
sys.modules["budget_gate"] = importlib.util.module_from_spec(SPEC)
GATE = sys.modules["budget_gate"]
SPEC.loader.exec_module(GATE)


def distribution(value=5.0, count=20):
    return {"count": count, "p50": value, "p95": value * 1.5, "max": value * 2}


def result(turns=100, *, status="passed", stream=True, append_ms=0.5):
    stream_cell = {
        "turn": turns, "kind": "stream", "wall_ms": 900.0, "reply_frame_ms": 300.0,
        "settled_ms": 500.0, "cpu_ms": 250.0, "rss_bytes": 40 << 20,
        "peak_rss_bytes": 41 << 20, "session_bytes": (turns << 16),
        "append_bytes": (1 << 16), "pty_frames": 12, "pty_bytes": 20000,
        "append_ms": append_ms, "render_wall_ms": None, "render_total_ms": None,
        "request_bytes": 30000, "submit_to_request_ms": 3.0, "trace": {}}
    summary = {
        "turn_latency_ms": distribution(8.0, turns),
        "reply_frame_ms": distribution(4.0, turns),
        "settled_ms": distribution(10.0, turns),
        "latency_by_kind_ms": {"small": distribution(7.0)},
        "early_small_p50_ms": 7.0, "late_small_p50_ms": 8.4,
        "early_small_reply_p50_ms": 4.0, "late_small_reply_p50_ms": 5.5,
        "early_small_settled_p50_ms": 9.0, "late_small_settled_p50_ms": 12.0,
        "peak_rss_bytes": 42 << 20, "end_rss_bytes": 40 << 20,
        "rss_growth_bytes_per_100_turns": 1 << 20,
        "session_bytes": turns << 16,
        "append_bytes_per_turn": distribution(3 << 16, turns),
        "append_persist_ms_per_turn": distribution(append_ms, turns - 1),
        "cpu_ms": 12.0 * turns, "stream": stream_cell}
    return {
        "schema": "octet.long-session.v1", "status": status,
        "failure": None if status == "passed" else "TimeoutError",
        "scales": [turns],
        "cells": [{"turns": turns, "summary": summary,
                   "resume": {"ready_frame_ms": 180.0, "rss_bytes": (40 << 20)},
                   "correctness": {"large_tools": turns // 25, "checkpoints": turns}}],
        "samples": [], "provider_requests": [], "correctness_passed": status == "passed"}


def budget(scales=100, **checks):
    base = {
        "turn_latency_growth_ratio": {"cap": 2.0},
        "turn_latency_p50_ms": {"cap": 100.0},
        "peak_rss_mib": {"cap": 300.0},
        "resume_100_ready_ms": {"cap": 2000.0},
    }
    base.update(checks)
    return {"schema": "octet.long-session.budget.v1", "scope": {"short_scale": scales},
            "checks": base}


class EvaluationTests(unittest.TestCase):
    def test_a_passing_short_scale_result_gates(self):
        count, breaches = GATE.evaluate(budget(), result())
        self.assertEqual((count, breaches), (0, []))

    def test_a_cap_breach_names_the_metric_and_both_sides(self):
        count, breaches = GATE.evaluate(
            budget(turn_latency_p50_ms={"cap": 4.0}), result())
        self.assertEqual(count, 1)
        self.assertIn("turn_latency_p50_ms: 8 (cap 4, floor none)", breaches[0])

    def test_the_growth_ratio_is_relative_and_heads_off_dividing_by_zero(self):
        count, _ = GATE.evaluate(budget(turn_latency_growth_ratio={"cap": 1.02}), result())
        self.assertEqual(count, 1)
        tight = result()
        tight["cells"][-1]["summary"]["early_small_p50_ms"] = 0
        count, breaches = GATE.evaluate(budget(), tight)
        self.assertEqual(count, 1)
        self.assertIn("turn_latency_growth_ratio unobserved", breaches[0])

    def test_a_floor_pins_fixture_driven_metrics_against_shrinkage(self):
        count, breaches = GATE.evaluate(
            budget(append_bytes_p50_per_turn={"cap": 4 << 16, "floor": 2 << 16}), result())
        self.assertEqual((count, breaches), (0, []))
        shrunk = result()
        shrunk["cells"][-1]["summary"]["append_bytes_per_turn"] = distribution(1000, 20)
        count, breaches = GATE.evaluate(
            budget(append_bytes_p50_per_turn={"cap": 4 << 16, "floor": 2 << 16}), shrunk)
        self.assertEqual(count, 1)
        self.assertIn("fell below floor", breaches[0])

    def test_alt_scale_metrics_are_reported_per_cell(self):
        widened = result()
        widened["cells"].append(dict(widened["cells"][-1], turns=1000))
        metrics = GATE.observed(widened)
        self.assertEqual(metrics["resume_100_ready_ms"], 180.0)
        self.assertEqual(metrics["resume_1000_ready_ms"], 180.0)

    def test_every_budget_check_needs_a_cap_and_an_observed_metric(self):
        with self.assertRaisesRegex(SystemExit, "no cap"):
            GATE.evaluate(budget(broken={"observed": True}), result())
        with self.assertRaisesRegex(SystemExit, "no observed metric"):
            GATE.evaluate(budget(absent_metric={"cap": 1.0}), result())

    def test_failed_and_wrong_scale_results_are_not_checkable(self):
        for mutated, message in (
                (result(status="failed"), "did not pass"),
                (dict(result(), cells=[]), "largest cell"),
                (result(turns=1000), "largest cell")):
            with self.subTest(mutated="cells removed" if not mutated.get("cells") else "other"):
                count, breaches = GATE.evaluate(budget(), mutated)
                self.assertEqual(count, -1)
                self.assertTrue(breaches)
        self.assertIn("TimeoutError", GATE.evaluate(budget(), result(status="failed"))[1][0])

    def test_infinite_and_missing_derived_metrics_breach_not_crash(self):
        uninstrumented = result()
        uninstrumented["cells"][-1]["summary"]["append_persist_ms_per_turn"] = None
        count, breaches = GATE.evaluate(
            budget(append_persist_p95_ms={"cap": 5.0}), uninstrumented)
        self.assertEqual(count, 1)
        self.assertIn("append_persist_p95_ms unobserved", breaches[0])


class BudgetFileTests(unittest.TestCase):
    def test_the_committed_budget_loads(self):
        loaded = GATE.load_budget(HERE.parents[1] / "benchmarks/performance_budget.json")
        self.assertEqual(loaded["schema"], "octet.long-session.budget.v1")
        self.assertTrue(loaded["checks"])
        for label, rule in loaded["checks"].items():
            self.assertIn("cap", rule, label)
            if rule["cap"] is not None:
                self.assertTrue(math.isfinite(rule["cap"]), label)

    def test_bad_schemas_and_empty_checks_are_refused(self):
        with self.assertRaisesRegex(SystemExit, "schema"):
            GATE.load_budget(self._write({"schema": "other"}))
        with self.assertRaisesRegex(SystemExit, "no budget checks"):
            GATE.load_budget(self._write({"schema": "octet.long-session.budget.v1", "checks": {}}))

    @staticmethod
    def _write(payload):
        import tempfile
        directory = tempfile.mkdtemp()
        path = Path(directory) / "budget.json"
        path.write_text(json.dumps(payload))
        return path


if __name__ == "__main__":
    unittest.main()
