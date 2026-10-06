"""Isolated synthetic-process contract tests. No competitors/inference/terminal."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import random
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("release_comparison", ROOT / "bench-release-comparison.py")
B = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(B)
DRIVER = ROOT / "fixtures/release-comparison/synthetic-driver.py"
PYTHON = str(Path(sys.executable).resolve())


def pin(path):
    return {"path": str(Path(path).resolve()), "sha256": B.sha256(path)}


def fixture():
    setup = {key: "synthetic-common-v1" for key in B.MATCHED}
    setup.update(extensions=[], dependency_count=0, expected_child_processes=0)
    client = {"argv": [PYTHON, "-B", str(DRIVER), "version"], "version": "synthetic-1",
              "package_identity": "synthetic-not-a-competitor", "artifacts": [pin(PYTHON), pin(DRIVER)],
              "provenance": {"kind": "source_build", "source_revision": "synthetic-fixture-v1", "note": "Python fixture source; no product build", "artifacts": [pin(DRIVER)]}, "setup": setup, "home_files": []}
    return {"schema": B.SCHEMA, "frozen": True, "trials": 9, "warmups": 1, "timeout_seconds": 2,
            "seed": 14, "order": "alternating", "native_tern_approved": False,
            "host": {"hardware": "synthetic-hardware", "os": "synthetic-os", "dimensions": [80, 24],
                     "tern": {**pin(DRIVER), "version": "synthetic"}, "cache_policy": "fresh-home/warm-file-cache"},
            "clients": {"a": client, "b": copy.deepcopy(client)},
            "cases": [{"id": "version", "comparative": True, "metrics": ["version_exit_ms"],
                       "checks": ["version_matches"], "members": {name: {"mode": "version", "version_pattern": "synthetic-1"}
                                                                 for name in ("a", "b")}}]}


def driver_fixture(mode="pass"):
    manifest = fixture()
    case = manifest["cases"][0]
    case.update(id="ready", metrics=["pty_ready_ms"], checks=["history_exact", "application_edit", "shutdown_settled"])
    for member in case["members"].values():
        member.clear()
        member.update(mode="driver", argv=[PYTHON, "-B", str(DRIVER), mode],
                      artifacts=[pin(PYTHON), pin(DRIVER)], approved=True, network_policy="no sockets; synthetic only")
    return manifest


def records(manifest):
    case = manifest["cases"][0]
    return [{"schema": B.DRIVER_SCHEMA, "type": "event", "name": "subject_spawn", "ns": 100},
            {"schema": B.DRIVER_SCHEMA, "type": "event", "name": "complete_ready_frame_received", "ns": 2000100},
            {"schema": B.DRIVER_SCHEMA, "type": "result", "setup": manifest["clients"]["a"]["setup"],
             "correctness": {key: True for key in case["checks"]},
             "metrics": {"pty_ready_ms": {"events": [0, 1], "observer": "pty"}}}]


class ContractTests(unittest.TestCase):
    def test_manifest_complete(self):
        B.validate(fixture(), True)
        B.validate(driver_fixture(), True)

    def test_incomplete_manifest_invalid_tools_and_missing_identity(self):
        mutations = [lambda m: m.update(frozen=False), lambda m: m.update(trials=8),
                     lambda m: m.update(timeout_seconds=float("nan")), lambda m: m.update(warmups=0),
                     lambda m: m["host"].pop("hardware"), lambda m: m["clients"]["a"].pop("provenance"),
                     lambda m: m["clients"]["a"].update(argv="/bin/echo --version"),
                     lambda m: m["clients"]["a"].update(argv=["/bin/sh", "-c", "echo danger"]),
                     lambda m: m["clients"]["a"]["artifacts"][0].update(path="/definitely/missing"),
                     lambda m: m["clients"]["a"]["artifacts"][0].update(sha256="f" * 64),
                     lambda m: m["clients"]["a"]["setup"].update(dependency_count=None),
                     lambda m: m["clients"]["a"]["setup"].update(expected_child_processes=None),
                     lambda m: m["cases"][0].update(metrics=["first_editable_ms"])]
        for mutate in mutations:
            m = fixture()
            mutate(m)
            with self.subTest(mutation=mutate), self.assertRaises((ValueError, KeyError)):
                B.validate(m, True)

    def test_non_equivalent_modes_refused_before_launch(self):
        m = fixture()
        m["clients"]["a"]["setup"]["frontend"] = "codex-app-server"
        m["clients"]["b"]["setup"]["frontend"] = "pi-tui"
        self.assertIn("frontend", B.refusal(m, m["cases"][0]))
        with tempfile.TemporaryDirectory() as tmp, patch.object(B, "execute") as execute:
            output = Path(tmp) / "evidence"
            B.run(m, output)
            execute.assert_not_called()
            rows = [json.loads(line) for line in (output / "runs.jsonl").read_text().splitlines()]
            self.assertTrue(all(not row["comparative_qualified"] for row in rows if row["type"] == "summary"))

    def test_missing_and_wrong_clock_not_zero(self):
        m = driver_fixture()
        case, setup = m["cases"][0], m["clients"]["a"]["setup"]
        r = records(m)
        self.assertEqual(B.observation(r, case, setup)["metrics"]["pty_ready_ms"]["value"], 2)
        r[-1]["metrics"] = {}
        actual = B.observation(r, case, setup)["metrics"]["pty_ready_ms"]
        self.assertIsNone(actual["value"])
        self.assertEqual(actual["reason"], "missing_metric")
        r = records(m)
        r[-1]["metrics"]["pty_ready_ms"]["observer"] = "native_tern"
        self.assertEqual(B.observation(r, case, setup)["metrics"]["pty_ready_ms"]["reason"], "non_equivalent_clock")

    def test_effective_setup_and_failed_correctness(self):
        m = driver_fixture()
        case, setup = m["cases"][0], m["clients"]["a"]["setup"]
        r = records(m)
        r[-1]["correctness"]["history_exact"] = False
        actual = B.observation(r, case, setup)
        self.assertEqual(actual["status"], "failed")
        self.assertIs(actual["correctness"]["history_exact"], False)
        r = copy.deepcopy(records(m))
        r[-1]["setup"]["extensions"] = ["unrecorded"]
        self.assertEqual(B.observation(r, case, setup)["reason"], "effective_setup_mismatch")

    def test_monotonic_events_and_duplicate_result(self):
        m = driver_fixture()
        r = records(m)
        r[1]["ns"] = 1
        self.assertEqual(B.observation(r, m["cases"][0], m["clients"]["a"]["setup"])["reason"], "invalid_monotonic_events")
        r = records(m)
        r.append(r[-1])
        self.assertEqual(B.observation(r, m["cases"][0], m["clients"]["a"]["setup"])["reason"], "missing_or_duplicate_result")

    def test_percentiles_use_run_not_frames_failures_retained(self):
        runs = [{"warmup": False, "status": "completed", "metrics": {"pty_ready_ms": {"value": i}},
                 "frames": [1] * 500} for i in range(1, 10)]
        actual = B.summary(runs, "pty_ready_ms")
        self.assertEqual(actual["median"], 5)
        self.assertEqual(actual["p95"], 8.6)
        self.assertTrue(actual["qualified"])
        runs += [{"warmup": True, "status": "failed", "metrics": {}}]
        self.assertEqual(B.summary(runs, "pty_ready_ms")["warmup_failures"], 1)
        self.assertFalse(B.summary(runs, "pty_ready_ms")["qualified"])
        runs += [{"warmup": False, "status": "failed", "metrics": {"pty_ready_ms": {"value": 0}}}]
        actual = B.summary(runs, "pty_ready_ms")
        self.assertEqual(actual["failed"], 1)
        self.assertEqual(actual["completed_observations"], 9)
        self.assertFalse(actual["qualified"])
        self.assertIsNone(B.summary([], "pty_ready_ms")["median"])

    def test_resources_require_descendant_evidence(self):
        m = driver_fixture()
        case = copy.deepcopy(m["cases"][0])
        case["metrics"] = ["rss_peak_bytes", "pss_peak_bytes", "allocation_calls"]
        r = records(m)
        r[-1]["metrics"] = {
            "rss_peak_bytes": {"scope": "direct_pids", "unit": "bytes", "samples": [{"value": 123}]},
            "pss_peak_bytes": {"value": None, "reason": "OS does not expose PSS"},
            "allocation_calls": {"scope": "allocator_traffic", "unit": "calls", "method": "counting allocator interval increments", "samples": [{"value": 2, "ns": 1}, {"value": 3, "ns": 2}]}}
        actual = B.observation(r, case, m["clients"]["a"]["setup"])
        self.assertIsNone(actual["metrics"]["rss_peak_bytes"]["value"])
        self.assertIsNone(actual["metrics"]["pss_peak_bytes"]["value"])
        self.assertEqual(actual["metrics"]["allocation_calls"]["value"], 5)
        r[-1]["metrics"]["rss_peak_bytes"].update(scope="agent_and_all_descendants", tree_complete=True, method="synthetic census",
                  samples=[{"value": 123, "ns": 1, "pids": [101, 102], "per_pid": {"101": 100, "102": 23}, "complete": True}])
        self.assertEqual(B.observation(r, case, m["clients"]["a"]["setup"])["metrics"]["rss_peak_bytes"]["value"], 123)

    def test_strict_json_and_home_symlink_do_not_read_outside(self):
        with self.assertRaises(ValueError):
            B.parse_json('{"value":NaN}')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            home.mkdir()
            (home / "outside").symlink_to("/definitely/unreadable-private-credentials")
            with patch.object(B, "sha256") as digest, self.assertRaisesRegex(ValueError, "symlink"):
                B.home_hashes(home)
            digest.assert_not_called()

    def test_native_paint_needs_approval(self):
        m = driver_fixture()
        m["cases"][0]["metrics"] = ["input_to_paint_ms"]
        with self.assertRaisesRegex(ValueError, "Tern"):
            B.validate(m)

    def test_reproducible_trial_order(self):
        a, b = random.Random(17), random.Random(17)
        self.assertEqual([B.ordered(["a", "b", "c"], i, "random", a) for i in range(9)],
                         [B.ordered(["a", "b", "c"], i, "random", b) for i in range(9)])
        orders = [B.ordered(["a", "b", "c"], i, "alternating", a) for i in range(6)]
        for client in ("a", "b", "c"):
            self.assertEqual([order[0] for order in orders].count(client), 2)

    def test_fixture_template_refuses_unfrozen_candidate(self):
        m = B.load(ROOT / "fixtures/release-comparison/manifest.template.json")
        with self.assertRaisesRegex(ValueError, "frozen"):
            B.validate(m)
        self.assertEqual(set(m["cases"][4]["members"]), {"pi", "octet", "pig"})
        self.assertEqual(set(m["cases"][1]["members"]), {"pi", "octet", "codex"})

    def test_home_escape_and_bad_digest_refused(self):
        for path in ("../credentials", "/user-home/file", "."):
            m = fixture()
            m["clients"]["a"]["home_files"] = [{"path": path, "content": "{}", "sha256": "f" * 64}]
            with self.assertRaises(ValueError):
                B.validate(m)


class IsolatedProcessTests(unittest.TestCase):
    def execute(self, root, args, timeout=2):
        env = B.environment(root / "home")
        return B.execute([PYTHON, "-B", str(DRIVER), *args], root, env, timeout, subprocess.DEVNULL, root)

    def test_credentials_home_and_argument_injection(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {
            "OPENAI_API_KEY": "secret", "NODE_OPTIONS": "danger", "HOME": "/private-user",
            "SSH_AUTH_SOCK": "/private-socket", "HTTPS_PROXY": "private-proxy", "CODEX_HOME": "/private-codex"}):
            root = Path(tmp)
            injected = f'; touch {root / "INJECTION"}'
            result = self.execute(root, ["echo", injected, "$(echo danger)"])
            self.assertEqual(result["status"], "completed")
            actual = json.loads((root / "stdout.bin").read_text())
            self.assertEqual(actual["argv"], [injected, "$(echo danger)"])
            self.assertFalse((root / "INJECTION").exists())
            self.assertEqual(actual["environment"]["HOME"], str(root / "home"))
            self.assertEqual(actual["environment"]["CODEX_HOME"], str(root / "home/.codex"))
            for key in ("OPENAI_API_KEY", "NODE_OPTIONS", "SSH_AUTH_SOCK", "HTTPS_PROXY"):
                self.assertNotIn(key, actual["environment"])

    def test_absent_process_is_retained(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            result = B.execute(["/definitely/missing"], root, B.environment(root / "home"), 1, subprocess.DEVNULL, root)
            self.assertEqual(result["reason"], "launch_failed")
            self.assertTrue((root / "stderr.bin").is_file())

    def test_timeout_own_group_cleanup_and_partial_capture(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            result = self.execute(root, ["timeout"], .4)
            self.assertEqual(result["reason"], "benchmark_timeout")
            self.assertTrue(result["cleanup_reaped"])
            self.assertIn(b"partial-timeout", (root / "stdout.bin").read_bytes())
            pid = int((root / "child.pid").read_text())
            # On Linux a reparented killed child can remain a zombie temporarily;
            # a zombie is not a running continuing process. Do not signal it.
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                probe = subprocess.run(["/bin/ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True, timeout=1)
                if not probe.stdout.strip() or probe.stdout.strip().startswith("Z"):
                    break
                time.sleep(.02)
            self.assertTrue(not probe.stdout.strip() or probe.stdout.strip().startswith("Z"))

    def test_output_limit_preserves_bounded_capture(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(B, "LIMIT", 64):
            root = Path(tmp)
            result = self.execute(root, ["echo", "x" * 200])
            self.assertEqual(result["reason"], "capture_limit")
            self.assertEqual((root / "stdout.bin").stat().st_size, 64)
            self.assertTrue(result["capture_truncated"])

    def test_fabricated_driver_cannot_qualify_comparison(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            B.run(driver_fixture(), root / "evidence")
            rows = [json.loads(line) for line in (root / "evidence/runs.jsonl").read_text().splitlines()]
            self.assertFalse(rows[-1]["comparative_qualified"])
            self.assertIn("fabricated", rows[-1]["comparison_refused"])

    def test_cli_retains_evidence_and_returns_nonzero_for_missing_metrics(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            manifest = driver_fixture("missing")
            manifest["cases"][0]["members"].pop("b")
            manifest["cases"][0]["comparative"] = False
            B.dump(root / "manifest.json", manifest)
            with patch.object(sys, "argv", [str(ROOT / "bench-release-comparison.py"), "run", "--manifest", str(root / "manifest.json"), "--output", str(root / "evidence")]):
                self.assertEqual(B.main(), 1)
            self.assertTrue((root / "evidence/runs.jsonl").is_file())

    def test_collision_preserves_old_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            evidence = root / "evidence"
            evidence.mkdir()
            (evidence / "sentinel").write_text("KEEP")
            with patch.object(B, "execute") as execute, self.assertRaises(FileExistsError):
                B.run(fixture(), evidence)
            execute.assert_not_called()
            self.assertEqual((evidence / "sentinel").read_text(), "KEEP")
            path = root / "manifest.json"
            B.dump(path, {"keep": True})
            with self.assertRaises(FileExistsError):
                B.dump(path, {})
            self.assertEqual(B.load(path), {"keep": True})

    def test_campaign_raw_failure_missing_and_export_are_separate(self):
        for mode in ("incorrect", "missing", "nonzero", "pass"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                manifest = driver_fixture(mode)
                # Nine independent synthetic trials, not a competitor campaign.
                manifest["cases"][0]["members"].pop("b")
                manifest["cases"][0]["comparative"] = False
                output = root / "private"
                B.run(manifest, output)
                rows = [json.loads(line) for line in (output / "runs.jsonl").read_text().splitlines()]
                trials = [row for row in rows if row["type"] == "run"]
                self.assertEqual(len(trials), 10)
                self.assertTrue(trials[0]["warmup"])
                self.assertEqual(len({row["environment"]["HOME"] for row in trials}), 10)
                result = rows[-1]["clients"]["a"]["pty_ready_ms"]
                self.assertEqual(result["qualified"], mode == "pass")
                if mode == "incorrect":
                    self.assertIs(trials[1]["correctness"]["history_exact"], False)
                    self.assertEqual(result["failed"], 9)
                if mode == "missing":
                    self.assertEqual(result["missing"], 9)
                    self.assertIsNone(result["median"])
                if mode == "nonzero":
                    self.assertEqual(trials[1]["reason"], "driver_nonzero_exit")
                original = B.sha256(output / "runs.jsonl")
                public = root / "public"
                B.export(output, public, {str(root): "$PRIVATE_ROOT"})
                self.assertEqual(original, B.sha256(output / "runs.jsonl"))
                self.assertFalse(any(p.name in ("home", "stdout.bin", "request.json") for p in public.rglob("*")))
                for path in public.iterdir():
                    self.assertNotIn(str(root), path.read_text())
                self.assertEqual((output / "runs.jsonl").stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
