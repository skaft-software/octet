#!/usr/bin/env python3
"""Offline regressions for the pre-release models.dev CI decision boundary."""

from __future__ import annotations

import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import urllib.error
from unittest import mock

SCRIPT = Path(__file__).with_name("check-release-model-metadata.py")
SPEC = importlib.util.spec_from_file_location("release_model_metadata_gate", SCRIPT)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)
SHA = "a" * 40
PROOF_SPEC = importlib.util.spec_from_file_location(
    "pr_model_metadata_proof", SCRIPT.with_name("check-pr-model-metadata-proof.py"),
)
proof = importlib.util.module_from_spec(PROOF_SPEC)
PROOF_SPEC.loader.exec_module(proof)


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

    def test_main_push_reuses_only_successful_bounded_proof(self) -> None:
        os.environ.update(GITHUB_EVENT_NAME="push", GITHUB_REF="refs/heads/main",
                          MODEL_METADATA_GITHUB_TOKEN="test-token")
        self.assertEqual(gate.main(), 0)
        self.assertEqual(self.run.call_args_list[0], self.base_call())
        args, kwargs = self.run.call_args_list[1]
        self.assertEqual(args[0], [sys.executable, gate.PROOF])
        self.assertEqual(kwargs["timeout"], 35)
        self.assertTrue(kwargs["check"])
        self.assertEqual(kwargs["env"]["MODEL_METADATA_GITHUB_TOKEN"], "test-token")
        self.assertEqual(len(self.run.call_args_list), 2)
        self.assertNotIn("MODEL_METADATA_GITHUB_TOKEN", os.environ)

    def test_unavailable_failed_or_timed_out_proof_falls_back_to_live(self) -> None:
        for failure in (OSError("unavailable"), subprocess.CalledProcessError(1, ["proof"]),
                        subprocess.TimeoutExpired(["proof"], 35)):
            with self.subTest(failure=failure):
                os.environ.update(GITHUB_EVENT_NAME="push", GITHUB_REF="refs/heads/main",
                                  MODEL_METADATA_GITHUB_TOKEN="test-token")
                self.run.reset_mock()
                self.run.side_effect = [
                    subprocess.CompletedProcess(["git"], 0, stdout=self.base_manifest),
                    failure, subprocess.CompletedProcess(["refresh"], 0),
                ]
                self.assertEqual(gate.main(), 0)
                self.assertEqual(self.run.call_args_list[-1], self.refresh_call())
                self.assertNotIn("MODEL_METADATA_GITHUB_TOKEN", os.environ)
                self.assertIn("requiring live", self.stdout.getvalue())

    def test_fallback_still_blocks_stale_metadata(self) -> None:
        os.environ.update(GITHUB_EVENT_NAME="push", GITHUB_REF="refs/heads/main",
                          MODEL_METADATA_GITHUB_TOKEN="test-token")
        self.run.side_effect = [
            subprocess.CompletedProcess(["git"], 0, stdout=self.base_manifest),
            subprocess.CalledProcessError(1, ["proof"]),
            subprocess.CalledProcessError(1, ["refresh"]),
        ]
        self.assertEqual(gate.main(), 1)
        self.assertIn("metadata is stale", self.stderr.getvalue())

    def test_pr_dispatch_and_non_main_push_never_reuse_proof_even_with_token(self) -> None:
        for event in ("pull_request", "workflow_dispatch", "push"):
            with self.subTest(event=event):
                os.environ.update(GITHUB_EVENT_NAME=event, GITHUB_REF="refs/heads/topic",
                                  MODEL_METADATA_GITHUB_TOKEN="test-token")
                self.run.reset_mock()
                self.assertEqual(gate.main(), 0)
                expected = [self.refresh_call()] if event == "workflow_dispatch" else [self.base_call(), self.refresh_call()]
                self.assertEqual(self.run.call_args_list, expected)
                self.assertNotIn("MODEL_METADATA_GITHUB_TOKEN", os.environ)

    def test_same_version_never_requests_proof(self) -> None:
        self.base_manifest = manifest("0.7.6")
        os.environ.update(GITHUB_EVENT_NAME="push", GITHUB_REF="refs/heads/main",
                          MODEL_METADATA_GITHUB_TOKEN="test-token")
        self.assertEqual(gate.main(), 0)
        self.assertEqual(self.run.call_args_list, [self.base_call()])

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


class FrozenPRProofTests(unittest.TestCase):
    def setUp(self) -> None:
        self.before, self.after, self.head, self.tree = (letter * 40 for letter in "abcd")
        self.repository = "skaft-software/octet"
        self.commit = {"sha": self.after, "parents": [{"sha": self.before}],
                       "commit": {"tree": {"sha": self.tree}}}
        self.pr = {
            "number": 639, "merged": True, "state": "closed", "merged_at": "2026-11-06T00:00:00Z",
            "merge_commit_sha": self.after,
            "base": {"sha": self.before, "ref": "main", "repo": {"id": 1, "full_name": self.repository}},
            "head": {"sha": self.head, "ref": "release/v0.10.0", "repo": {"id": 1, "full_name": self.repository}},
        }
        self.run = {
            "id": 42, "run_attempt": 2, "head_sha": self.head,
            "event": "workflow_dispatch", "path": proof.WORKFLOW, "workflow_id": 7,
            "status": "completed", "conclusion": "success",
            "repository": {"id": 1, "full_name": self.repository},
            "head_repository": {"id": 1, "full_name": self.repository},
            "head_branch": "release/v0.10.0",
            "pull_requests": [{"number": 639, "base": copy.deepcopy(self.pr["base"]),
                               "head": copy.deepcopy(self.pr["head"])}],
        }
        self.step = {"name": proof.FRESHNESS_STEP, "status": "completed", "conclusion": "success"}
        self.job = {"name": "quality", "status": "completed", "conclusion": "success",
                    "run_id": 42, "run_attempt": 2, "head_sha": self.head, "steps": [self.step]}
        self.jobs = {"total_count": 1, "jobs": [self.job]}
        self.responses = {
            "actions/workflows/ci.yml": {"id": 7, "path": proof.WORKFLOW},
            f"commits/{self.after}": self.commit,
            f"commits/{self.after}/pulls?per_page=100": [{"number": 639}],
            "pulls/639": self.pr,
            f"actions/workflows/ci.yml/runs?event=workflow_dispatch&head_sha={self.head}&per_page=100": {
                "workflow_runs": [self.run],
            },
            "actions/runs/42": self.run,
            "actions/runs/42/attempts/2/jobs?per_page=100": self.jobs,
        }
        self.api = mock.Mock()
        self.api.get.side_effect = self.responses.__getitem__
        self.git_results = {
            ("rev-parse", "HEAD"): self.after,
            ("rev-list", "--parents", "-n", "1", self.after): f"{self.after} {self.before}",
            ("rev-parse", f"{self.after}^{{tree}}"): self.tree,
            ("cat-file", "-e", f"{self.head}^{{commit}}"): "",
            ("rev-parse", "--verify", f"{self.head}^{{commit}}"): self.head,
            ("merge-base", "--is-ancestor", self.before, self.head): "",
            ("rev-parse", f"{self.head}^{{tree}}"): self.tree,
        }
        stack = contextlib.ExitStack()
        self.addCleanup(stack.close)
        self.git = stack.enter_context(mock.patch.object(proof, "git", side_effect=lambda *args, **kwargs: self.git_results[args]))
        self.client = stack.enter_context(mock.patch.object(proof, "GitHub", return_value=self.api))
        stack.enter_context(mock.patch.dict(os.environ, {
            "GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main",
            "GITHUB_REPOSITORY": self.repository, "GITHUB_SHA": self.after,
            "MODEL_METADATA_BASE_SHA": self.before, "MODEL_METADATA_AFTER_SHA": self.after,
        }, clear=True))
        self.stdout, self.stderr = io.StringIO(), io.StringIO()
        stack.enter_context(contextlib.redirect_stdout(self.stdout))
        stack.enter_context(contextlib.redirect_stderr(self.stderr))

    def check(self) -> int:
        os.environ["MODEL_METADATA_GITHUB_TOKEN"] = "secret-test-token"
        return proof.main()

    def test_exact_merged_pr_successful_run_job_step_and_tree_qualify(self) -> None:
        self.assertEqual(self.check(), 0)
        self.assertIn("PR #639, dispatch CI run 42, attempt 2", self.stdout.getvalue())
        self.assertIn(mock.call("actions/runs/42/attempts/2/jobs?per_page=100"), self.api.get.call_args_list)
        self.git.assert_any_call("merge-base", "--is-ancestor", self.before, self.head)
        self.assertNotIn("MODEL_METADATA_GITHUB_TOKEN", os.environ)

    def test_direct_push_without_associated_pr_cannot_qualify(self) -> None:
        self.responses[f"commits/{self.after}/pulls?per_page=100"] = []
        self.assertEqual(self.check(), 1)

    def test_wrong_checkout_parent_or_tree_cannot_qualify(self) -> None:
        for args, value in list(self.git_results.items()):
            if args[0] in {"merge-base", "cat-file"}:
                continue
            with self.subTest(args=args), mock.patch.dict(self.git_results, {args: "e" * 40}):
                self.assertEqual(self.check(), 1)
        # Also disallow merge commits, even if the first parent matches.
        args = ("rev-list", "--parents", "-n", "1", self.after)
        self.git_results[args] += " " + "e" * 40
        self.assertEqual(self.check(), 1)

    def missing_head_git(self, *args, **kwargs):
        if args[0] == "cat-file":
            raise subprocess.CalledProcessError(1, ["git", *args])
        if args == ("remote", "get-url", "origin"):
            return f"https://github.com/{self.repository}.git"
        if "fetch" in args:
            return ""
        return self.git_results[args]

    def test_deleted_branch_head_is_fetched_by_immutable_sha_and_still_proven(self) -> None:
        self.git.side_effect = self.missing_head_git
        self.assertEqual(self.check(), 0)
        fetches = [call for call in self.git.call_args_list if "fetch" in call.args]
        self.assertEqual(len(fetches), 1)
        fetch = fetches[0]
        self.assertEqual(fetch.args[-2:], (f"https://github.com/{self.repository}.git", self.head))
        self.assertEqual(fetch.kwargs, {"timeout": 10})
        for option in ("credential.helper=", "core.askPass=", "http.extraHeader=", "http.proxy=",
                       "http.followRedirects=false", "http.sslVerify=true", "protocol.allow=never",
                       "protocol.https.allow=always", "--no-tags", "--no-write-fetch-head",
                       "--no-recurse-submodules", "--no-auto-maintenance", "--refmap="):
            self.assertIn(option, fetch.args)
        self.git.assert_any_call("rev-parse", "--verify", f"{self.head}^{{commit}}")
        self.git.assert_any_call("merge-base", "--is-ancestor", self.before, self.head)
        self.git.assert_any_call("rev-parse", f"{self.head}^{{tree}}")
        self.assertNotIn("secret-test-token", repr(fetch))

    def test_failed_timed_out_or_wrong_origin_fetch_cannot_qualify(self) -> None:
        for failure in (subprocess.CalledProcessError(128, ["git", "fetch"]),
                        subprocess.TimeoutExpired(["git", "fetch"], 10)):
            def get(*args, **kwargs):
                if "fetch" in args:
                    raise failure
                return self.missing_head_git(*args, **kwargs)
            with self.subTest(failure=failure):
                self.git.side_effect = get
                self.assertEqual(self.check(), 1)
        for origin in ("https://evil.example/octet.git", "git@github.com:skaft-software/octet.git",
                       "https://github.com/other/repo.git", "https://token@github.com/skaft-software/octet.git"):
            def get(*args, **kwargs):
                if args == ("remote", "get-url", "origin"):
                    return origin
                return self.missing_head_git(*args, **kwargs)
            with self.subTest(origin=origin):
                self.git.reset_mock()
                self.git.side_effect = get
                self.assertEqual(self.check(), 1)
                self.assertFalse(any("fetch" in call.args for call in self.git.call_args_list))

    def test_fetched_wrong_object_or_tree_cannot_qualify(self) -> None:
        self.git.side_effect = self.missing_head_git
        for args in (("rev-parse", "--verify", f"{self.head}^{{commit}}"),
                     ("rev-parse", f"{self.head}^{{tree}}")):
            with self.subTest(args=args), mock.patch.dict(self.git_results, {args: "e" * 40}):
                self.assertEqual(self.check(), 1)

    def test_existing_head_does_not_fetch(self) -> None:
        self.assertEqual(self.check(), 0)
        self.assertFalse(any("fetch" in call.args for call in self.git.call_args_list))

    def test_missing_git_objects_or_nonancestor_cannot_qualify(self) -> None:
        for failure in (subprocess.CalledProcessError(1, ["git", "merge-base"]),
                        subprocess.TimeoutExpired(["git"], 5), OSError("missing git")):
            with self.subTest(failure=failure):
                self.git.side_effect = failure
                self.assertEqual(self.check(), 1)

    def test_wrong_api_main_sha_parent_or_tree_cannot_qualify(self) -> None:
        for changes in ({"sha": self.head}, {"parents": []}, {"parents": [{"sha": self.head}]},
                        {"parents": [{"sha": self.before}, {"sha": self.head}]},
                        {"commit": {"tree": {"sha": self.head}}}):
            with self.subTest(changes=changes), mock.patch.dict(self.commit, changes):
                self.assertEqual(self.check(), 1)

    def test_unmerged_wrong_merge_or_wrong_base_pr_cannot_qualify(self) -> None:
        for changes in ({"number": 640}, {"merged": False}, {"merged": "true"},
                        {"state": "open"}, {"merged_at": None}, {"merge_commit_sha": self.head},
                        {"head": {"sha": "main"}}):
            with self.subTest(changes=changes), mock.patch.dict(self.pr, changes):
                self.assertEqual(self.check(), 1)
        for changes in ({"ref": "other"}, {"repo": {"full_name": "other/repo"}}):
            with self.subTest(changes=changes), mock.patch.dict(self.pr["base"], changes):
                self.assertEqual(self.check(), 1)

    def test_wrong_run_identity_event_workflow_or_outcome_cannot_qualify(self) -> None:
        for changes in ({"id": "42"}, {"run_attempt": 0}, {"run_attempt": True},
                        {"head_sha": self.after}, {"event": "push"}, {"event": "pull_request_target"},
                        {"event": "pull_request"}, {"path": ".github/workflows/other.yml"},
                        {"workflow_id": 8}, {"workflow_id": None},
                        {"status": "in_progress"}, {"conclusion": "failure"},
                        {"conclusion": "skipped"}, {"conclusion": "cancelled"},
                        {"repository": {"full_name": "other/repo"}},
                        {"head_repository": {"full_name": "other/repo"}},
                        {"head_repository": {"full_name": self.repository, "id": 999}},
                        {"head_branch": "other"}, {"head_branch": "release/other"}):
            with self.subTest(changes=changes), mock.patch.dict(self.run, changes):
                self.assertEqual(self.check(), 1)

    def test_expected_workflow_endpoint_must_identify_ci_workflow(self) -> None:
        for changes in ({"id": 0}, {"id": "7"}, {"path": ".github/workflows/other.yml"}):
            with self.subTest(changes=changes), mock.patch.dict(self.responses["actions/workflows/ci.yml"], changes):
                self.assertEqual(self.check(), 1)

    def test_mutable_run_associations_are_never_sha_evidence(self) -> None:
        self.run["pull_requests"] = [{"number": 999, "head": {"sha": "e" * 40}, "base": {"sha": "f" * 40}}]
        self.pr["base"]["sha"] = self.after  # Current PR base is not historical evidence either.
        self.assertEqual(self.check(), 0)
        self.run["head_sha"] = "e" * 40
        self.assertEqual(self.check(), 1)

    def test_only_matching_same_repository_pr_refs_can_qualify(self) -> None:
        for changes in ({"ref": "feature/topic"}, {"repo": {"full_name": "fork/octet"}}):
            with self.subTest(changes=changes), mock.patch.dict(self.pr["head"], changes):
                self.assertEqual(self.check(), 1)

    def test_latest_run_must_pass_not_an_older_green_run(self) -> None:
        latest = {**self.run, "id": 43, "conclusion": "failure"}
        self.responses["actions/runs/43"] = latest
        path = f"actions/workflows/ci.yml/runs?event=workflow_dispatch&head_sha={self.head}&per_page=100"
        self.responses[path]["workflow_runs"].append(latest)
        self.assertEqual(self.check(), 1)
        self.assertNotIn(mock.call("actions/runs/42"), self.api.get.call_args_list)

    def test_quality_and_exact_freshness_step_must_complete_successfully_once(self) -> None:
        for target in (self.job, self.step):
            for changes in ({"name": "other"}, {"status": "in_progress"}, {"conclusion": None},
                            {"conclusion": "skipped"}, {"conclusion": "failure"},
                            {"conclusion": "cancelled"}, {"conclusion": "neutral"}):
                with self.subTest(target=target, changes=changes), mock.patch.dict(target, changes):
                    self.assertEqual(self.check(), 1)
        for steps in ([], [self.step, self.step]):
            with self.subTest(steps=steps), mock.patch.dict(self.job, {"steps": steps}):
                self.assertEqual(self.check(), 1)
        for jobs in ([], [self.job, self.job]):
            with self.subTest(jobs=jobs), mock.patch.dict(self.jobs, {"jobs": jobs, "total_count": len(jobs)}):
                self.assertEqual(self.check(), 1)

    def test_jobs_cannot_cross_runs_attempts_heads_or_omit_a_page(self) -> None:
        for changes in ({"run_id": 43}, {"run_attempt": 1}, {"head_sha": self.after}):
            with self.subTest(changes=changes), mock.patch.dict(self.job, changes):
                self.assertEqual(self.check(), 1)
        self.jobs["total_count"] = 101
        self.assertEqual(self.check(), 1)

    def test_run_rechecked_after_jobs_cannot_change_attempt_status_or_identity(self) -> None:
        for changes in ({"run_attempt": 3}, {"status": "in_progress"},
                        {"conclusion": "failure"}, {"head_sha": self.after},
                        {"event": "pull_request"}, {"path": ".github/workflows/other.yml"},
                        {"workflow_id": 8}):
            reads = 0
            def get(path):
                nonlocal reads
                if path == "actions/runs/42":
                    reads += 1
                    if reads == 2:
                        return {**self.run, **changes}
                return self.responses[path]
            with self.subTest(changes=changes):
                self.api.get.side_effect = get
                self.assertEqual(self.check(), 1)
                self.assertEqual(reads, 2)

    def test_api_failures_or_malformed_data_fall_back_without_secret_logs(self) -> None:
        for path in self.responses:
            for failure in (urllib.error.URLError("secret-test-token"),
                            ValueError("secret-test-token"), TimeoutError("secret-test-token")):
                def get(request, path=path, failure=failure):
                    if request == path:
                        raise failure
                    return self.responses[request]
                with self.subTest(path=path, failure=failure):
                    self.api.get.side_effect = get
                    self.assertEqual(self.check(), 1)
        self.api.get.side_effect = self.responses.__getitem__
        for path in self.responses:
            for malformed in (None, {}, "wrong type"):
                with self.subTest(path=path, malformed=malformed), mock.patch.dict(self.responses, {path: malformed}):
                    self.assertEqual(self.check(), 1)
        self.assertNotIn("secret-test-token", self.stdout.getvalue() + self.stderr.getvalue())

    def test_untrusted_event_coordinates_do_not_contact_api(self) -> None:
        for key, value in (("GITHUB_EVENT_NAME", "pull_request"), ("GITHUB_REF", "refs/heads/topic"),
                           ("GITHUB_REPOSITORY", "owner/repo/../../other"),
                           ("GITHUB_SHA", self.head), ("MODEL_METADATA_AFTER_SHA", "HEAD"),
                           ("MODEL_METADATA_BASE_SHA", "0" * 40)):
            with self.subTest(key=key), mock.patch.dict(os.environ, {key: value}):
                self.assertEqual(self.check(), 1)
                self.api.get.assert_not_called()
                self.git.assert_not_called()
        self.assertEqual(proof.main(), 1)  # No token.
        self.api.get.assert_not_called()


class GitHubTransportTests(unittest.TestCase):
    def test_fixed_https_origin_headers_no_proxies_and_bounded_response(self) -> None:
        with mock.patch.object(proof.urllib.request, "build_opener") as build:
            response = build.return_value.open.return_value.__enter__.return_value
            response.status = 200
            response.read.return_value = b'{"ok": true}'
            api = proof.GitHub("skaft-software/octet", "test-token")
            self.assertEqual(api.get("commits/" + SHA), {"ok": True})
            self.assertEqual(build.call_args.args[0].proxies, {})
            self.assertIsInstance(build.call_args.args[1], proof.NoRedirect)
            request = build.return_value.open.call_args.args[0]
            self.assertEqual(request.full_url, f"https://api.github.com/repos/skaft-software/octet/commits/{SHA}")
            self.assertEqual(request.get_header("Authorization"), "Bearer test-token")
            self.assertNotIn("test-token", request.full_url)
            self.assertEqual(build.return_value.open.call_args.kwargs, {"timeout": 5})
            response.read.assert_called_once_with(proof.MAX_RESPONSE + 1)
            response.read.return_value = b"x" * (proof.MAX_RESPONSE + 1)
            with self.assertRaises(ValueError):
                api.get("commits/" + SHA)
            response.read.return_value = b"invalid-json"
            with self.assertRaises(ValueError):
                api.get("commits/" + SHA)
            response.status = 302
            with self.assertRaises(ValueError):
                api.get("commits/" + SHA)
            api.requests = 16
            build.return_value.open.reset_mock()
            with self.assertRaises(ValueError):
                api.get("commits/" + SHA)
            build.return_value.open.assert_not_called()

    def test_redirects_never_forward_the_token(self) -> None:
        with self.assertRaises(ValueError):
            proof.NoRedirect().redirect_request(None, None, 302, "Found", {}, "https://evil.example/")


class WorkflowWiringTests(unittest.TestCase):
    def test_quality_keeps_offline_tests_but_not_live_freshness(self) -> None:
        workflows = SCRIPT.parent.parent / ".github/workflows"
        ci = (workflows / "ci.yml").read_text()
        quality = ci.split("  quality:\n", 1)[1].split("\n  first-party-extension-tests:", 1)[0]
        preflight = quality.split("      - uses: dtolnay/rust-toolchain@", 1)[0]
        self.assertIn("python3 scripts/test_refresh_models_dev_metadata.py", preflight)
        self.assertIn("python3 scripts/test_release_model_metadata_gate.py", preflight)
        self.assertNotIn("scripts/check-release-model-metadata.py", ci)
        self.assertNotIn("scripts/refresh-models-dev-pricing.py --check", ci)
        self.assertNotIn("MODEL_METADATA_GITHUB_TOKEN", ci)

    def test_live_check_is_separate_scheduled_and_read_only(self) -> None:
        workflow = (SCRIPT.parent.parent / ".github/workflows/models-dev-freshness.yml").read_text()
        self.assertIn("  schedule:\n", workflow)
        self.assertIn('    - cron: "23 4 * * *"', workflow)
        self.assertIn("  workflow_dispatch:\n", workflow)
        self.assertIn("permissions:\n  contents: read\n", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("timeout-minutes: 2", workflow)
        self.assertIn("run: python3 scripts/refresh-models-dev-pricing.py --check", workflow)


if __name__ == "__main__":
    unittest.main()
