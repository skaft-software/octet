#!/usr/bin/env python3
"""Resolve the public octet PR candidate or v0.10.0 tag to a commit.

Print only the full commit SHA. Requires gh with read access to the public repo.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess

REPOSITORY = "skaft-software/octet"


def github_get(path):
    result = subprocess.run(["gh", "api", f"repos/{REPOSITORY}/{path}"],
                            text=True, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"GitHub API request failed: {path}")
    return json.loads(result.stdout)


def commit_sha(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("host did not resolve to a full lowercase commit SHA")
    return value


def tag_commit(get):
    try:
        obj = get("git/ref/tags/v0.10.0")["object"]
        # Support lightweight and annotated tags without resolving a same-named branch.
        for _ in range(6):
            sha = commit_sha(obj["sha"])
            if obj["type"] == "commit":
                return sha
            if obj["type"] != "tag":
                break
            obj = get(f"git/tags/{sha}")["object"]
    except (KeyError, TypeError) as exc:
        raise ValueError("incomplete host tag metadata") from exc
    raise ValueError("host tag does not resolve to a commit")


def resolve(target, get=github_get):
    if target in ("auto", "candidate"):
        pull = get("pulls/639")
        if target == "auto" and pull.get("state") == "closed":
            return tag_commit(get)
        try:
            valid = (pull["state"] == "open" and pull["head"]["repo"]["full_name"] == REPOSITORY
                     and pull["base"]["repo"]["full_name"] == REPOSITORY
                     and pull["base"]["ref"] == "main")
            commit = pull["head"]["sha"]
        except (KeyError, TypeError) as exc:
            raise ValueError("incomplete host PR metadata") from exc
        if not valid:
            raise ValueError("host PR is not the expected open octet PR against main")
    elif target in ("tag", "release"):
        if target == "release":
            release = get("releases/tags/v0.10.0")
            if (release.get("tag_name") != "v0.10.0" or release.get("draft") is not False
                    or release.get("prerelease") is not False):
                raise ValueError("octet v0.10.0 is not a published stable release")
        commit = tag_commit(get)
    else:
        raise ValueError("host target must be auto, candidate, tag, or release")
    return commit_sha(commit)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=("auto", "candidate", "tag", "release"))
    args = parser.parse_args()
    print(resolve(args.target))


if __name__ == "__main__":
    main()
