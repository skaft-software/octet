#!/usr/bin/env python3
"""Offline regressions for the pre-release models.dev CI decision boundary."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("check-release-model-metadata.py")
SPEC = importlib.util.spec_from_file_location("release_model_metadata_gate", SCRIPT)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)
SHA = "a" * 40


def manifest(version: str) -> str:
    # The workspace version, not the first unrelated package version, matters.
    return f'[package]\nversion = "9.9.9"\n[workspace.package]\nversion = "{version}"\n'


class ReleaseModelMetadataGateTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.current = self.root / "Cargo.toml"
        self.current.write_text(manifest("0.7.6"))
        self.base_manifest = manifest("0.7.5")
        self.stdout = io.StringIO()
        self.stderr = io.StringIO()
        stack = contextlib.ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(mock.patch.object(gate, "ROOT", self.root))
        stack.enter_context(mock.patch.dict(os.environ, {
            "GITHUB_EVENT_NAME": "pull_request", "MODEL_METADATA_BASE_SHA": SHA,
        }, clear=True))
        stack.enter_context(contextlib.redirect_stdout(self.stdout))
        stack.enter_context(contextlib.redirect_stderr(self.stderr))
        # Never invoke Git, the network, a shell or provider credentials in tests.
        self.run = stack.enter_context(mock.patch.object(gate.subprocess, "run"))
        self.run.side_effect = lambda command, **kwargs: subprocess.CompletedProcess(
            command, 0, stdout=self.base_manifest if command[0] == "git" else ""
        )

    def base_call(self, sha: str = SHA) -> mock._Call:
        return mock.call(["git", "show", f"{sha}:Cargo.toml"], cwd=self.root,
                         check=True, capture_output=True, text=True, timeout=10)

    def refresh_call(self) -> mock._Call:
        return mock.call([sys.executable, gate.REFRESH, "--check"],
                         cwd=self.root, check=True, timeout=60)

    def test_same_version_push_and_pull_request_skip_network(self) -> None:
        self.base_manifest = manifest("0.7.6")
        for event in ("push", "pull_request"):
            with self.subTest(event=event):
                os.environ["GITHUB_EVENT_NAME"] = event
                self.run.reset_mock()
                self.assertEqual(gate.main(), 0)
                self.assertEqual(self.run.call_args_list, [self.base_call()])
                self.assertIn("skipping live models.dev check", self.stdout.getvalue())

    def test_every_version_difference_requires_live_read_only_check(self) -> None:
        before = self.current.read_bytes()
        for event in ("push", "pull_request"):
            for previous in ("0.7.5", "0.7.7", "0.7.6-rc.1"):
                with self.subTest(event=event, previous=previous):
                    os.environ["GITHUB_EVENT_NAME"] = event
                    self.base_manifest = manifest(previous)
                    self.run.reset_mock()
                    self.assertEqual(gate.main(), 0)
                    self.assertEqual(self.run.call_args_list,
                                     [self.base_call(), self.refresh_call()])
        self.assertEqual(self.current.read_bytes(), before)
        self.assertEqual(list(self.root.iterdir()), [self.current])

    def test_dispatch_always_checks_without_requiring_a_base_sha(self) -> None:
        os.environ["GITHUB_EVENT_NAME"] = "workflow_dispatch"
        for sha in (None, "", SHA):
            with self.subTest(sha=sha):
                if sha is None:
                    os.environ.pop("MODEL_METADATA_BASE_SHA", None)
                else:
                    os.environ["MODEL_METADATA_BASE_SHA"] = sha
                self.run.reset_mock()
                self.assertEqual(gate.main(), 0)
                self.assertEqual(self.run.call_args_list, [self.refresh_call()])

    def test_missing_or_invalid_base_sha_fails_closed_without_commands(self) -> None:
        for sha in (None, "", "0" * 40, "a" * 39, "a" * 41, "g" * 40,
                    "HEAD", "main", "--help", SHA + "\n", "$(touch injected)"):
            with self.subTest(sha=sha):
                if sha is None:
                    os.environ.pop("MODEL_METADATA_BASE_SHA", None)
                else:
                    os.environ["MODEL_METADATA_BASE_SHA"] = sha
                self.assertEqual(gate.main(), 1)
                self.run.assert_not_called()
                self.assertIn("nonzero 40-hex", self.stderr.getvalue())

    def test_uppercase_hex_sha_is_passed_as_one_git_argument(self) -> None:
        os.environ["MODEL_METADATA_BASE_SHA"] = SHA.upper()
        self.assertEqual(gate.main(), 0)
        self.assertEqual(self.run.call_args_list,
                         [self.base_call(SHA.upper()), self.refresh_call()])

    def test_missing_base_commit_or_manifest_fails_without_refresh(self) -> None:
        self.run.side_effect = subprocess.CalledProcessError(128, ["git", "show"])
        self.assertEqual(gate.main(), 1)
        self.assertEqual(self.run.call_args_list, [self.base_call()])
        self.assertIn("fetch-depth: 0", self.stderr.getvalue())

    def test_git_timeout_fails_without_refresh(self) -> None:
        self.run.side_effect = subprocess.TimeoutExpired(["git", "show"], 10)
        self.assertEqual(gate.main(), 1)
        self.assertEqual(self.run.call_args_list, [self.base_call()])
        self.assertIn("10-second deadline", self.stderr.getvalue())

    def test_refresh_timeout_fails_closed_for_version_changes_and_dispatch(self) -> None:
        for event in ("push", "pull_request", "workflow_dispatch"):
            with self.subTest(event=event):
                os.environ["GITHUB_EVENT_NAME"] = event
                self.run.reset_mock()
                failure = subprocess.TimeoutExpired([sys.executable, gate.REFRESH], 60)
                if event == "workflow_dispatch":
                    self.run.side_effect = failure
                    expected = [self.refresh_call()]
                else:
                    self.run.side_effect = [
                        subprocess.CompletedProcess(["git", "show"], 0, stdout=self.base_manifest),
                        failure,
                    ]
                    expected = [self.base_call(), self.refresh_call()]
                self.assertEqual(gate.main(), 1)
                self.assertEqual(self.run.call_args_list, expected)
                self.assertIn("60-second deadline", self.stderr.getvalue())
                self.assertIn(f"python3 {gate.REFRESH}", self.stderr.getvalue())
                self.assertIn("CI does not update snapshots", self.stderr.getvalue())

    def test_invalid_current_or_base_workspace_version_fails_closed(self) -> None:
        for source in ("current", "base"):
            for invalid in ("not toml", '[package]\nversion="0.7.6"',
                            '[workspace]\npackage="wrong type"',
                            '[workspace.package]\nversion=76',
                            '[workspace.package]\nversion=""'):
                with self.subTest(source=source, invalid=invalid):
                    self.current.write_text(invalid if source == "current" else manifest("0.7.6"))
                    self.base_manifest = invalid if source == "base" else manifest("0.7.5")
                    self.run.reset_mock()
                    self.assertEqual(gate.main(), 1)
                    expected = [] if source == "current" else [self.base_call()]
                    self.assertEqual(self.run.call_args_list, expected)

    def test_missing_current_manifest_fails_without_commands(self) -> None:
        self.current.unlink()
        self.assertEqual(gate.main(), 1)
        self.run.assert_not_called()

    def test_unknown_or_missing_event_fails_without_commands(self) -> None:
        for event in (None, "", "pull_request_target", "$(touch injected)"):
            with self.subTest(event=event):
                if event is None:
                    os.environ.pop("GITHUB_EVENT_NAME", None)
                else:
                    os.environ["GITHUB_EVENT_NAME"] = event
                self.assertEqual(gate.main(), 1)
                self.run.assert_not_called()

    def test_stale_unavailable_or_unlaunchable_refresher_blocks_release(self) -> None:
        os.environ["GITHUB_EVENT_NAME"] = "workflow_dispatch"
        for failure in (subprocess.CalledProcessError(1, ["refresh"]),
                        subprocess.CalledProcessError(2, ["refresh"]),
                        OSError("cannot execute refresher")):
            with self.subTest(failure=failure):
                self.run.reset_mock()
                self.run.side_effect = failure
                self.assertEqual(gate.main(), 1)
                self.assertEqual(self.run.call_args_list, [self.refresh_call()])
                self.assertIn(f"python3 {gate.REFRESH}", self.stderr.getvalue())
                self.assertIn("review the names, pricing, capabilities and source receipt",
                              self.stderr.getvalue())
                self.assertIn("CI does not update snapshots", self.stderr.getvalue())


class WorkflowWiringTests(unittest.TestCase):
    def test_quality_runs_offline_tests_and_gate_before_compilation(self) -> None:
        workflow = (SCRIPT.parent.parent / ".github/workflows/ci.yml").read_text()
        quality = workflow.split("  quality:\n", 1)[1].split("\n  first-party-extension-tests:", 1)[0]
        preflight = quality.split("      - uses: dtolnay/rust-toolchain@", 1)[0]
        self.assertIn("fetch-depth: 0", preflight)
        self.assertIn("persist-credentials: false", preflight)
        self.assertIn("python3 scripts/test_refresh_models_dev_metadata.py", preflight)
        self.assertIn("python3 scripts/test_release_model_metadata_gate.py", preflight)
        self.assertIn('MODEL_METADATA_BASE_SHA: "${{ github.event.pull_request.base.sha || github.event.before }}"',
                      preflight)
        self.assertIn("run: python3 scripts/check-release-model-metadata.py", preflight)
        self.assertIn("- name: Check pre-release model metadata freshness\n"
                      "        timeout-minutes: 2\n", preflight)
        self.assertNotIn("if:", preflight)
        self.assertIn("permissions:\n  contents: read\n", workflow)


if __name__ == "__main__":
    unittest.main()
