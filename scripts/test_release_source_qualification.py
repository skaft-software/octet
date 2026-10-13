#!/usr/bin/env python3
"""Offline tests of the release workflow's exact-source CI gate; no GitHub writes."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parent.parent
SOURCE = "a" * 40
REQUIRED = (
    "dependencies", "quality", "test (ubuntu-24.04)", "test (macos-15)",
    "windows (x86_64-pc-windows-gnu)", "first-party-extension-tests",
    "extension-api-v03", "msrv", "smoke-install",
)


class ReleaseSourceQualificationTests(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / ".github/workflows/release-octet.yml").read_text()
        block = self.workflow[self.workflow.index("          ci_run=$(gh api --method GET"):]
        self.gate = textwrap.dedent(block.split('          if [[ "$mode" != installer ]]; then', 1)[0])

    def run_gate(self, *, run="42", sha=SOURCE, status="completed", conclusion="success", jobs=None):
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is required")
        stubs = r'''
set -euo pipefail
source_commit="$TEST_SOURCE"
gh() {
  printf '%s\n' "$*" >> "$TEST_REQUESTS"
  case "$*" in
    *'/ci.yml/runs'*) printf '%s\n' "$TEST_RUN" ;;
    *'/jobs?'*) printf '%s\n' "$TEST_JOBS" ;;
    *'/actions/runs/'*) printf '%s\t%s\t%s\n' "$TEST_SHA" "$TEST_STATUS" "$TEST_CONCLUSION" ;;
    *) return 99 ;;
  esac
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            requests = Path(temporary) / "requests"
            env = {
                "PATH": os.defpath, "HOME": temporary,
                "GITHUB_REPOSITORY": "skaft-software/octet", "TEST_SOURCE": SOURCE,
                "TEST_RUN": run, "TEST_SHA": sha, "TEST_STATUS": status,
                "TEST_CONCLUSION": conclusion, "TEST_REQUESTS": str(requests),
                "TEST_JOBS": jobs if jobs is not None else "\n".join(f"{name}\tsuccess" for name in REQUIRED),
            }
            result = subprocess.run([bash, "-c", stubs + self.gate], env=env,
                                    capture_output=True, text=True, timeout=10)
            return result, requests.read_text()

    def test_exact_successful_source_with_all_required_lanes_passes(self):
        result, requests = self.run_gate()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn(f"head_sha={SOURCE}", requests)
        self.assertIn("--paginate", requests)
        self.assertIn("filter=latest", requests)
        self.assertIn("max_by(.id)", requests)
        self.assertNotIn("-f status=success", requests)
        self.assertIn('select(.event == "push" or .event == "workflow_dispatch")', requests)

    def test_missing_wrong_failed_or_pending_source_is_rejected(self):
        for overrides in ({"run": ""}, {"run": "invalid"}, {"sha": "b" * 40},
                          {"status": "in_progress"}, {"conclusion": "failure"},
                          {"conclusion": "cancelled"}):
            with self.subTest(overrides=overrides):
                result, _ = self.run_gate(**overrides)
                self.assertNotEqual(0, result.returncode)

    def test_each_required_lane_must_be_present_once_and_successful(self):
        for name in REQUIRED:
            for outcome in (None, "skipped", "failure", "cancelled", "duplicate"):
                jobs = [f"{check}\tsuccess" for check in REQUIRED if check != name]
                if outcome is not None:
                    jobs.append(f"{name}\t{'success' if outcome == 'duplicate' else outcome}")
                if outcome == "duplicate":
                    jobs.append(f"{name}\tsuccess")
                with self.subTest(name=name, outcome=outcome):
                    result, _ = self.run_gate(jobs="\n".join(jobs))
                    self.assertNotEqual(0, result.returncode)

    def test_gate_precedes_resolve_outputs_and_is_not_provider_opt_in(self):
        start = self.workflow.index("# Qualification must name the immutable source")
        self.assertLess(start, self.workflow.index('echo "source_commit=$source_commit"'))
        self.assertLess(start, self.workflow.index('if [[ "${INPUT_REQUIRE_PROVIDER_ACCEPTANCE:-false}" == true ]]'))
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("python3 scripts/test_release_source_qualification.py", ci)


if __name__ == "__main__":
    unittest.main()
