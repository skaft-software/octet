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
                 event="workflow_dispatch", force="", reason="maintenance", runs=None, statuses=None):
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is required")
        stubs = r'''
set -euo pipefail
source_commit="$TEST_SOURCE"
workflow_commit="$TEST_SOURCE"
tag=v0.10.0
sleep() { :; }
gh() {
  printf '%s\n' "$*" >> "$TEST_REQUESTS"
  case "$*" in
    *'/ci.yml/runs'*)
      count=$(cat "$TEST_COUNTER" 2>/dev/null || echo 0)
      count=$((count + 1))
      printf '%s' "$count" > "$TEST_COUNTER"
      run=$(printf '%s\n' "$TEST_RUNS" | sed -n "${count}p")
      printf '%s\n' "${run:-$TEST_RUN}" ;;
    *'/jobs?'*) printf '%s\n' "$TEST_JOBS" ;;
    *'/actions/runs/'*)
      count=$(cat "$TEST_COUNTER")
      status=$(printf '%s\n' "$TEST_STATUSES" | sed -n "${count}p")
      printf '%s\t%s\t%s\n' "$TEST_SHA" "${status:-$TEST_STATUS}" "$TEST_CONCLUSION" ;;
    *) return 99 ;;
  esac
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            requests = Path(temporary) / "requests"
            env = {
                "PATH": os.defpath, "HOME": temporary,
                "GITHUB_REPOSITORY": "skaft-software/octet", "TEST_SOURCE": SOURCE,
                "GITHUB_EVENT_NAME": event, "GITHUB_REF_NAME": "v0.10.0",
                "INPUT_FORCE_RELEASE": force, "INPUT_FORCE_REASON": reason,
                "GITHUB_ACTOR": "release-owner", "GITHUB_SERVER_URL": "https://github.com",
                "GITHUB_RUN_ID": "123", "GITHUB_STEP_SUMMARY": str(Path(temporary) / "summary"),
                "TEST_RUN": run, "TEST_SHA": sha, "TEST_STATUS": status,
                "TEST_CONCLUSION": conclusion, "TEST_REQUESTS": str(requests),
                "TEST_COUNTER": str(Path(temporary) / "count"),
                "TEST_RUNS": "\n".join(runs or []), "TEST_STATUSES": "\n".join(statuses or []),
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
        self.assertIn('.event == "push" and .head_branch == "main"', requests)
        self.assertIn('.event == "merge_group"', requests)
        self.assertIn('.event == "workflow_dispatch"', requests)

    def test_missing_wrong_failed_or_pending_source_is_rejected(self):
        for overrides in ({"run": ""}, {"run": "invalid"}, {"sha": "b" * 40},
                          {"status": "in_progress"}, {"conclusion": "failure"},
                          {"conclusion": "cancelled"}):
            with self.subTest(overrides=overrides):
                result, _ = self.run_gate(**overrides)
                self.assertNotEqual(0, result.returncode)

    def test_canonical_push_requires_main_push_ci_not_earlier_dispatch(self):
        result, requests = self.run_gate(event="push")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn('.event == "push" and .head_branch == "main"', requests)
        self.assertNotIn('.event == "merge_group"', requests)
        self.assertNotIn('.event == "workflow_dispatch"', requests)

    def test_waits_for_run_and_completion_then_qualifies_latest(self):
        result, requests = self.run_gate(run="", runs=["", "41", "42"],
                                         statuses=["queued", "in_progress", "completed"])
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual(3, requests.count("/ci.yml/runs"))
        self.assertIn("/actions/runs/42/jobs", requests)
        self.assertIn("waiting for", result.stdout)

    def test_missing_or_running_ci_times_out_with_clear_message(self):
        for overrides in ({"run": ""}, {"status": "in_progress"}):
            with self.subTest(overrides=overrides):
                result, requests = self.run_gate(**overrides)
                self.assertNotEqual(0, result.returncode)
                self.assertIn("timed out after 75 minutes", result.stderr)
                self.assertEqual(151, requests.count("/ci.yml/runs"))

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
        self.assertIn("::warning::Source CI qualification WAIVED: maintenance by release-owner", result.stdout)
        self.assertIn("Source CI qualification WAIVED: maintenance by release-owner", self.summary)
        self.assertIn("WAIVED, not passed", self.summary)
        self.assertIn(SOURCE, self.summary)
        self.assertIn("release-owner", self.summary)
        self.assertIn("/actions/runs/123", self.summary)
        self.assertNotIn("qualified immutable", result.stdout)

    def test_force_requires_nonempty_single_line_reason(self):
        for reason in ("", "  \t ", "first\nsecond", "first\rsecond"):
            with self.subTest(reason=reason):
                result, requests = self.run_gate(force="true", reason=reason)
                self.assertNotEqual(0, result.returncode)
                self.assertIn("force_reason", result.stderr)
                self.assertEqual("", requests)
                self.assertEqual("", self.summary)

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
  case "$*" in
    'release view '*)
      [[ "$TEST_MISSING_RELEASE" != true ]] || return 1
      printf '%s\t%s\t%s\n' "$TEST_TAG" "$TEST_DRAFT" "$TEST_PRERELEASE" ;;
    *'/ci.yml/runs'*) printf '42\n' ;;
    *'/jobs?'*) printf '%s\n' "$TEST_JOBS" ;;
    *'/actions/runs/42'*) printf '%s\tcompleted\tsuccess\n' "$TEST_SOURCE" ;;
    *) return 99 ;;
  esac
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
                "INPUT_RELEASE_TAG": "v0.10.0", "INPUT_FORCE_RELEASE": "true", "INPUT_FORCE_REASON": "maintenance",
                "INPUT_REQUIRE_PROVIDER_ACCEPTANCE": "false", "INPUT_PUBLISH_NPM": "false",
                "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(Path(temporary) / "summary"),
                "TEST_SOURCE": SOURCE, "TEST_ANCESTOR": "true",
                "TEST_PATHS": ".github/workflows/release-octet.yml", "TEST_WORKFLOW_CHANGED": "true",
                "TEST_VERSION": "0.10.0", "TEST_TAG": "v0.10.0",
                "TEST_DRAFT": "false", "TEST_PRERELEASE": "false", "TEST_MISSING_RELEASE": "false",
                "TEST_JOBS": "\n".join(f"{name}\tsuccess" for name in REQUIRED),
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

    def test_canonical_push_can_start_before_github_release_exists(self):
        result, output = self.run_forced_resolve(
            GITHUB_EVENT_NAME="push", GITHUB_REF_NAME="v0.10.0", GITHUB_REF="refs/tags/v0.10.0",
            GITHUB_SHA=SOURCE, INPUT_RELEASE_TAG="", INPUT_FORCE_RELEASE="false", TEST_MISSING_RELEASE="true",
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn(f"source_commit={SOURCE}\n", output)
        self.assertIn("tag=v0.10.0\n", output)

    def test_forced_tooling_repair_can_create_a_missing_release(self):
        result, output = self.run_forced_resolve(TEST_MISSING_RELEASE="true")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("tag=v0.10.0\n", output)

    def test_source_must_be_on_main_even_when_force_is_set(self):
        result, output = self.run_forced_resolve(TEST_ANCESTOR="false")
        self.assertNotEqual(0, result.returncode)
        self.assertIn("release tags must point at a commit on main", result.stderr)
        self.assertEqual("", output)

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

    def test_canonical_push_creates_release_only_after_ref_validation(self):
        step = self.workflow.split('      - name: Attach assets to canonical release\n', 1)[1]
        script = textwrap.dedent(step.split('        run: |\n', 1)[1].split('\n  verify-published:', 1)[0])
        stubs = r'''
set -euo pipefail
git() {
  case "$1" in
    fetch) : ;;
    rev-parse) printf '%s\n' "$SOURCE_COMMIT" ;;
    *) return 98 ;;
  esac
}
gh() {
  printf '%s\n' "$*" >> "$TEST_REQUESTS"
  case "$1 $2" in
    'release view')
      if [[ "$*" == *'--json assets'* ]]; then return 0; fi
      [[ -e "$TEST_RELEASE" ]] || return 1
      printf '%s\tfalse\tfalse\n' "$RELEASE_TAG" ;;
    'release create')
      [[ "$*" == *'--verify-tag'* && "$*" == *'--notes-file docs/releases/v0.10.3.md'* ]] || return 1
      touch "$TEST_RELEASE" ;;
    'release upload') : ;;
    *) return 99 ;;
  esac
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "release-assets").mkdir()
            (root / "release-assets" / "artifact").write_text("fixture")
            requests = root / "requests"
            env = {"PATH": os.defpath, "GITHUB_EVENT_NAME": "push", "GITHUB_REF_TYPE": "tag",
                   "RELEASE_TAG": "v0.10.3",
                   "SOURCE_COMMIT": SOURCE, "WORKFLOW_COMMIT": SOURCE, "WORKFLOW_TAG": "v0.10.3",
                   "INPUT_FORCE_RELEASE": "", "INPUT_FORCE_REASON": "", "RUNNER_TEMP": temporary,
                   "TEST_RELEASE": str(root / "release"), "TEST_REQUESTS": str(requests)}
            result = subprocess.run(["bash", "-c", stubs + script], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(0, result.returncode, result.stderr)
            calls = requests.read_text()
            self.assertIn('release create v0.10.3 --verify-tag', calls)
            self.assertIn('release upload v0.10.3', calls)

    def test_release_notes_waiver_preserves_existing_text_and_is_idempotent(self):
        start = self.workflow.index('          if [[ "$GITHUB_EVENT_NAME" == workflow_dispatch && "$INPUT_FORCE_RELEASE" == true ]]; then',
                                    self.workflow.index('      - name: Attach assets to canonical release'))
        end = self.workflow.index('          existing_digests=', start)
        block = textwrap.dedent(self.workflow[start:end])
        stubs = r'''
set -euo pipefail
gh() {
  if [[ "$1 $2" == "release view" ]]; then
    cat "$TEST_BODY"
  elif [[ "$1 $2" == "release edit" ]]; then
    cp "$5" "$TEST_BODY"
    echo edit >> "$TEST_EDITS"
  else
    return 99
  fi
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            body, edits = Path(temporary) / "body", Path(temporary) / "edits"
            body.write_text("Existing notes\n")
            env = {"PATH": os.defpath, "RUNNER_TEMP": temporary, "TEST_BODY": str(body),
                   "TEST_EDITS": str(edits), "GITHUB_EVENT_NAME": "workflow_dispatch",
                   "INPUT_FORCE_RELEASE": "true", "INPUT_FORCE_REASON": "CI incident #42",
                   "GITHUB_ACTOR": "release-owner", "RELEASE_TAG": "v0.10.0"}
            for _ in range(2):
                result = subprocess.run(["bash", "-c", stubs + block], env=env,
                                        capture_output=True, text=True)
                self.assertEqual(0, result.returncode, result.stderr)
            self.assertEqual(body.read_text(), "Existing notes\n\n**Source CI qualification WAIVED: CI incident #42 by release-owner**\n")
            self.assertEqual(edits.read_text().splitlines(), ["edit"])
        self.assertEqual(2, self.workflow.count('name: Record a source CI waiver in the release notes'))

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
