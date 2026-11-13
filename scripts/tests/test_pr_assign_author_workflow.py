"""Offline contracts for the privileged, metadata-only author assignment job."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/pr-assign-author.yml"


class PRAssignAuthorWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        code = cls.workflow.split("          python3 - <<'PY'\n", 1)[1].split("\n          PY", 1)[0]
        cls.assign = compile(textwrap.dedent(code), str(WORKFLOW), "exec")

    def run_assignment(self, login="contributor", user_type="User", owner="repo-owner", failure=None):
        event = {
            "number": 727,
            "repository": {"owner": {"login": owner}},
            "pull_request": {"user": {"login": login, "type": user_type}},
        }
        with tempfile.TemporaryDirectory() as directory:
            event_path = Path(directory) / "event.json"
            event_path.write_text(json.dumps(event))
            env = {"GITHUB_EVENT_PATH": str(event_path), "GITHUB_REPOSITORY": "repo-owner/octet"}
            with mock.patch.dict(os.environ, env), mock.patch("subprocess.run", side_effect=failure) as run:
                exec(self.assign, {})
                return run

    def test_privileged_event_has_minimal_permissions_and_no_pr_execution(self):
        self.assertIn("  pull_request_target:\n    types: [opened, reopened]\n", self.workflow)
        self.assertIn("permissions: { pull-requests: write }\n", self.workflow)
        self.assertEqual(self.workflow.count("permissions:"), 1)
        self.assertNotIn("uses:", self.workflow)
        self.assertNotIn("checkout", self.workflow)
        self.assertNotIn("secrets.", self.workflow)
        self.assertIn("GITHUB_TOKEN: ${{ github.token }}", self.workflow)
        self.assertNotIn("${{", self.workflow.split("        run: |\n", 1)[1])
        self.assertNotIn("pull_request.head", self.workflow)

    def test_adds_author_with_rest_without_replacing_other_assignees(self):
        run = self.run_assignment()
        run.assert_called_once_with(
            ["gh", "api", "--method", "POST", "repos/repo-owner/octet/issues/727/assignees",
             "-f", "assignees[]=contributor"],
            check=True, stdout=subprocess.DEVNULL,
        )

    def test_bots_and_repository_owner_are_not_assigned(self):
        for login, user_type in (("dependabot[bot]", "Bot"), ("custom-automation", "Bot"),
                                 ("repo-owner", "User"), ("REPO-OWNER", "User")):
            with self.subTest(login=login):
                self.run_assignment(login=login, user_type=user_type).assert_not_called()

    def test_metadata_is_data_not_shell_code(self):
        login = "$(touch injected); 'quoted'\n--help"
        run = self.run_assignment(login=login)
        self.assertEqual(run.call_args.args[0][-1], f"assignees[]={login}")
        self.assertNotIn("shell", run.call_args.kwargs)

    def test_rest_failure_is_reported(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_assignment(failure=subprocess.CalledProcessError(1, ["gh", "api"]))


if __name__ == "__main__":
    unittest.main()
