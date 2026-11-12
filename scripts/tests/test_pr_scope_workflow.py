"""Offline tests of the untrusted PR-body scope check."""

import contextlib
import io
import os
from pathlib import Path
import re
import tempfile
import textwrap
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/pr-scope.yml"


class PRScopeWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        code = cls.workflow.split("          python3 - <<'PY'\n", 1)[1].split("\n          PY", 1)[0]
        cls.check = compile(textwrap.dedent(code), str(WORKFLOW), "exec")

    def check_body(self, body):
        with mock.patch.dict(os.environ, {"PR_BODY": body}), contextlib.redirect_stdout(io.StringIO()):
            try:
                exec(self.check, {})
            except SystemExit as error:
                return error.code
        return 0

    def test_unprivileged_pr_events_and_author_exceptions(self):
        self.assertIn("  pull_request:\n    types: [opened, edited, synchronize, reopened]", self.workflow)
        self.assertNotIn("pull_request_target", self.workflow)
        self.assertIn("permissions: {}", self.workflow)
        self.assertIn("  pr-scope:\n", self.workflow)
        self.assertIn("github.event.pull_request.user.login != 'dependabot[bot]'", self.workflow)
        self.assertIn("github.event.pull_request.user.login != 'achuthanmukundan00'", self.workflow)
        self.assertNotIn("actions/checkout", self.workflow)
        self.assertNotIn("secrets.", self.workflow)
        self.assertIn("PR_BODY: ${{ github.event.pull_request.body }}", self.workflow)
        run_blocks = re.findall(r"(?m)^        run: \|\n((?:          [^\n]*\n|\n)+)", self.workflow)
        self.assertEqual(len(run_blocks), 2)
        for block in run_blocks:
            self.assertNotIn("${{", block)

    def test_issue_references_are_accepted(self):
        for body in ("Fixes #1", "closes #42", "RESOLVES #107", "Refs #88",
                     "Details\nfixes\n#17\n", "See https://github.com/owner/repo/issues/123",
                     "[issue](https://github.com/org/repo.name/issues/45#issuecomment-1)"):
            with self.subTest(body=body):
                self.assertEqual(self.check_body(body), 0)

    def test_missing_or_non_issue_references_are_rejected(self):
        for body in ("", "Refs #", "#123", "Fixes #0", "Fixes #123abc", "prefixes #12",
                     "https://github.com/owner/repo/pull/123",
                     "http://github.com/owner/repo/issues/12",
                     "https://github.com.evil/owner/repo/issues/12",
                     "https://github.com/owner/repo/issues/0",
                     "$(touch injection); exit 0"):
            with self.subTest(body=body):
                self.assertNotEqual(self.check_body(body), 0)

    def test_unfilled_template_does_not_pass(self):
        template = (ROOT / ".github/PULL_REQUEST_TEMPLATE.md").read_text()
        self.assertTrue(template.startswith("## In your own words\n"))
        self.assertNotEqual(self.check_body(template), 0)


class ContributionNotesWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()
        cls.job = cls.workflow.split("  contribution-notes:\n", 1)[1]
        code = cls.job.split("          python3 - <<'PY'\n", 1)[1].split("\n          PY", 1)[0]
        cls.check = compile(textwrap.dedent(code), str(WORKFLOW), "exec")

    def summarize(self, body):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "summary.md"
            env = {"PR_BODY": body, "GITHUB_STEP_SUMMARY": str(path)}
            with mock.patch.dict(os.environ, env), contextlib.redirect_stdout(io.StringIO()) as stdout:
                exec(self.check, {})
            self.assertEqual(stdout.getvalue(), "")
            return path.read_text()

    def test_advisory_job_is_unprivileged_and_skips_owner_and_all_bots(self):
        self.assertIn("github.event.pull_request.user.type != 'Bot'", self.job)
        self.assertIn("github.event.pull_request.user.login != 'achuthanmukundan00'", self.job)
        self.assertIn("    continue-on-error: true\n", self.job)
        self.assertIn("    timeout-minutes: 2\n", self.job)
        self.assertEqual(self.workflow.count("permissions:"), 1)
        for forbidden in ("uses:", "secrets.", "github.token", "pull_request_target", "needs:"):
            self.assertNotIn(forbidden, self.workflow)
        self.assertIn("PR_BODY: ${{ github.event.pull_request.body }}", self.job)
        self.assertNotIn("${{", self.job.split("        run: |\n", 1)[1])

    def test_all_sections_with_content_are_present(self):
        summary = self.summarize(
            "## In your own words\nThis matters to me.\n"
            "## Security check\nDocs only; no executable changes.\n"
            "## AI provenance\nnone"
        )
        self.assertEqual(summary.count(": present"), 3)
        self.assertIn("Presence only; human review is still required.", summary)

    def test_empty_and_comment_only_sections_are_missing_without_failure(self):
        for body in ("", (ROOT / ".github/PULL_REQUEST_TEMPLATE.md").read_text(),
                     "## In your own words\n \n## Security check\n<!-- not an answer -->\n"
                     "## AI provenance\n<!-- multi\nline -->\n"):
            with self.subTest(body=body):
                self.assertEqual(self.summarize(body).count(": missing or empty"), 3)

    def test_another_heading_does_not_fill_a_required_section(self):
        summary = self.summarize("## In your own words\n## Other\nNot the human's words.\n")
        self.assertEqual(summary.count(": missing or empty"), 3)

    def test_headings_are_case_insensitive_and_comments_cannot_supply_them(self):
        summary = self.summarize(
            "<!-- ## Security check\nnot visible -->\n"
            "## IN YOUR OWN WORDS\r\nA reason.\r\n"
            "## AI PROVENANCE\nnot exposed\n"
        )
        self.assertIn("- In your own words: present", summary)
        self.assertIn("- AI provenance: present", summary)
        self.assertIn("- Security check: missing or empty", summary)

    def test_untrusted_body_is_never_executed_or_copied_to_summary(self):
        payload = "$(touch injected); 'quoted'\n<script>untrusted</script>"
        with mock.patch("os.system") as system:
            summary = self.summarize("## Security check\n" + payload)
        system.assert_not_called()
        self.assertNotIn(payload, summary)
        self.assertNotIn("untrusted", summary)
        self.assertIn("- Security check: present", summary)


if __name__ == "__main__":
    unittest.main()
