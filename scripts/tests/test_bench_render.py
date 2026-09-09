"""Deterministic evidence-contract checks; no timing thresholds or provider calls."""

import copy
import importlib.util
import os
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("bench_render", Path(__file__).resolve().parents[1] / "bench-render.py")
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


def result_fixture():
    trials = []
    for elapsed in (10, 20, 30, 40):
        phases = {
            phase: {"calls": 1, "elapsed_ns": elapsed, "allocation_calls": 2, "allocation_requested_bytes": 64}
            for phase in BENCH.PHASES
        }
        phases["total"] = {metric: sum(phases[phase][metric] for phase in BENCH.PHASES)
                           for metric in (*BENCH.METRICS, "calls")}
        trials.append({"phases": phases, "correctness_passed": True})
    summary = {
        phase: {
            metric: {"count": len(trials), **{
                name: BENCH.percentile([trial["phases"][phase][metric] for trial in trials], point)
                for name, point in (("p50", 0.5), ("p95", 0.95))}}
            for metric in BENCH.METRICS}
        for phase in (*BENCH.PHASES, "total")}
    return {"schema": BENCH.DRIVER_SCHEMA, "repetitions": 4, "cases": [{
        "mode": "tail", "source_bytes": 7, "chunk_bytes": 7, "chunk_count": 1,
        "correctness": {"live_exact_replay": True, "final_raw_exact": True,
                        "final_semantics_exact": True, "final_copy_exact": True,
                        "final_output_exact": True},
        "trials": trials, "summary": summary}]}


class BenchmarkContractTests(unittest.TestCase):
    def test_percentiles_interpolate_independent_trials(self):
        self.assertEqual(BENCH.percentile([40, 10, 30, 20], 0.5), 25)
        self.assertEqual(BENCH.percentile([40, 10, 30, 20], 0.95), 38.5)
        self.assertEqual(BENCH.percentile([7], 0.95), 7)

    def test_environment_does_not_inherit_credentials_or_user_config(self):
        with patch.dict(os.environ, {"PROVIDER_API_KEY": "not-a-real-secret", "HOME": "/private-home", "OCTET_CONFIG": "/private-config", "RUST_LOG": "trace"}):
            environment = BENCH.scrubbed_environment(Path("/temporary-home"))
        self.assertEqual(environment["HOME"], "/temporary-home")
        for key in ("PROVIDER_API_KEY", "OCTET_CONFIG", "RUST_LOG"):
            self.assertNotIn(key, environment)

    def test_accepts_complete_raw_trials(self):
        BENCH.validate_result(result_fixture())

    def test_rejects_wrong_percentiles_counts_totals_and_correctness(self):
        original = result_fixture()
        mutations = (
            lambda result: result.update(schema="other"),
            lambda result: result.update(repetitions=5),
            lambda result: result["cases"][0].update(chunk_count=2),
            lambda result: result["cases"][0].update(chunk_bytes=0),
            lambda result: result["cases"][0].update(mode="missing"),
            lambda result: result["cases"][0]["correctness"].pop("final_copy_exact"),
            lambda result: result["cases"][0]["correctness"].update(final_raw_exact=False),
            lambda result: result["cases"][0]["trials"][0].update(correctness_passed=False),
            lambda result: result["cases"][0]["trials"][0]["phases"]["total"].update(elapsed_ns=0),
            lambda result: result["cases"][0]["summary"]["total"]["elapsed_ns"].update(p95=0),
            lambda result: result["cases"][0]["summary"]["total"]["elapsed_ns"].update(count=8),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                result = copy.deepcopy(original)
                mutate(result)
                with self.assertRaises(ValueError):
                    BENCH.validate_result(result)


if __name__ == "__main__":
    unittest.main()
