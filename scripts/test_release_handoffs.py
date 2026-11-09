#!/usr/bin/env python3
"""Execute release handoff workflow steps against isolated, offline fixtures."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parent.parent
REPOSITORY = "skaft-software/octet"
VERSION = "0.10.3"
TAG = f"v{VERSION}"
SOURCE = "a" * 40
TOOLING = "b" * 40


def step_script(workflow, name):
    step = workflow.split(f"      - name: {name}\n", 1)[1]
    return textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n      - ", 1)[0])


# Only the external GitHub and Sigstore boundaries are mocked. The workflow's
# real checksum checks and metadata regeneration execute on fixture assets.
STUBS = r'''
set -euo pipefail
gh() {
  if [[ "$1 $2" == "release download" ]]; then
    shift 3
    patterns=()
    while (($#)); do
      case "$1" in
        --dir) dest=$2; shift 2 ;;
        --pattern) patterns+=("$2"); shift 2 ;;
        --clobber) shift ;;
        *) return 98 ;;
      esac
    done
    mkdir -p "$dest"
    for file in "$TEST_FIXTURES"/*; do
      name=${file##*/}
      for pattern in "${patterns[@]}"; do
        if [[ "$name" == $pattern ]]; then cp "$file" "$dest/$name"; break; fi
      done
    done
  elif [[ "$1 $2" == "release view" ]]; then
    cat "$TEST_RELEASE_JSON"
  else
    return 99
  fi
}
git() {
  case "$1" in
    fetch) : ;;
    rev-parse) printf '%s\n' "$SOURCE_COMMIT" ;;
    *) return 98 ;;
  esac
}
cosign() {
  [[ "$1" == verify-blob ]] || return 97
  shift
  identity= sha= issuer= name= repo= bundle= blob=
  while (($#)); do
    case "$1" in
      --bundle) bundle=$2; shift 2 ;;
      --certificate-identity) identity=$2; shift 2 ;;
      --certificate-oidc-issuer) issuer=$2; shift 2 ;;
      --certificate-github-workflow-name) name=$2; shift 2 ;;
      --certificate-github-workflow-repository) repo=$2; shift 2 ;;
      --certificate-github-workflow-sha) sha=$2; shift 2 ;;
      *) blob=$1; shift ;;
    esac
  done
  [[ -f "$bundle" && -f "$blob" && "$identity" == "$TEST_IDENTITY" \
    && "$sha" == "$TEST_SHA" && "$issuer" == https://token.actions.githubusercontent.com \
    && "$name" == 'octet binary release' && "$repo" == "$WORKFLOW_REPOSITORY" ]] || return 1
  printf '%s\n' "$blob" >> "$TEST_VERIFIED"
}
'''


class ReleaseHandoffTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="octet-handoff-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fixtures = self.root / "fixtures"
        self.fixtures.mkdir()
        (self.root / "scripts").symlink_to(ROOT / "scripts", target_is_directory=True)
        (self.root / "source").mkdir()
        (self.root / "source/scripts").symlink_to(ROOT / "scripts", target_is_directory=True)
        names = ["install-octet.sh", "install-octet.ps1"] + [
            f"octet-{VERSION}-{target}.tar.gz" for target in (
                "aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"
            )
        ] + [f"octet-{VERSION}-x86_64-pc-windows-msvc.zip"]
        for name in names:
            (self.fixtures / name).write_text(f"fixture {name}\n")
        sums = "".join(
            f"{hashlib.sha256((self.fixtures / name).read_bytes()).hexdigest()}  ./{name}\n"
            for name in sorted(names)
        )
        (self.fixtures / "OCTET_SHA256SUMS").write_text(sums)
        self.binary_workflow = (ROOT / ".github/workflows/release-octet.yml").read_text()
        self.brew_workflow = (ROOT / ".github/workflows/homebrew-formula.yml").read_text()

    def fixture_signer(self, signer):
        signer_tag = TAG if signer == "canonical" else f"octet-binaries-{TAG}"
        commit = SOURCE if signer == "canonical" else TOOLING
        ref = f"{REPOSITORY}/.github/workflows/release-octet.yml@refs/tags/{signer_tag}"
        subprocess.run([
            "python3", str(ROOT / "scripts/generate-octet-release-metadata.py"),
            VERSION, TAG, SOURCE, commit, ref, REPOSITORY,
            str(self.fixtures / "OCTET_SHA256SUMS"), str(self.fixtures / "OCTET_RELEASE_METADATA.json"),
            "--include-windows-msvc",
        ], check=True, capture_output=True)
        for file in list(self.fixtures.iterdir()):
            if not file.name.endswith(".sigstore.json"):
                (self.fixtures / f"{file.name}.sigstore.json").write_text("offline signature fixture\n")
        release = {"assets": [
            {"name": file.name, "digest": "sha256:" + hashlib.sha256(file.read_bytes()).hexdigest()}
            for file in self.fixtures.iterdir()
        ]}
        release_json = self.root / "release.json"
        release_json.write_text(json.dumps(release))
        return {
            "PATH": os.defpath, "HOME": str(self.root), "RUNNER_TEMP": str(self.root),
            "TEST_FIXTURES": str(self.fixtures), "TEST_RELEASE_JSON": str(release_json),
            "TEST_VERIFIED": str(self.root / "verified"), "TEST_IDENTITY": f"https://github.com/{ref}",
            "TEST_SHA": commit, "RELEASE_TAG": TAG, "VERSION": VERSION,
            "SOURCE_COMMIT": SOURCE, "WORKFLOW_REPOSITORY": REPOSITORY,
            "WORKFLOW_COMMIT": commit, "WORKFLOW_REF": ref,
            "CORE_WORKFLOW_COMMIT": commit, "CORE_WORKFLOW_TAG": signer_tag,
            "CORE_WORKFLOW_REF": ref, "COSIGN_OIDC_ISSUER": "https://token.actions.githubusercontent.com",
        }

    def run_step(self, script, env):
        return subprocess.run(["bash", "-c", STUBS + script], cwd=self.root, env=env,
                              text=True, capture_output=True, timeout=30)

    def test_npm_homebrew_and_installer_verify_both_exact_signers(self):
        for signer in ("canonical", "legacy"):
            for channel, workflow, name in (
                ("npm", self.binary_workflow, "Download and verify the settled immutable release"),
                ("brew", self.brew_workflow, "Download and verify the signed immutable release handoff"),
                ("installer", self.binary_workflow, "Verify existing core release provenance"),
            ):
                with self.subTest(signer=signer, channel=channel):
                    # Each workflow step starts in a clean runner directory.
                    env = self.fixture_signer(signer)
                    for dirname in ("release-assets", "upstream-assets"):
                        path = self.root / dirname
                        if path.exists():
                            shutil.rmtree(path)
                    script = step_script(workflow, name).replace("${{ needs.resolve.outputs.version }}", VERSION)
                    result = self.run_step(script, env)
                    self.assertEqual(0, result.returncode, result.stdout + result.stderr)
                    verified = Path(env["TEST_VERIFIED"]).read_text()
                    self.assertIn("OCTET_SHA256SUMS", verified)
                    if channel != "installer":
                        self.assertIn("OCTET_RELEASE_METADATA.json", verified)
                        self.assertIn("install-octet.ps1", verified)
                        self.assertIn("x86_64-pc-windows-msvc.zip", verified)
                    Path(env["TEST_VERIFIED"]).unlink()

    def test_signer_or_commit_mismatch_fails_before_handoff(self):
        for signer in ("canonical", "legacy"):
            for field in ("CORE_WORKFLOW_REF", "CORE_WORKFLOW_COMMIT"):
                with self.subTest(signer=signer, field=field):
                    env = self.fixture_signer(signer)
                    if field == "CORE_WORKFLOW_REF":
                        env[field] += "-wrong"
                    else:
                        env[field] = "c" * 40
                    release_assets = self.root / "release-assets"
                    if release_assets.exists():
                        shutil.rmtree(release_assets)
                    result = self.run_step(step_script(self.binary_workflow, "Download and verify the settled immutable release"), env)
                    self.assertNotEqual(0, result.returncode)

    def test_homebrew_dispatch_requires_either_exact_signer_tag(self):
        step = self.brew_workflow.split("      - id: resolve\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n      - ", 1)[0])
        output = self.root / "output"
        env = {"PATH": os.defpath, "INPUT_RELEASE_TAG": TAG, "CURRENT_REF_TYPE": "tag",
               "CURRENT_SHA": SOURCE, "WORKFLOW_REPOSITORY": REPOSITORY, "GITHUB_OUTPUT": str(output)}
        for ref in (TAG, f"octet-binaries-{TAG}"):
            output.write_text("")
            result = subprocess.run(["bash", "-c", script], cwd=self.root,
                                    env=env | {"CURRENT_REF_NAME": ref}, capture_output=True, text=True)
            self.assertEqual(0, result.returncode, result.stderr)
            self.assertIn(f"@refs/tags/{ref}", output.read_text())
        for ref in ("main", "v0.10.2", f"{TAG}-rc.1"):
            result = subprocess.run(["bash", "-c", script], cwd=self.root,
                                    env=env | {"CURRENT_REF_NAME": ref}, capture_output=True, text=True)
            self.assertNotEqual(0, result.returncode)


if __name__ == "__main__":
    unittest.main()
