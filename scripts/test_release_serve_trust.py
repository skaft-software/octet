#!/usr/bin/env python3
"""Offline regression checks for Serve release source and execution authority."""

import os
import pathlib
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/release-serve.yml"
SOURCE = "a" * 40
TOOLING = "b" * 40


class ServeReleaseTrustTests(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text(encoding="utf-8")
        self.jobs = dict(re.findall(
            r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)",
            self.workflow.split("\njobs:\n", 1)[1],
            re.MULTILINE | re.DOTALL,
        ))

    def test_branch_dispatch_cannot_allocate_release_runners(self):
        self.assertIn("    if: github.ref_type == 'tag'\n", self.jobs["resolve"])
        for name, job in self.jobs.items():
            if name != "resolve":
                self.assertRegex(job, r"    needs: (?:resolve|\[[^\n]*\bresolve\b[^\n]*\])\n")
                self.assertNotIn("always()", job)

    def test_dispatch_executes_trigger_sha_not_input_selected_source(self):
        for name in ("security", "build", "build-bundles", "publish", "verify-published"):
            with self.subTest(job=name):
                checkouts = re.findall(
                    r"^      - uses: actions/checkout@[^\n]+\n(.*?)(?=^      - |\Z)",
                    self.jobs[name], re.MULTILINE | re.DOTALL,
                )
                self.assertEqual(1, len(checkouts))
                self.assertIn(
                    "ref: ${{ github.event_name == 'push' && "
                    "needs.resolve.outputs.source_commit || github.sha }}\n",
                    checkouts[0],
                )

    def test_no_release_cache_actions_or_implicit_node_caches(self):
        self.assertNotRegex(self.workflow, r"uses: (?:actions/cache|Swatinem/rust-cache)[@/]")
        self.assertNotRegex(self.workflow, re.compile(r"^\s+cache:", re.MULTILINE))

    def test_signing_remains_protected_and_actions_remain_pinned(self):
        self.assertIn("environment: stable-release-publish", self.jobs["publish"])
        self.assertIn("contents: write", self.jobs["publish"])
        self.assertIn("id-token: write", self.jobs["publish"])
        for name, job in self.jobs.items():
            if name != "publish":
                self.assertNotIn("contents: write", job)
                self.assertNotIn("id-token: write", job)
        for action in re.findall(r"^\s+- uses: (\S+)", self.workflow, re.MULTILINE):
            self.assertRegex(action, r"@[a-f0-9]{40}$")

    def run_resolve(self, *, event="workflow_dispatch", ref_type="tag", ref="v0.8.2",
                    tag="v0.8.2", workflow_sha=SOURCE, ancestor=True,
                    changed_paths=".github/workflows/release-serve.yml",
                    serve_changed=True, version="0.8.2", draft="false", prerelease="false"):
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is required")
        script = textwrap.dedent(self.jobs["resolve"].split("        run: |\n", 1)[1])
        # Stub only external Git/GitHub queries. Execute the actual full gate,
        # without repository changes, network access, or ambient credentials.
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
        [[ "$TEST_SERVE_CHANGED" != true ]]
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
            output = pathlib.Path(temporary) / "output"
            env = {
                "PATH": os.defpath, "HOME": temporary,
                "GITHUB_EVENT_NAME": event, "GITHUB_REF_TYPE": ref_type,
                "GITHUB_REF_NAME": ref, "GITHUB_SHA": workflow_sha,
                "INPUT_RELEASE_TAG": tag, "INPUT_REQUIRE_PROVIDER_ACCEPTANCE": "false",
                "GITHUB_OUTPUT": str(output),
                "GITHUB_STEP_SUMMARY": str(pathlib.Path(temporary) / "summary"),
                "TEST_SOURCE": SOURCE, "TEST_ANCESTOR": str(ancestor).lower(),
                "TEST_PATHS": changed_paths, "TEST_SERVE_CHANGED": str(serve_changed).lower(),
                "TEST_VERSION": version, "TEST_TAG": tag or "v0.8.2",
                "TEST_DRAFT": draft, "TEST_PRERELEASE": prerelease,
            }
            result = subprocess.run([bash, "-c", stubs + script], env=env,
                                    capture_output=True, text=True, timeout=10)
            return result, output.read_text() if output.exists() else ""

    def test_manual_release_requires_exact_canonical_tag_and_commit(self):
        result, output = self.run_resolve()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn(f"source_commit={SOURCE}\n", output)
        self.assertIn(f"workflow_commit={SOURCE}\n", output)
        for overrides in (
            {"ref_type": "branch", "ref": "main"},
            {"ref": "v0.8.1"},
            {"ref": "octet-serve-v0.8.2"},
            {"workflow_sha": TOOLING},
            {"tag": "v0.8.2;echo injection"},
            {"tag": "v0.8.2-rc.1"},
        ):
            with self.subTest(overrides=overrides):
                result, output = self.run_resolve(**overrides)
                self.assertNotEqual(0, result.returncode)
                self.assertEqual("", output)

    def test_dedicated_push_retains_immutable_source_for_tooling_repairs(self):
        for sha in (SOURCE, TOOLING):
            with self.subTest(workflow_sha=sha):
                result, output = self.run_resolve(event="push", ref="octet-serve-v0.8.2",
                                                  tag="", workflow_sha=sha)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertIn(f"source_commit={SOURCE}\n", output)
                self.assertIn(f"workflow_commit={sha}\n", output)

    def test_tooling_repairs_cannot_change_executable_source(self):
        for overrides in (
            {"ancestor": False},
            {"changed_paths": "extensions/octet-codemode/main.py"},
            {"changed_paths": ".github/workflows/release-serve.yml\nscripts/package-octet-extension-release.sh"},
            {"changed_paths": ".github/workflows/release-octet.yml", "serve_changed": False},
            {"ref": "v0.8.2"},
        ):
            with self.subTest(overrides=overrides):
                options = {"event": "push", "ref": "octet-serve-v0.8.2", "workflow_sha": TOOLING}
                options.update(overrides)
                result, output = self.run_resolve(**options)
                self.assertNotEqual(0, result.returncode)
                self.assertEqual("", output)

    def test_release_stays_version_matched_published_and_stable(self):
        for overrides in ({"version": "0.8.1"}, {"draft": "true"}, {"prerelease": "true"}):
            with self.subTest(overrides=overrides):
                result, output = self.run_resolve(**overrides)
                self.assertNotEqual(0, result.returncode)
                self.assertEqual("", output)


if __name__ == "__main__":
    unittest.main()
