#!/usr/bin/env python3
"""Owner-operated candidate lifecycle. No GitHub mutation occurs on import.

The draft body points to an immutable JSON receipt. Candidate workers only append
immutable generation-scoped assets and build receipts; only the owner changes the
pointer. A partial upload or a cancelled worker cannot clear a pause or select a
partial respin. Run one owner CLI at a time. No runner waits for another workflow.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
from importlib import import_module
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import uuid

notes = import_module("release-notes")
changelog = import_module("check-changelog")
from octet_release_identity import CANONICAL_REPOSITORY

REPO = CANONICAL_REPOSITORY
PREFIX = f"repos/{REPO}"
ROOT = Path(__file__).resolve().parents[1]
MARKER = re.compile(r"<!-- octet-candidate:([0-9a-f]{64}) -->")
LANES = ("dependencies", "quality", "test (ubuntu-24.04)", "test (macos-15)",
         "windows (x86_64-pc-windows-gnu)", "first-party-extension-tests",
         "extension-api-v03", "msrv", "smoke-install")
CANDIDATE = "release-candidate.yml"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def command(*args, input=None, binary=False, timeout=300):
    return subprocess.run(args, input=input, check=True, capture_output=True,
                          text=not binary, timeout=timeout).stdout


def api(path, method="GET", data=None, pages=False, binary=False):
    args = ["gh", "api", f"{PREFIX}/{path}", "--method", method]
    if pages:
        args += ["--paginate", "--slurp"]
    if binary:
        args += ["-H", "Accept: application/octet-stream"]
    if data is not None:
        args += ["--input", "-"]
    result = command(*args, input=json.dumps(data) if data is not None else None, binary=binary)
    return result if binary else json.loads(result or "null")


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False) + "\n").encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def assets(release):
    result = {a["name"]: a for a in release["assets"]}
    require(len(result) == len(release["assets"]), "duplicate release asset names")
    return result


def download(asset):
    data = api(f"releases/assets/{asset['id']}", binary=True)
    require(asset.get("digest") == "sha256:" + digest(data), f"download digest mismatch: {asset['name']}")
    return data


def upload(release, name, data):
    existing = assets(release).get(name)
    if existing:
        require(existing.get("digest") == "sha256:" + digest(data), f"immutable asset differs: {name}")
        return
    with tempfile.TemporaryDirectory(prefix="octet-upload-") as temporary:
        path = Path(temporary) / name
        path.write_bytes(data)
        command("gh", "release", "upload", release["tag_name"], "--repo", REPO, str(path))
    hosted = assets(api(f"releases/{release['id']}"))
    require(name in hosted and hosted[name].get("digest") == "sha256:" + digest(data),
            f"uploaded asset digest mismatch: {name}")


def releases():
    return [r for page in api("releases?per_page=100", pages=True) for r in page]


def get_release(version):
    matches = [r for r in releases() if r["tag_name"] == f"v{version}"]
    require(len(matches) <= 1, "ambiguous release")
    return matches[0] if matches else None


def load(release):
    require(not release["prerelease"], "not a stable release")
    pointers = MARKER.findall(release["body"] or "")
    require(len(pointers) == 1 or (not pointers and not release["draft"]), "foreign or ambiguous draft")
    name = f"candidate-receipt-{pointers[0]}.json" if pointers else "ship-receipt.json"
    require(name in assets(release), "receipt missing; rerun start to recover initial draft")
    data = download(assets(release)[name])
    if pointers:
        require(digest(data) == pointers[0], "receipt pointer digest differs")
    state = json.loads(data)
    require(state["schema"] == "octet.candidate.v1" and state["repository"] == REPO
            and release["tag_name"] == "v" + state["version"], "receipt identity differs")
    require(release["target_commitish"] == state["sha"], "release target differs from receipt")
    return state


def save(release, state):
    require(release["draft"], "never edit a published receipt")
    data = encoded(state)
    key = digest(data)
    upload(release, f"candidate-receipt-{key}.json", data)
    # One atomic pointer change; previous complete generations remain intact.
    return api(f"releases/{release['id']}", "PATCH", {
        "body": state["notes"] + f"\n<!-- octet-candidate:{key} -->\n",
        "target_commitish": state["sha"], "name": "v" + state["version"],
    })


def on_main(sha):
    require(re.fullmatch(r"[0-9a-f]{40}", sha), "candidate needs a full SHA")
    comparison = api(f"compare/{sha}...main")
    require(comparison["status"] in ("ahead", "identical"), "candidate is not an ancestor of origin/main")
    return comparison


def qualify(sha):
    # Same lanes and exact-once-success rule as release-octet resolve. A full
    # main PUSH is mandatory here; manual recovery retains its separate waiver.
    rows = [r for page in api(f"actions/workflows/ci.yml/runs?head_sha={sha}&per_page=100", pages=True)
            for r in page["workflow_runs"]]
    rows = [r for r in rows if r["head_sha"] == sha and r["event"] == "push" and r["head_branch"] == "main"]
    run = max(rows, key=lambda r: r["id"], default=None)
    require(run and run["status"] == "completed" and run["conclusion"] == "success",
            "requires completed successful full-tier main push CI for the exact SHA")
    jobs = [j for page in api(f"actions/runs/{run['id']}/jobs?filter=latest&per_page=100", pages=True)
            for j in page["jobs"]]
    for lane in LANES:
        matching = [j for j in jobs if j["name"] == lane]
        require(len(matching) == 1 and matching[0]["conclusion"] == "success", f"CI lacks successful required lane: {lane}")
    return run["id"]


def prepare(version, sha=None, require_head=True):
    root = Path.cwd()
    require(notes.workspace_version(root) == version, "workspace version differs")
    require(not command("git", "status", "--porcelain").strip(), "requires a clean tree")
    head = command("git", "rev-parse", "HEAD").strip()
    require(head == (sha or head), "HEAD must equal candidate SHA")
    if require_head:
        require(head == command("git", "rev-parse", "refs/remotes/origin/main").strip(),
                "HEAD must equal candidate SHA on origin/main; refresh your checkout first")
    errors = changelog.check((root / "CHANGELOG.md").read_text())
    require(not errors, f"check-changelog failed: {errors}")
    require(notes.section(root / "CHANGELOG.md", version)[1], "release changelog section must be dated")
    require(not notes.sync(root, root / "CHANGELOG.md", check=True), "run scripts/release-notes.py sync")
    return head


def groups(version):
    native = {f"octet-{version}-{target}.tar.gz" for target in
              ("aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu")}
    native.add(f"octet-{version}-x86_64-pc-windows-msvc.zip")
    extensions = {f"{name}-{version}.tar.gz" for name in
                  ("octet-codemode", "octet-computer-use", "octet-mcp", "octet-pi-compat", "octet-subagents", "octet-web-search")}
    npm = {f"octet{suffix}-{version}.tgz" for suffix in
           ("", "-darwin-arm64", "-darwin-x64", "-linux-x64-gnu", "-win32-x64")}
    return dict(native=native, extensions=extensions, npm=npm, website={"release.json"})


def expected(version):
    return set().union(*groups(version).values())


def title(state, operation):
    return f"octet v{state['version']} {state['generation']} {operation}"


def workflow_runs(state, workflow, operation):
    rows = [r for page in api(f"actions/workflows/{workflow}/runs?event=workflow_dispatch&per_page=100", pages=True)
            for r in page["workflow_runs"]]
    return sorted((r for r in rows if r["display_title"] == title(state, operation)), key=lambda r: r["id"])


def build_run(state):
    rows = workflow_runs(state, CANDIDATE, "build")
    require(len(rows) <= 1, "duplicate candidate dispatches; inspect Actions before continuing")
    return rows[0] if rows else None


def reconcile(release, state):
    state = copy.deepcopy(state)
    run = build_run(state)
    if run:
        require(run["event"] == "workflow_dispatch" and run["head_branch"] == "main"
                and run["path"] == ".github/workflows/" + CANDIDATE, "candidate run identity differs")
        state["run_ids"] = [run["id"]]
        if run["status"] != "completed" or run["conclusion"] != "success":
            state.update(complete=False, digests={})
    if run and run["status"] == "completed" and run["conclusion"] == "success":
        name = f"build-{state['generation']}-{run['id']}.json"
        require(name in assets(release), "successful build lacks its build receipt")
        built = json.loads(download(assets(release)[name]))
        for key in ("sha", "version", "generation", "versions"):
            require(built[key] == state[key], f"build receipt {key} differs")
        require(built["run_ids"] == [run["id"]], "build receipt run differs")
        require(set(built["digests"]) == expected(state["version"]), "incomplete build receipt")
        state["digests"] = built["digests"]
        state["complete"] = True
    return state, run


def check_digests(release, state):
    require(state["complete"] and set(state["digests"]) == expected(state["version"]), "candidate is incomplete")
    hosted = assets(release)
    for name, entry in state["digests"].items():
        require(re.fullmatch(r"[A-Za-z0-9_.-]+", entry["asset"]), "unsafe candidate asset name")
        require(entry["asset"] in hosted and hosted[entry["asset"]].get("digest") == "sha256:" + entry["sha256"],
                f"candidate digest mismatch: {name}")


def dispatch_build(release, state):
    run = build_run(state)
    if run:
        if run["status"] == "completed" and run["conclusion"] != "success":
            command("gh", "run", "rerun", str(run["id"]), "--failed", "--repo", REPO)
        return
    if state.get("dispatch_requested"):
        # Do not blindly replay a dispatch whose response was interrupted.
        # GitHub has no dispatch idempotency key. A stable run title lets a later
        # invocation recover the accepted run once Actions exposes it.
        raise ValueError("dispatch outcome unknown: inspect Actions; use respin for a new generation only after confirming no run was accepted")
    state["dispatch_requested"] = True
    save(release, state)  # durable intent before the external side effect
    command("gh", "workflow", "run", CANDIDATE, "--repo", REPO, "--ref", "main",
            "-f", f"version={state['version']}", "-f", f"source_commit={state['sha']}",
            "-f", f"candidate={state['generation']}")


def tag_shas(version):
    result = {}
    for tag in (f"v{version}", f"octet-binaries-v{version}", f"octet-serve-v{version}"):
        refs = api(f"git/matching-refs/tags/{tag}")
        exact = [r for r in refs if r["ref"] == f"refs/tags/{tag}"]
        if exact:
            result[tag] = api(f"commits/{tag}")["sha"]  # peel annotated refs too
    return result


def cancel(state):
    run = build_run(state)
    if run and run["status"] != "completed":
        command("gh", "run", "cancel", str(run["id"]), "--repo", REPO)


def new_state(version, sha, ci):
    root = Path.cwd()
    return dict(schema="octet.candidate.v1", repository=REPO, version=version, sha=sha,
                versions={"octet": version, "extensions": version, "npm": version},
                generation=uuid.uuid4().hex, paused=False, complete=False, shipping=False,
                ci_run=ci, run_ids=[], digests={}, notes=notes.render(root / "CHANGELOG.md", version))


def ship_run(state, workflow, operation, wait=False):
    rows = workflow_runs(state, workflow, operation)
    require(len(rows) <= 1, f"duplicate {operation} runs")
    ref = f"octet-{'serve' if operation == 'extensions' else 'binaries'}-v{state['version']}"
    if not rows:
        release = get_release(state["version"])
        intent = f"dispatch-{state['generation']}-{operation}.json"
        require(intent not in assets(release), "ship dispatch outcome unknown; inspect Actions before manual recovery with this candidate generation")
        upload(release, intent, encoded(dict(workflow=workflow, ref=ref, generation=state["generation"])))
        command("gh", "workflow", "run", workflow, "--repo", REPO, "--ref", ref,
                "-f", f"release_tag=v{state['version']}", "-f", f"candidate={state['generation']}")
        # Local-only visibility wait. No runner is held during a handoff.
        for _ in range(30):
            rows = workflow_runs(state, workflow, operation)
            if rows:
                break
            time.sleep(2)
        require(len(rows) == 1, "dispatch not visible; rerun ship after checking Actions")
    run = rows[0]
    require(run["event"] == "workflow_dispatch" and run["head_branch"] == ref
            and run["head_sha"] == state["sha"] and run["path"] == ".github/workflows/" + workflow,
            "ship run ref/SHA/workflow differs")
    if run["status"] == "completed" and run["conclusion"] != "success":
        command("gh", "run", "rerun", str(run["id"]), "--failed", "--repo", REPO)
    if wait:
        # Approvals and execution wait on the owner's computer, not a runner.
        subprocess.run(["gh", "run", "watch", str(run["id"]), "--exit-status", "--repo", REPO], check=True, timeout=86400)
    return run["id"]


def ship(release, state, args):
    state, run = reconcile(release, state)
    require(not state["paused"], "candidate is paused; resume before ship")
    check_digests(release, state)
    require(run and run["status"] == "completed" and run["conclusion"] == "success", "candidate build did not succeed")
    on_main(state["sha"])
    tags = tag_shas(state["version"])
    require(all(sha == state["sha"] for sha in tags.values()), "tag collision: existing tag points at a different SHA")
    if not state["shipping"]:
        require(f"v{state['version']}" not in tags, "canonical tag already exists outside this ship")
        require(not args.skip_npm or valid_reason(args.reason), "--skip-npm requires a nonempty single-line --reason")
        require(args.skip_npm or not args.reason, "--reason requires --skip-npm")
        # Verify bytes, not just GitHub's digest metadata, before any tag exists.
        for entry in state["digests"].values():
            require(digest(download(assets(release)[entry["asset"]])) == entry["sha256"], "candidate download digest differs")
        state.update(shipping=True, skip_npm=args.reason if args.skip_npm else "")
        release = save(release, state)
    else:
        require(not args.skip_npm or args.reason == state["skip_npm"], "npm decision is frozen in ship receipt")
    upload(release, "ship-receipt.json", encoded(state))
    # All collisions checked before creating anything. Never PATCH/DELETE a ref.
    for tag in (f"v{state['version']}", f"octet-binaries-v{state['version']}", f"octet-serve-v{state['version']}"):
        if tag not in tags:
            api("git/refs", "POST", {"ref": "refs/tags/" + tag, "sha": state["sha"]})
    ship_run(state, "release-serve.yml", "extensions", wait=True)
    print(f"binary ship run: {ship_run(state, 'release-octet.yml', 'ship')}")
    print(f"After ship succeeds and publication is verified, land the post-ship docs commit: switch install commands and extension READMEs to {state['version']}.")


def valid_reason(reason):
    return bool(reason.strip()) and not any(c in reason for c in "\r\n")


def workflow_state(version, generation, role):
    release = get_release(version)
    require(release, "candidate draft missing")
    state = load(release)
    require(state["generation"] == generation and not state["paused"], "stale or paused candidate")
    if role == "build":
        require(release["draft"] and not state["shipping"], "candidate is frozen")
        require(os.environ["GITHUB_REF"] == "refs/heads/main", "candidate dispatch must use main")
    else:
        require(state["shipping"], "ship not authorized in receipt")
        tag = f"octet-{'serve' if role == 'extensions' else 'binaries'}-v{version}"
        require(os.environ["GITHUB_REF"] == "refs/tags/" + tag, "sign only on the dedicated tooling-tag ref")
        require(os.environ["GITHUB_SHA"] == state["sha"], "ship workflow must equal candidate SHA")
        require(all(tag_shas(version).get(t) == state["sha"] for t in
                    (f"v{version}", f"octet-binaries-v{version}", f"octet-serve-v{version}")), "ship tags differ")
        check_digests(release, state)
    return release, state


def attach_build(release, state, directory):
    require({p.name for p in directory.iterdir()} == expected(state["version"]), "unexpected candidate asset set")
    built = {k: state[k] for k in ("sha", "version", "generation", "versions")}
    built.update(run_ids=[int(os.environ["GITHUB_RUN_ID"])], digests={})
    for path in sorted(directory.iterdir()):
        require(path.is_file() and not path.is_symlink(), "candidate must be a regular file")
        data = path.read_bytes()
        name = f"build-{state['generation']}-{os.environ['GITHUB_RUN_ID']}-{path.name}"
        upload(release, name, data)
        built["digests"][path.name] = dict(asset=name, sha256=digest(data))
    # Worker never writes the owner receipt/pointer. Pauses cannot be lost.
    latest, current = workflow_state(state["version"], state["generation"], "build")
    require(current["sha"] == state["sha"], "candidate changed during upload")
    upload(latest, f"build-{state['generation']}-{os.environ['GITHUB_RUN_ID']}.json", encoded(built))


def download_group(release, state, group, directory):
    check_digests(release, state)
    directory.mkdir(parents=True, exist_ok=True)
    for name in groups(state["version"])[group]:
        entry = state["digests"][name]
        data = download(assets(release)[entry["asset"]])
        require(digest(data) == entry["sha256"], f"candidate bytes differ: {name}")
        (directory / name).write_bytes(data)


def verify_args(state, role):
    workflow = "release-serve.yml" if role == "extensions" else "release-octet.yml"
    tag = f"octet-{'serve' if role == 'extensions' else 'binaries'}-v{state['version']}"
    return ["--certificate-identity", f"https://github.com/{REPO}/.github/workflows/{workflow}@refs/tags/{tag}",
            "--certificate-oidc-issuer", "https://token.actions.githubusercontent.com",
            "--certificate-github-workflow-name", "octet extension release" if role == "extensions" else "octet binary release",
            "--certificate-github-workflow-repository", REPO, "--certificate-github-workflow-sha", state["sha"]]


def attach_signed(release, state, role, directory):
    if role == "extensions":
        names = groups(state["version"])["extensions"] | {"SHA256SUMS"}
        require({p.name for p in directory.iterdir()} == names | {n + ".sigstore.json" for n in names},
                "incomplete signed extension set")
    for name in groups(state["version"])["extensions" if role == "extensions" else "native"]:
        require(digest((directory / name).read_bytes()) == state["digests"][name]["sha256"],
                f"signed asset differs from candidate: {name}")
    # Never replace even a draft's final names: identical bytes or a verifying
    # signature for those exact bytes are the only accepted interrupted outputs.
    for path in sorted(directory.iterdir()):
        existing = assets(release).get(path.name)
        if existing and path.name.endswith(".sigstore.json"):
            with tempfile.TemporaryDirectory() as temporary:
                signature = Path(temporary) / path.name
                signature.write_bytes(download(existing))
                command("cosign", "verify-blob", "--bundle", str(signature), *verify_args(state, role),
                        str(directory / path.name.removesuffix(".sigstore.json")))
        else:
            upload(release, path.name, path.read_bytes())


def publish_native(release, state, directory):
    names = groups(state["version"])["native"] | {"install-octet.sh", "install-octet.ps1", "OCTET_SHA256SUMS", "OCTET_RELEASE_METADATA.json"}
    require({p.name for p in directory.iterdir()} == names | {n + ".sigstore.json" for n in names} | {"release.json"},
            "incomplete signed native set")
    require(digest((directory / "release.json").read_bytes()) == state["digests"]["release.json"]["sha256"],
            "website artifact differs from candidate")
    # Extension signing has finished before binary dispatch; fail once, never poll.
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        for name in groups(state["version"])["extensions"] | {"SHA256SUMS"}:
            for item in (name, name + ".sigstore.json"):
                require(item in assets(release), f"extension signing incomplete: {item}")
                (root / item).write_bytes(download(assets(release)[item]))
            command("cosign", "verify-blob", "--bundle", str(root / (name + ".sigstore.json")),
                    *verify_args(state, "extensions"), str(root / name))
            if name != "SHA256SUMS":
                require(digest((root / name).read_bytes()) == state["digests"][name]["sha256"], "signed extension differs from candidate")
    attach_signed(release, state, "ship", directory)
    release, current = workflow_state(state["version"], state["generation"], "ship")
    require(current == state, "receipt changed during signing")
    if release["draft"]:
        api(f"releases/{release['id']}", "PATCH", {"draft": False, "prerelease": False,
            "make_latest": "false", "body": state["notes"]})


def provider_acceptance(sha):
    rows = [r for page in api(f"actions/workflows/provider-acceptance.yml/runs?head_sha={sha}&event=workflow_dispatch&per_page=100", pages=True)
            for r in page["workflow_runs"]]
    rows = [r for r in rows if r["head_sha"] == sha and r["event"] == "workflow_dispatch"
            and r["status"] == "completed" and r["conclusion"] == "success"]
    require(rows, "no successful protected provider/audio acceptance run for candidate SHA")
    run = max(rows, key=lambda r: r["id"])
    approvals = api(f"actions/runs/{run['id']}/approvals")
    require(any(a["state"] == "approved" and any(e["name"] == "stable-release-provider-acceptance" for e in a["environments"])
                for a in approvals), "provider acceptance lacks recorded protected-environment approval")
    return run["id"]


def internal(args):
    role = args.role
    release, state = workflow_state(args.version, args.candidate, role)
    if args.command == "_resolve":
        on_main(state["sha"])
        if role == "build":
            require(args.sha == state["sha"], "dispatch SHA differs from candidate")
        outputs = dict(tag=f"v{state['version']}", version=state["version"], source_commit=state["sha"],
                       workflow_commit=state["sha"], core_workflow_commit=state["sha"],
                       core_workflow_tag=f"octet-binaries-v{state['version']}", mode="ship",
                       skip_npm=str(bool(state.get("skip_npm"))).lower())
        for key, value in outputs.items():
            print(f"{key}={value}")
        if role != "build":
            evidence = provider_acceptance(state["sha"]) if os.environ.get("INPUT_REQUIRE_PROVIDER_ACCEPTANCE") == "true" else None
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
                summary.write(f"Provider/audio acceptance: run {evidence}\n" if evidence else "Provider/audio acceptance: **NOT RUN** (optional)\n")
                if state.get("skip_npm"):
                    summary.write(f"npm skipped: {state['skip_npm']}\n")
    elif args.command == "_attach":
        attach_build(release, state, args.assets)
    elif args.command == "_download":
        download_group(release, state, args.group, args.assets)
    elif args.command == "_signed":
        attach_signed(release, state, role, args.assets)
    elif args.command == "_publish":
        publish_native(release, state, args.assets)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "start", "status", "pause", "resume", "respin", "ship", "abort",
                                             "_resolve", "_attach", "_download", "_signed", "_publish"))
    parser.add_argument("version")
    parser.add_argument("--sha")
    parser.add_argument("--skip-npm", action="store_true")
    parser.add_argument("--reason", default="")
    parser.add_argument("--candidate")
    parser.add_argument("--role", choices=("build", "extensions", "ship"), default="build")
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--group", choices=("native", "extensions", "npm", "website"))
    args = parser.parse_args(argv)
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version), "version must be X.Y.Z")
    if args.command.startswith("_"):
        internal(args)
        return
    if args.command == "prepare":
        print(prepare(args.version, args.sha))
        return
    release = get_release(args.version)
    if args.command == "start" and not release:
        sha = prepare(args.version, args.sha)
        on_main(sha)
        require(not tag_shas(args.version), "release tags already exist")
        state = new_state(args.version, sha, qualify(sha))
        # Seed the receipt in the body until its asset/pointer is installed.
        # This recovers a crash between draft creation and first receipt upload.
        release = api("releases", "POST", dict(tag_name="v" + args.version, target_commitish=sha,
                      name="v" + args.version, draft=True, prerelease=False,
                      body=state["notes"] + "\n<!-- octet-candidate-seed:" + encoded(state).hex() + " -->"))
        release = save(release, state)
    elif release and args.command == "start" and "octet-candidate-seed:" in (release["body"] or ""):
        seed = re.search(r"<!-- octet-candidate-seed:([0-9a-f]+) -->", release["body"])
        state = json.loads(bytes.fromhex(seed[1]))
        release = save(release, state)
    if not release:
        require(args.command in ("status", "abort"), "no candidate draft; run start")
        print(json.dumps({"state": "absent-or-aborted", "version": args.version}))
        return
    state = load(release)
    if args.command == "status":
        current, run = reconcile(release, state)
        print(json.dumps(dict(state="published" if not release["draft"] else "shipping" if state["shipping"] else
                              "paused" if state["paused"] else "complete" if current["complete"] else "building",
                              receipt=current, run=run, commits_since=on_main(state["sha"])), indent=2))
        return
    if args.command == "ship":
        ship(release, state, args)
        return
    require(release["draft"] and not state["shipping"], "candidate is frozen for ship or already published")
    if args.command == "pause":
        state["paused"] = True
        save(release, state)  # block before asynchronous cancellation
        cancel(state)
    elif args.command == "abort":
        require(not tag_shas(args.version), "abort never deletes a tag")
        state["paused"] = True
        save(release, state)
        cancel(state)
        api(f"releases/{release['id']}", "DELETE")
    elif args.command == "respin":
        require(not tag_shas(args.version), "cannot respin tagged source")
        sha = args.sha or state["sha"]
        prepare(args.version, sha, require_head=False)
        on_main(sha)
        ci = qualify(sha) if sha != state["sha"] else state["ci_run"]
        # Pause first. Stale uploads can never change the new generation pointer.
        state["paused"] = True
        release = save(release, state)
        cancel(state)
        state = new_state(args.version, sha, ci)
        release = save(release, state)
        dispatch_build(release, state)
    elif args.command in ("start", "resume"):
        require(args.command != "start" or not state["paused"], "candidate is paused; use resume")
        state, run = reconcile(release, state)
        state["paused"] = False
        release = save(release, state)
        if not state["complete"]:
            dispatch_build(release, state)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        print(f"release: {error}", file=sys.stderr)
        sys.exit(1)
