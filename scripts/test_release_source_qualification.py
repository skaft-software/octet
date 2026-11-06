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
        block = self.workflow[self.workflow.index("          # Qualification must name the immutable source"):]
        self.gate = textwrap.dedent(block.split('          if [[ "$mode" != installer ]]; then', 1)[0])

    def run_gate(self, *, run="42", sha=SOURCE, status="completed", conclusion="success", jobs=None,
                 event="workflow_dispatch", force=""):
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is required")
        stubs = r'''
set -euo pipefail
source_commit="$TEST_SOURCE"
workflow_commit="$TEST_SOURCE"
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
                "GITHUB_EVENT_NAME": event, "INPUT_FORCE_RELEASE": force,
                "GITHUB_ACTOR": "release-owner", "GITHUB_SERVER_URL": "https://github.com",
                "GITHUB_RUN_ID": "123", "GITHUB_STEP_SUMMARY": str(Path(temporary) / "summary"),
                "TEST_RUN": run, "TEST_SHA": sha, "TEST_STATUS": status,
                "TEST_CONCLUSION": conclusion, "TEST_REQUESTS": str(requests),
                "TEST_JOBS": jobs if jobs is not None else "\n".join(f"{name}\tsuccess" for name in REQUIRED),
            }
            result = subprocess.run([bash, "-c", stubs + self.gate], env=env,
                                    capture_output=True, text=True, timeout=10)
            summary = Path(env["GITHUB_STEP_SUMMARY"])
            self.summary = summary.read_text() if summary.exists() else ""
            return result, requests.read_text() if requests.exists() else ""

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

    def test_manual_override_is_explicit_default_off_and_audited(self):
        inputs = self.workflow.split("      force_release:\n", 1)[1].split("      require_provider_acceptance:", 1)[0]
        self.assertIn("default: false", inputs)
        self.assertIn("type: boolean", inputs)
        self.assertIn("INPUT_FORCE_RELEASE: ${{ inputs.force_release }}", self.workflow)
        for force in ("", "false", "TRUE", "1"):
            with self.subTest(force=force):
                result, _ = self.run_gate(force=force, conclusion="failure")
                self.assertNotEqual(0, result.returncode)
        result, requests = self.run_gate(force="true", conclusion="failure")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual("", requests)
        self.assertIn("::warning::force_release=true", result.stdout)
        self.assertIn("WAIVED, not passed", self.summary)
        self.assertIn(SOURCE, self.summary)
        self.assertIn("release-owner", self.summary)
        self.assertIn("/actions/runs/123", self.summary)
        self.assertNotIn("qualified immutable", result.stdout)

    def test_override_is_ignored_for_push_and_pull_request_events(self):
        for event in ("push", "pull_request"):
            with self.subTest(event=event):
                result, requests = self.run_gate(event=event, force="true", conclusion="failure")
                self.assertNotEqual(0, result.returncode)
                self.assertIn("/ci.yml/runs", requests)
                self.assertEqual("", self.summary)

    def run_forced_resolve(self, **overrides):
        script = textwrap.dedent(self.workflow.split("        run: |\n", 1)[1].split("\n  security:", 1)[0])
        stubs = r'''
set -euo pipefail
git() {
  case "$1" in
    fetch) return 0 ;;
    rev-parse)
      if [[ "$2" == "$GITHUB_SHA^{commit}" ]]; then
        printf '%s\n' "$GITHUB_SHA"
      else
        printf '%s\n' "$TEST_SOURCE"
      fi ;;
    merge-base) [[ "$TEST_ANCESTOR" == true ]] ;;
    diff)
      if [[ "$2" == --name-only ]]; then
        printf '%s\n' "$TEST_PATHS"
      else
        [[ "$TEST_WORKFLOW_CHANGED" != true ]]
      fi ;;
    show) printf 'version = "%s"\n' "$TEST_VERSION" ;;
    *) return 98 ;;
  esac
}
gh() {
  [[ "$1 $2" == 'release view' ]] || return 99
  printf '%s\t%s\t%s\n' "$TEST_TAG" "$TEST_DRAFT" "$TEST_PRERELEASE"
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "output"
            env = {
                "PATH": os.defpath, "HOME": temporary,
                "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF_TYPE": "tag",
                "GITHUB_REF_NAME": "octet-binaries-v0.10.0", "GITHUB_SHA": "b" * 40,
                "GITHUB_REF": "refs/tags/octet-binaries-v0.10.0",
                "GITHUB_ACTOR": "release-owner", "GITHUB_SERVER_URL": "https://github.com",
                "GITHUB_REPOSITORY": "skaft-software/octet", "GITHUB_RUN_ID": "123",
                "INPUT_RELEASE_TAG": "v0.10.0", "INPUT_FORCE_RELEASE": "true",
                "INPUT_REQUIRE_PROVIDER_ACCEPTANCE": "false", "INPUT_PUBLISH_NPM": "false",
                "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(Path(temporary) / "summary"),
                "TEST_SOURCE": SOURCE, "TEST_ANCESTOR": "true",
                "TEST_PATHS": ".github/workflows/release-octet.yml", "TEST_WORKFLOW_CHANGED": "true",
                "TEST_VERSION": "0.10.0", "TEST_TAG": "v0.10.0",
                "TEST_DRAFT": "false", "TEST_PRERELEASE": "false",
            }
            env.update(overrides)
            result = subprocess.run([shutil.which("bash"), "-c", stubs + script], env=env,
                                    capture_output=True, text=True, timeout=10)
            return result, output.read_text() if output.exists() else ""

    def test_manual_tooling_tag_keeps_the_canonical_product_source(self):
        result, output = self.run_forced_resolve()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn(f"source_commit={SOURCE}\n", output)
        self.assertIn(f"workflow_commit={'b' * 40}\n", output)
        self.assertIn("mode=binaries\n", output)

    def test_force_cannot_bypass_source_release_or_provider_checks(self):
        for overrides in (
            {"GITHUB_REF_TYPE": "branch", "GITHUB_REF_NAME": "main"},
            {"GITHUB_REF_NAME": "octet-binaries-v0.9.6"},
            {"GITHUB_REF_NAME": "v0.10.0"},
            {"TEST_ANCESTOR": "false"},
            {"TEST_PATHS": "Cargo.toml"},
            {"TEST_WORKFLOW_CHANGED": "false"},
            {"TEST_VERSION": "0.9.6"},
            {"TEST_TAG": "v0.9.6"},
            {"TEST_DRAFT": "true"},
            {"TEST_PRERELEASE": "true"},
            {"INPUT_RELEASE_TAG": "v0.10.0;echo unsafe"},
            {"INPUT_REQUIRE_PROVIDER_ACCEPTANCE": "true"},
        ):
            with self.subTest(overrides=overrides):
                result, output = self.run_forced_resolve(**overrides)
                self.assertNotEqual(0, result.returncode)
                self.assertEqual("", output)

    def test_override_does_not_change_build_or_signing_dependencies(self):
        self.assertIn("needs: [resolve, security]", self.workflow)
        self.assertIn("needs: [resolve, build, build-windows]", self.workflow)
        self.assertIn("environment: stable-release-publish", self.workflow)
        self.assertIn('sha256sum --check OCTET_SHA256SUMS', self.workflow)
        self.assertIn('--certificate-github-workflow-sha "$WORKFLOW_COMMIT"', self.workflow)

    def test_gate_precedes_resolve_outputs_and_is_not_provider_opt_in(self):
        start = self.workflow.index("# Qualification must name the immutable source")
        self.assertLess(start, self.workflow.index('echo "source_commit=$source_commit"'))
        self.assertLess(start, self.workflow.index('if [[ "${INPUT_REQUIRE_PROVIDER_ACCEPTANCE:-false}" == true ]]'))
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("python3 scripts/test_release_source_qualification.py", ci)


if __name__ == "__main__":
    unittest.main()
