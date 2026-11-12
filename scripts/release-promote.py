#!/usr/bin/env python3
"""Final read-only npm/receipt gate before the workflow marks GitHub Latest."""
import sys
import json
import os

from release import command, get_release, load, require, valid_reason


def check(version):
    release = get_release(version)
    require(release and not release["draft"] and not release["prerelease"], "not a published stable release")
    state = load(release)
    require(state["shipping"] and not state["paused"], "ship receipt is not authorized")
    if state["skip_npm"]:
        require(valid_reason(state["skip_npm"]), "npm skip lacks a reason")
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
            summary.write(f"npm skipped: {state['skip_npm']}\n")
        return
    for suffix in ("", "-darwin-arm64", "-darwin-x64", "-linux-x64-gnu", "-win32-x64"):
        package = "@skaft/octet" + suffix
        latest = json.loads(command("npm", "view", package, "dist-tags", "--json", "--registry", "https://registry.npmjs.org/"))
        require(latest.get("latest") == version, f"npm latest differs: {package}")


if __name__ == "__main__":
    check(sys.argv[1])
