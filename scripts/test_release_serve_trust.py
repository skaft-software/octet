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
                if name != "publish":
                    self.assertNotIn("always()", job)
                else:
                    self.assertIn("needs.resolve.result == 'success'", job)

    def test_dispatch_executes_trigger_sha_not_input_selected_source(self):
        for name in ("security", "launcher", "build-bundles", "publish", "verify-published"):
            with self.subTest(job=name):
                checkouts = re.findall(
                    r"^      - uses: actions/checkout@[^\n]+\n(.*?)(?=^      - |\Z)",
                    self.jobs[name], re.MULTILINE | re.DOTALL,
                )
                self.assertEqual(1, len(checkouts))
                self.assertIn(
                    "ref: ${{ needs.resolve.outputs.source_commit }}\n",
                    checkouts[0],
                )

    def test_no_release_cache_actions_or_implicit_node_caches(self):
        self.assertNotRegex(self.workflow, r"uses: (?:actions/cache|Swatinem/rust-cache)[@/]")
        self.assertNotRegex(self.workflow, re.compile(r"^\s+cache:", re.MULTILINE))

    def test_signing_remains_protected_and_actions_remain_pinned(self):
        self.assertIn("environment: stable-release-publish", self.jobs["publish"])
        expected = {
            "resolve": {"actions": "read", "contents": "write"},
            "publish": {"actions": "read", "contents": "write", "id-token": "write"},
        }
        for name, job in self.jobs.items():
            with self.subTest(job=name):
                header = job.split("    steps:\n", 1)[0]
                permissions = dict(re.findall(r"(?m)^      ([\w-]+): (read|write|none)$", header))
                self.assertEqual(expected.get(name, {}), permissions)
        for action in re.findall(r"^\s+- uses: (\S+)", self.workflow, re.MULTILINE):
            self.assertRegex(action, r"@[a-f0-9]{40}$")

    def run_resolve(self, *, event="workflow_dispatch", ref_type="tag", ref="v0.8.2",
                    tag="v0.8.2", workflow_sha=SOURCE, ancestor=True,
                    changed_paths=".github/workflows/release-serve.yml",
                    serve_changed=True, version="0.8.2", draft="false", prerelease="false",
                    binary_status="completed", binary_conclusion="success", binary_states=None):
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
sleep() { :; }
gh() {
  case "$*" in
    *'/release-octet.yml/runs'*)
      count=$(cat "$TEST_POLLS" 2>/dev/null || echo 0)
      count=$((count + 1))
      printf '%s' "$count" > "$TEST_POLLS"
      state=$(printf '%s\n' "$TEST_BINARY_STATES" | sed -n "${count}p")
      [[ "$state" == missing ]] || printf '42\n' ;;
    *'/actions/runs/42'*)
      count=$(cat "$TEST_POLLS")
      state=$(printf '%s\n' "$TEST_BINARY_STATES" | sed -n "${count}p")
      printf '%s\t%s\t%s\n' "$TEST_SOURCE" "${state:-$TEST_BINARY_STATUS}" "$TEST_BINARY_CONCLUSION" ;;
    'release view '*) printf '%s\t%s\t%s\n' "$TEST_TAG" "$TEST_DRAFT" "$TEST_PRERELEASE" ;;
    *) return 99 ;;
  esac
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary) / "output"
            env = {
                "PATH": os.defpath, "HOME": temporary,
                "GITHUB_EVENT_NAME": event, "GITHUB_REF_TYPE": ref_type,
                "GITHUB_REF_NAME": ref, "GITHUB_SHA": workflow_sha,
                "GITHUB_REPOSITORY": "skaft-software/octet",
                "INPUT_RELEASE_TAG": tag, "INPUT_REQUIRE_PROVIDER_ACCEPTANCE": "false",
                "GITHUB_OUTPUT": str(output),
                "GITHUB_STEP_SUMMARY": str(pathlib.Path(temporary) / "summary"),
                "TEST_SOURCE": SOURCE, "TEST_ANCESTOR": str(ancestor).lower(),
                "TEST_PATHS": changed_paths, "TEST_SERVE_CHANGED": str(serve_changed).lower(),
                "TEST_VERSION": version, "TEST_TAG": tag or "v0.8.2",
                "TEST_DRAFT": draft, "TEST_PRERELEASE": prerelease,
                "TEST_BINARY_STATUS": binary_status, "TEST_BINARY_CONCLUSION": binary_conclusion,
                "TEST_BINARY_STATES": "\n".join(binary_states or []),
                "TEST_POLLS": str(pathlib.Path(temporary) / "polls"),
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
            {"workflow_sha": TOOLING},
            {"tag": "v0.8.2;echo injection"},
            {"tag": "v0.8.2-rc.1"},
        ):
            with self.subTest(overrides=overrides):
                result, output = self.run_resolve(**overrides)
                self.assertNotEqual(0, result.returncode)
                self.assertEqual("", output)

    def test_release_resolve_never_waits_for_binary_workflow(self):
        # A legacy repair may attach only to a published release. Candidate ship
        # uses the receipt path and the owner waits locally before binary dispatch.
        for binary_states in ([], ["missing", "in_progress", "completed"], ["failure"]):
            result, output = self.run_resolve(binary_states=binary_states)
            self.assertEqual(0, result.returncode, result.stderr)
            self.assertIn(f"source_commit={SOURCE}\n", output)
            self.assertNotIn("waiting for", result.stdout)
        self.assertNotIn("release-octet.yml/runs", self.jobs["resolve"])
        self.assertNotIn("sleep ", self.jobs["resolve"])
        result, output = self.run_resolve(draft="true")
        self.assertNotEqual(0, result.returncode)
        self.assertIn("still a draft", result.stderr)
        self.assertEqual("", output)

    def test_same_commit_tooling_dispatch_needs_no_workflow_diff(self):
        result, output = self.run_resolve(ref="octet-serve-v0.8.2", changed_paths="", serve_changed=False)
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn(f"source_commit={SOURCE}\n", output)
        self.assertIn(f"workflow_commit={SOURCE}\n", output)

    def test_release_source_must_be_on_main_even_without_a_repair(self):
        result, output = self.run_resolve(ancestor=False)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("release tags must point at a commit on main", result.stderr)
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

    def run_publish(self, *, existing=None):
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is required")
        step = self.workflow.split("      - name: Attach assets to the canonical octet release\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n  verify-published:", 1)[0])
        stubs = r'''
set -euo pipefail
git() {
  case "$1" in
    fetch) printf 'git %s\n' "$*" >> "$TEST_REQUESTS" ;;
    rev-parse) printf '%s\n' "$SOURCE_COMMIT" ;;
    *) return 98 ;;
  esac
}
gh() {
  printf '%s\n' "$*" >> "$TEST_REQUESTS"
  case "$1 $2" in
    'release view')
      if [[ "$*" == *'--json assets'* ]]; then cat "$TEST_ASSETS"
      else printf '%s\tfalse\tfalse\n' "$RELEASE_TAG"; fi ;;
    'release download')
      shift 3
      destination= pattern=
      while (($#)); do
        case "$1" in
          --pattern) pattern=$2; shift 2 ;;
          --dir) destination=$2; shift 2 ;;
          --clobber) shift ;;
          *) return 98 ;;
        esac
      done
      [[ -f "$TEST_EXISTING/$pattern" ]] || return 1
      mkdir -p "$destination"
      cp "$TEST_EXISTING/$pattern" "$destination/$pattern" ;;
    'release upload')
      shift 3
      for file in "$@"; do
        printf '%s\tsha256:%s\n' "${file##*/}" "$(sha256sum "$file" | awk '{print $1}')" >> "$TEST_ASSETS"
      done ;;
    *) return 99 ;;
  esac
}
cosign() {
  printf 'cosign %s\n' "$*" >> "$TEST_REQUESTS"
  [[ "$1" == verify-blob ]] || return 97
  shift
  bundle= identity= sha= blob=
  while (($#)); do
    case "$1" in
      --bundle) bundle=$2; shift 2 ;;
      --certificate-identity) identity=$2; shift 2 ;;
      --certificate-oidc-issuer) shift 2 ;;
      --certificate-github-workflow-name) shift 2 ;;
      --certificate-github-workflow-repository) shift 2 ;;
      --certificate-github-workflow-sha) sha=$2; shift 2 ;;
      *) blob=$1; shift ;;
    esac
  done
  [[ -f "$bundle" && -f "$blob" && "$sha" == "$SOURCE_COMMIT" \
    && "$identity" == "$TEST_COSIGN_IDENTITY" ]] || return 1
  grep -Fxq VALID "$bundle"
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            local = root / "release-assets"
            local.mkdir()
            names = {
                "octet-mcp-0.8.2.tar.gz": b"bundle fixture\n",
                "octet-mcp-0.8.2.tar.gz.sigstore.json": b"fresh signature\n",
                "SHA256SUMS": b"sums fixture\n",
                "SHA256SUMS.sigstore.json": b"fresh signature\n",
            }
            for name, content in names.items():
                (local / name).write_bytes(content)
            assets = root / "assets"
            assets.write_text("")
            if existing:
                (root / "existing").mkdir()
                lines = []
                for name, content in existing.items():
                    (root / "existing" / name).write_bytes(content)
                    lines.append(f"{name}\tsha256:{__import__('hashlib').sha256(content).hexdigest()}")
                assets.write_text("\n".join(lines) + "\n")
            requests = root / "requests"
            env = {
                "PATH": os.defpath, "HOME": temporary, "RUNNER_TEMP": temporary,
                "GITHUB_REPOSITORY": "skaft-software/octet", "RELEASE_TAG": "v0.8.2",
                "SOURCE_COMMIT": SOURCE, "WORKFLOW_COMMIT": SOURCE, "WORKFLOW_TAG": "v0.8.2",
                "COSIGN_IDENTITY": "https://github.com/skaft-software/octet/.github/workflows/release-serve.yml@refs/tags/v0.8.2",
                "TEST_COSIGN_IDENTITY": "https://github.com/skaft-software/octet/.github/workflows/release-serve.yml@refs/tags/v0.8.2",
                "WORKFLOW_REPOSITORY": "skaft-software/octet",
                "TEST_REQUESTS": str(requests), "TEST_ASSETS": str(assets),
                "TEST_EXISTING": str(root / "existing"),
            }
            result = subprocess.run([bash, "-c", stubs + script], cwd=root, env=env,
                                    capture_output=True, text=True)
            return result, requests.read_text() if requests.exists() else ""

    def test_fresh_publish_uploads_every_extension_asset(self):
        result, calls = self.run_publish()
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        uploads = [line for line in calls.splitlines() if line.startswith("release upload")]
        self.assertEqual(1, len(uploads))
        for name in ("octet-mcp-0.8.2.tar.gz", "SHA256SUMS", "SHA256SUMS.sigstore.json"):
            self.assertIn(name, uploads[0])

    def test_rerun_converges_after_a_partial_upload(self):
        existing = {
            "octet-mcp-0.8.2.tar.gz": b"bundle fixture\n",
            "octet-mcp-0.8.2.tar.gz.sigstore.json": b"VALID\n",
            "SHA256SUMS": b"sums fixture\n",
            "SHA256SUMS.sigstore.json": b"VALID\n",
        }
        result, calls = self.run_publish(existing=existing)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual([], [line for line in calls.splitlines() if line.startswith("release upload")])
        self.assertEqual(2, calls.count("cosign verify-blob"))

    def test_rerun_rejects_changed_or_unverifiable_evidence(self):
        for existing, message in (
            ({"octet-mcp-0.8.2.tar.gz": b"changed bundle\n"},
             "immutable release asset differs from the existing asset"),
            ({"octet-mcp-0.8.2.tar.gz.sigstore.json": b"INVALID\n"},
             "does not verify the immutable candidate"),
        ):
            with self.subTest(message=message):
                result, calls = self.run_publish(existing=existing)
                self.assertNotEqual(0, result.returncode)
                self.assertIn(message, result.stderr)
                self.assertEqual([], [line for line in calls.splitlines() if line.startswith("release upload")])


if __name__ == "__main__":
    unittest.main()
