"""Focused contract test for the hermetic Pi runtime evidence driver."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
HARNESS = ROOT / "scripts/bench-pi-runtime.py"


def load_harness():
    spec = importlib.util.spec_from_file_location("bench_pi_runtime_under_test", HARNESS)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def passing_profiles() -> dict:
    """Minimal profile summaries with every threshold metric measurable."""
    def summary(*values: float) -> dict:
        return {"count": len(values), "median": values[0], "p95": values[-1], "min": min(values), "max": max(values)}

    def run(rss: float) -> dict:
        return {"agent": {"active_extension_process": {"peak_rss_kib": rss}}}

    measurement = {
        "startup_readiness_ms": summary(40.0, 50.0),
        "first_activation_ms": summary(1.0, 2.0),
        "warm_call_ms": summary(0.2, 0.3),
        "process_restart_readiness_ms": summary(40.0, 50.0),
    }
    return {
        "no_extension": {"runs": [run(50_000.0)] * 5, "summary": dict(measurement)},
        "pi_aggregate": {"runs": [run(52_000.0)] * 5, "summary": dict(measurement)},
    }


class PiRuntimeEvidenceHarnessTests(unittest.TestCase):
    def test_one_repetition_emits_bounded_all_profile_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "evidence"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(HARNESS),
                    "--candidate",
                    "fixture-contract-test",
                    "--repetitions",
                    "1",
                    "--max-resource-samples",
                    "4",
                    "--output",
                    str(output),
                ],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
                timeout=60,
            )
            # One repetition is below the documented decision minimum, so the
            # derived status is `incomplete`, never a bare hold or pass.
            self.assertIn('"decision": "incomplete"', completed.stdout)
            artifact = json.loads((output / "results.json").read_text(encoding="utf-8"))
            self.assertEqual("octet.pi.runtime.evidence.v1", artifact["schema"])
            self.assertEqual("0.3", artifact["api"]["version"])
            self.assertEqual(
                {"no_extension", "legacy_eager", "lazy", "shared_workspace", "pi_aggregate"},
                set(artifact["profiles"]),
            )
            decision = artifact["release_decision"]
            self.assertEqual("octet.pi.runtime.decision.v1", decision["schema"])
            self.assertEqual("incomplete", decision["status"])
            self.assertEqual(1, decision["repetitions"])
            self.assertFalse(decision["release_approval"]["approved"])
            self.assertEqual(
                {"runtime_manager_adapter", "inference_attribution", "repetitions"},
                {gate["gate"] for gate in decision["release_approval"]["gates"]},
            )
            self.assertEqual(
                set(artifact["release_decision"]["thresholds"][0]),
                {"metric", "about", "unit", "observed", "limit", "status"},
            )
            self.assertTrue(all(row["status"] in {"pass", "fail", "unavailable"} for row in decision["thresholds"]))
            self.assertFalse(artifact["inference_server"]["included"])
            self.assertEqual("hermetic_fixture", artifact["inputs"]["adapter"])
            self.assertEqual("checked_in_fake_pi", artifact["inputs"]["pi_runtime"]["kind"])
            self.assertRegex(artifact["inputs"]["bridge"]["sha256"], r"^[0-9a-f]{64}$")
            for profile in artifact["profiles"].values():
                self.assertEqual(1, len(profile["runs"]))
                for samples in profile["runs"][0]["raw_resource_samples"].values():
                    self.assertLessEqual(len(samples), 4)
            self.assertTrue((output / "SHA256SUMS").is_file())

    def test_publish_requires_a_repository_benchmark_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            completed = subprocess.run(
                [
                    sys.executable,
                    str(HARNESS),
                    "--candidate",
                    "00e3ca3e561fc807491931712b93e534c952cf59",
                    "--repetitions",
                    "1",
                    "--publish",
                    "--output",
                    directory,
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertEqual(2, completed.returncode)
            self.assertIn("--publish requires an output directory inside docs/benchmarks/", completed.stderr)

    def test_publish_refuses_a_non_revision_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            completed = subprocess.run(
                [
                    sys.executable,
                    str(HARNESS),
                    "--candidate",
                    "not-a-revision",
                    "--repetitions",
                    "1",
                    "--publish",
                    "--output",
                    str(ROOT / "docs/benchmarks/.contract-test-must-not-exist"),
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertEqual(2, completed.returncode)
            self.assertIn("full lowercase 40-character commit", completed.stderr)
            self.assertFalse((ROOT / "docs/benchmarks/.contract-test-must-not-exist").exists())


class PiRuntimeDecisionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.harness = load_harness()
        self.thresholds = self.harness.threshold_settings([])

    def test_defaults_are_bounded_and_documented(self) -> None:
        self.assertEqual(len(self.harness.THRESHOLD_DEFAULTS), len(self.thresholds))
        for name, spec in self.thresholds.items():
            self.assertGreater(spec["limit"], 0, name)
            self.assertTrue(spec["unit"], name)
            self.assertTrue(spec["about"], name)

    def test_unknown_or_invalid_override_fails_closed(self) -> None:
        with self.assertRaises(self.harness.EvidenceError):
            self.harness.threshold_settings(["aggregate_warm_call_p95_ms"])  # missing =VALUE
        with self.assertRaises(self.harness.EvidenceError):
            self.harness.threshold_settings(["not_a_metric=1"])
        with self.assertRaises(self.harness.EvidenceError):
            self.harness.threshold_settings(["aggregate_warm_call_p95_ms=fast"])
        with self.assertRaises(self.harness.EvidenceError):
            self.harness.threshold_settings(["aggregate_warm_call_p95_ms=-1"])
        overridden = self.harness.threshold_settings(["aggregate_warm_call_p95_ms=12.5"])
        self.assertEqual(12.5, overridden["aggregate_warm_call_p95_ms"]["limit"])
        self.assertEqual(
            self.harness.THRESHOLD_DEFAULTS["aggregate_warm_call_p95_ms"]["limit"],
            self.thresholds["aggregate_warm_call_p95_ms"]["limit"],
        )

    def test_decision_is_derived_from_measured_thresholds(self) -> None:
        profiles = passing_profiles()
        passed = self.harness.release_decision(
            profiles, self.thresholds, 5, "hermetic_fixture", False
        )
        self.assertEqual("pass", passed["status"])
        self.assertFalse(passed["release_approval"]["approved"])
        self.assertTrue(all(row["status"] == "pass" for row in passed["thresholds"]))

        tightened = self.harness.threshold_settings(["aggregate_warm_call_p95_ms=0.25"])
        failed = self.harness.release_decision(profiles, tightened, 5, "hermetic_fixture", False)
        self.assertEqual("fail", failed["status"])
        self.assertIn("aggregate_warm_call_p95_ms", {row["metric"] for row in failed["thresholds"] if row["status"] == "fail"})
        self.assertTrue(any("exceeded" in reason for reason in failed["reasons"]))

        incomplete = self.harness.release_decision(profiles, self.thresholds, 1, "hermetic_fixture", False)
        self.assertEqual("incomplete", incomplete["status"])
        self.assertIn("repetitions", {gate["gate"] for gate in incomplete["release_approval"]["gates"]})

    def test_unmeasurable_metric_is_unavailable_not_estimated(self) -> None:
        profiles = passing_profiles()
        for run in profiles["pi_aggregate"]["runs"]:
            run["agent"]["active_extension_process"]["peak_rss_kib"] = None
        rows = {row["metric"]: row for row in self.harness.threshold_observations(profiles, self.thresholds)}
        self.assertEqual("unavailable", rows["aggregate_peak_rss_delta_kib"]["status"])
        self.assertIsNone(rows["aggregate_peak_rss_delta_kib"]["observed"])
        decision = self.harness.release_decision(profiles, self.thresholds, 5, "hermetic_fixture", False)
        self.assertEqual("incomplete", decision["status"])

    def test_measured_metric_rejects_unknown_names(self) -> None:
        with self.assertRaises(self.harness.EvidenceError):
            self.harness.measured_metric(passing_profiles(), "aggregate_nonexistent_p95_ms")


if __name__ == "__main__":
    unittest.main()
