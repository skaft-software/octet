#!/usr/bin/env python3
"""Prove a main squash push is the identical, dispatch-qualified PR source.

Only the parent gate calls this helper, with a 35-second *whole-process* bound.
GitHub API evidence is read-only, fixed-origin, bounded and never redirected.
A deleted PR branch may require a bounded fetch of its immutable API-proven head
SHA from the verified public GitHub origin. No proof means the parent does the
existing live check. This does not waive any other main/release CI checks.
The repository's squash-only, strict protected-main policy remains required.
Only an exact-head workflow_dispatch run qualifies: dispatch runs this source's
workflow directly and always checks live metadata, independent of base version.
A PR run alone cannot prove that: its merge-tree workflow can differ, its live
check can skip on same-version bases, and run.pull_requests associations mutate.
PR CI remains independently required by branch protection and release policy.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ".github/workflows/ci.yml"
FRESHNESS_STEP = "Check pre-release model metadata freshness"
MAX_RESPONSE = 8 * 1024 * 1024


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("GitHub API redirect refused")


class GitHub:
    def __init__(self, repository: str, token: str):
        self.prefix = f"https://api.github.com/repos/{repository}/"
        self.token = token
        self.requests = 0
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect(),
        )

    def get(self, path: str):
        # Paths are constructed here, never taken from API-provided URLs.
        self.requests += 1
        if self.requests > 16 or not re.fullmatch(r"[A-Za-z0-9_./?=&-]+", path):
            raise ValueError("GitHub API request bound exceeded")
        request = urllib.request.Request(self.prefix + path, headers={
            "Authorization": f"Bearer {self.token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "octet-model-metadata-gate",
        })
        with self.opener.open(request, timeout=5) as response:
            if response.status != 200:
                raise ValueError("unexpected GitHub API status")
            data = response.read(MAX_RESPONSE + 1)
        if len(data) > MAX_RESPONSE:
            raise ValueError("GitHub API response bound exceeded")
        return json.loads(data)


def sha(value) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"[0-9a-f]{40}", value)) and value != "0" * 40


def positive_id(value) -> bool:
    return type(value) is int and value > 0


def git(*args: str, timeout: int = 5) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, check=True, capture_output=True,
        text=True, timeout=timeout,
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0", "GIT_NO_REPLACE_OBJECTS": "1"},
    ).stdout.strip()


def ensure_head(repository: str, head: str) -> bool:
    try:
        git("cat-file", "-e", f"{head}^{{commit}}")
    except subprocess.CalledProcessError:
        # checkout's full history excludes deleted branches after a squash.
        # Fetch only the immutable API-proven SHA, never a branch/PR ref. Match
        # the configured origin to this repository's fixed public HTTPS URL;
        # do not follow redirects or use credentials, prompts or proxies.
        origin = git("remote", "get-url", "origin")
        if origin not in {f"https://github.com/{repository}", f"https://github.com/{repository}.git"}:
            return False
        git(
            "-c", "credential.helper=", "-c", "core.askPass=",
            "-c", "http.extraHeader=", "-c", "http.proxy=",
            "-c", "http.followRedirects=false", "-c", "http.sslVerify=true",
            "-c", "protocol.allow=never", "-c", "protocol.https.allow=always",
            "fetch", "--quiet", "--no-tags", "--no-write-fetch-head",
            "--no-recurse-submodules", "--no-auto-maintenance", "--refmap=",
            origin, head, timeout=10,
        )
    # Verify the local object, then the caller still proves ancestry and tree
    # equality. The fetch changes no branch, index, worktree or remote state.
    return git("rev-parse", "--verify", f"{head}^{{commit}}") == head


def successful(value: dict) -> bool:
    return value.get("status") == "completed" and value.get("conclusion") == "success"


def prove(api: GitHub, repository: str, before: str, after: str) -> bool:
    # Independently bind the checkout and GitHub commit to the trusted push event.
    if git("rev-parse", "HEAD") != after:
        return False
    if git("rev-list", "--parents", "-n", "1", after).split() != [after, before]:
        return False
    commit = api.get(f"commits/{after}")
    if commit["sha"] != after or [p["sha"] for p in commit["parents"]] != [before]:
        return False
    main_tree = git("rev-parse", f"{after}^{{tree}}")
    if commit["commit"]["tree"]["sha"] != main_tree:
        return False

    workflow = api.get("actions/workflows/ci.yml")
    workflow_id = workflow.get("id")
    if not positive_id(workflow_id) or workflow.get("path") != WORKFLOW:
        return False

    # Bounded first pages: absence or omitted older evidence is not permission.
    associated = api.get(f"commits/{after}/pulls?per_page=100")
    if not isinstance(associated, list) or len(associated) > 100:
        return False
    for candidate in associated:
        number = candidate.get("number")
        if not positive_id(number):
            continue
        pr = api.get(f"pulls/{number}")
        if (pr.get("number") != number or pr.get("merged") is not True
                or pr.get("state") != "closed" or not pr.get("merged_at")
                or pr.get("merge_commit_sha") != after
                or pr["base"]["repo"]["full_name"] != repository
                or pr["base"]["ref"] != "main"
                or pr["head"]["repo"]["full_name"] != repository):
            continue
        head = pr["head"]["sha"]
        if not sha(head):
            continue
        if not ensure_head(repository, head):
            continue
        git("merge-base", "--is-ancestor", before, head)
        if git("rev-parse", f"{head}^{{tree}}") != main_tree:
            continue
        runs = api.get(f"actions/workflows/ci.yml/runs?event=workflow_dispatch&head_sha={head}&per_page=100")
        # Require the latest run for this exact head, not an older green result.
        summaries = runs["workflow_runs"]
        if not summaries or len(summaries) > 100 or any(not positive_id(r.get("id")) for r in summaries):
            continue
        for summary in [max(summaries, key=lambda r: r["id"])]:
            run_id = summary["id"]
            run = api.get(f"actions/runs/{run_id}")
            attempt = run.get("run_attempt")
            if (run.get("id") != run_id or not positive_id(attempt)
                    or run.get("event") != "workflow_dispatch" or run.get("path") != WORKFLOW
                    or run.get("workflow_id") != workflow_id
                    or run.get("head_sha") != head or not successful(run)
                    or run.get("head_branch") != pr["head"]["ref"]
                    or run["repository"]["full_name"] != repository
                    or run["repository"]["id"] != pr["base"]["repo"]["id"]
                    or run["head_repository"]["full_name"] != repository
                    or run["head_repository"]["id"] != pr["head"]["repo"]["id"]):
                continue
            # Do not inspect run.pull_requests: GitHub updates those associations.
            # Dispatch always runs the live check using this exact head's workflow
            # and checkout, not a potentially different synthetic PR merge tree.
            jobs = api.get(f"actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100")
            # Reject partial pages, duplicate jobs/steps and evidence from any
            # other attempt. A green run alone can hide a skipped freshness step.
            if jobs["total_count"] != len(jobs["jobs"]):
                continue
            quality = [job for job in jobs["jobs"] if job.get("name") == "quality"]
            if len(quality) != 1 or not successful(quality[0]):
                continue
            job = quality[0]
            if job.get("run_id") != run_id or job.get("run_attempt") != attempt or job.get("head_sha") != head:
                continue
            steps = [step for step in job["steps"] if step.get("name") == FRESHNESS_STEP]
            if len(steps) != 1 or not successful(steps[0]):
                continue
            # A rerun may have started while jobs were being read. Never combine
            # a previous successful attempt's jobs with the current run status.
            final_run = api.get(f"actions/runs/{run_id}")
            identity = ("id", "run_attempt", "event", "path", "workflow_id", "head_sha", "head_branch",
                        "repository", "head_repository")
            if successful(final_run) and all(final_run.get(key) == run.get(key) for key in identity):
                print(f"Frozen models.dev source qualified by PR #{number}, dispatch CI run {run_id}, attempt {attempt}.")
                return True
    return False


def main() -> int:
    # Remove the credential before any Git subprocess; never log API exceptions,
    # response bodies, headers or the token, and never put credentials in argv.
    token = os.environ.pop("MODEL_METADATA_GITHUB_TOKEN", "")
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    before = os.environ.get("MODEL_METADATA_BASE_SHA", "")
    after = os.environ.get("MODEL_METADATA_AFTER_SHA", "")
    if (not token or not re.fullmatch(r"[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+", repository)
            or os.environ.get("GITHUB_EVENT_NAME") != "push"
            or os.environ.get("GITHUB_REF") != "refs/heads/main"
            or os.environ.get("GITHUB_SHA") != after or not sha(before) or not sha(after)):
        return 1
    try:
        return 0 if prove(GitHub(repository, token), repository, before, after) else 1
    except (OSError, ValueError, KeyError, TypeError, AttributeError,
            subprocess.SubprocessError, urllib.error.URLError):
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
