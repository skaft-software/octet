"""Offline regressions for the CI tier and required-status contract."""

import contextlib
import io
import json
import os
from pathlib import Path
import re
import shlex
import textwrap
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
FULL_JOBS = {
    "first-party-extension-tests", "extension-api-v03", "windows", "msrv", "smoke-install",
}
FAST_JOBS = {"dependency-review", "dependencies", "quality", "test"}
ALL_JOBS = FAST_JOBS | FULL_JOBS | {"release-candidate"}
FULL_CONDITION = (
    "github.event_name != 'pull_request' || "
    "contains(github.event.pull_request.labels.*.name, 'ci:full')"
)
MATRIX = (
    "${{ fromJSON((github.event_name == 'pull_request' && "
    "!contains(github.event.pull_request.labels.*.name, 'ci:full')) && "
    "'[\"ubuntu-24.04\"]' || '[\"ubuntu-24.04\", \"macos-15\"]') }}"
)


def jobs(workflow):
    return dict(re.findall(r"(?ms)^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)",
                           workflow.split("\njobs:\n", 1)[1]))


class CIWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        cls.jobs = jobs(cls.workflow)
        aggregator = cls.jobs["ci-required"]
        code = aggregator.split("          python3 - <<'PY'\n", 1)[1].split("\n          PY", 1)[0]
        cls.gate = compile(textwrap.dedent(code), str(WORKFLOW), "exec")

    def gate_result(self, event, full_pr, statuses):
        context = {job: {"result": status, "outputs": {}} for job, status in statuses.items()}
        env = {"NEEDS_CONTEXT": json.dumps(context), "CI_EVENT_NAME": event,
               "CI_FULL_PR": str(full_pr).lower()}
        with mock.patch.dict(os.environ, env), contextlib.redirect_stdout(io.StringIO()):
            try:
                exec(self.gate, {})
            except SystemExit as error:
                return error.code
        return 0

    def test_triggers_and_concurrency_isolate_non_pr_runs(self):
        header = self.workflow.split("\njobs:\n", 1)[0]
        self.assertIn("  push:\n    branches:\n      - main\n", header)
        self.assertIn("  merge_group:\n", header)
        self.assertIn("  workflow_dispatch:\n", header)
        self.assertIn("types: [opened, synchronize, reopened, labeled, unlabeled]", header)
        self.assertIn("format('pr-{0}', github.event.pull_request.number)", header)
        self.assertIn("format('run-{0}', github.run_id)", header)
        self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}", header)

    def test_tier_and_event_conditions(self):
        self.assertIn("if: github.event_name == 'pull_request'", self.jobs["dependency-review"])
        for job in FULL_JOBS:
            with self.subTest(job=job):
                self.assertIn(f"    if: {FULL_CONDITION}\n", self.jobs[job])
        self.assertIn(f"        os: {MATRIX}\n", self.jobs["test"])
        self.assertNotRegex(self.jobs["test"], r"(?m)^    name:")
        self.assertIn("if: github.event_name == 'workflow_dispatch'", self.jobs["release-candidate"])
        self.assertIn("    if: always()\n", self.jobs["ci-required"])

    def test_aggregator_needs_all_jobs_and_release_lane_names_survive(self):
        aggregator = self.jobs["ci-required"]
        needed_block = aggregator.split("    needs:\n", 1)[1].split("    runs-on:", 1)[0]
        needed = re.findall(r"(?m)^      - ([\w-]+)$", needed_block)
        self.assertEqual(set(needed), ALL_JOBS)
        self.assertEqual(len(needed), len(ALL_JOBS))
        self.assertEqual(set(self.jobs) - {"ci-required"}, ALL_JOBS)
        self.assertIn("name: windows (x86_64-pc-windows-gnu)", self.jobs["windows"])
        release = (ROOT / ".github/workflows/release-octet.yml").read_text()
        lane_loop = release.split("for check in ", 1)[1].split("; do", 1)[0]
        required = set(shlex.split(lane_loop.replace("\\\n", " ")))
        self.assertEqual(required, {
            "dependencies", "quality", "test (ubuntu-24.04)", "test (macos-15)",
            "windows (x86_64-pc-windows-gnu)", "first-party-extension-tests",
            "extension-api-v03", "msrv", "smoke-install",
        })

    def test_required_status_for_each_event_and_each_result(self):
        for event, full_pr in (("pull_request", False), ("pull_request", True),
                               ("merge_group", False), ("push", False),
                               ("workflow_dispatch", False)):
            statuses = {job: "success" for job in ALL_JOBS}
            if event != "pull_request":
                statuses["dependency-review"] = "skipped"
            if event != "workflow_dispatch":
                statuses["release-candidate"] = "skipped"
            if event == "pull_request" and not full_pr:
                statuses.update({job: "skipped" for job in FULL_JOBS})
            with self.subTest(event=event, full_pr=full_pr, case="expected"):
                self.assertEqual(self.gate_result(event, full_pr, statuses), 0)
            for job, expected in statuses.items():
                for result in ("failure", "cancelled", "skipped"):
                    altered = {**statuses, job: result}
                    with self.subTest(event=event, full_pr=full_pr, job=job, result=result):
                        outcome = self.gate_result(event, full_pr, altered)
                        if result == expected == "skipped":
                            self.assertEqual(outcome, 0)
                        else:
                            self.assertNotEqual(outcome, 0)
                            self.assertIn(job, str(outcome))


if __name__ == "__main__":
    unittest.main()
