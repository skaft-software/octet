"""Replay the published 0.10.3 policy, NOT the current installer policy.

The fixture preserves the published function and bound configuration verbatim.
Only downloading/executing cosign is replaced, with an offline policy-only stub.
These tests do not establish cryptographic or hosted release qualification.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

FIXTURES = Path(__file__).resolve().parents[1] / "fixtures/release-v0103"
EVIDENCE = json.loads((FIXTURES / "evidence.json").read_text())

# Check the actual arguments supplied by the frozen verifier. No fixture can
# pass merely because the stub exits zero: all identity/binding flags must match.
POLICY_STUB = '''#!/usr/bin/env python3
import json, re, sys
args = sys.argv[1:]
def flag(name):
    return args[args.index(name) + 1]
signer = json.load(open(flag("--bundle")))
assert args[0] == "verify-blob"
accepted = re.fullmatch(flag("--certificate-identity-regexp"), signer["san"]) is not None
for option, key in (("--certificate-oidc-issuer", "issuer"),
                    ("--certificate-github-workflow-name", "name"),
                    ("--certificate-github-workflow-repository", "repository"),
                    ("--certificate-github-workflow-sha", "sha")):
    accepted = accepted and flag(option) == signer[key]
sys.exit(0 if accepted else 1)
'''


class PublishedInstallerPolicyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="octet-legacy-policy-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.cosign = self.root / "policy-only-cosign"
        self.cosign.write_text(POLICY_STUB)
        self.cosign.chmod(0o700)

    def verify(self, *, signer=None, manifest_version="0.10.3"):
        manifest = self.root / "checksums"
        names = ["install-octet.sh", "install-octet.ps1"] + [
            f"octet-{manifest_version}-{target}"
            for target in ("aarch64-apple-darwin.tar.gz", "x86_64-apple-darwin.tar.gz",
                           "x86_64-pc-windows-msvc.zip", "x86_64-unknown-linux-gnu.tar.gz")
        ]
        manifest.write_text("".join(f"{'a' * 64}  ./{name}\n" for name in names))
        bundle = self.root / "policy-evidence.json"
        bundle.write_text(json.dumps(signer or EVIDENCE["signer"]))
        command = '''set -eu
. "$1"
install_pinned_cosign() { cosign_path="$FAKE_COSIGN"; }
ui_stage() { :; }
verified_archive_sha256 "$2" "$3" "$4"
'''
        return subprocess.run(
            ["sh", "-c", command, "legacy-policy", str(FIXTURES / "verify-checksums.sh"),
             str(manifest), str(bundle), names[-1]],
            env={**os.environ, "FAKE_COSIGN": str(self.cosign)},
            capture_output=True, text=True, timeout=10,
        )

    def test_fixture_provenance_and_published_certificate_identity(self):
        self.assertEqual(hashlib.sha256((FIXTURES / "verify-checksums.sh").read_bytes()).hexdigest(),
                         EVIDENCE["excerpt_sha256"])
        self.assertEqual(hashlib.sha256((FIXTURES / "install-octet.ps1").read_bytes()).hexdigest(),
                         EVIDENCE["asset_sha256"]["install-octet.ps1"])
        certificate = base64.b64decode(EVIDENCE["certificate_der_base64"])
        self.assertIn(EVIDENCE["signer"]["san"].encode(), certificate)
        self.assertIn(EVIDENCE["commit"].encode(), certificate)

    def test_published_repair_tag_identity_is_accepted_for_its_original_release(self):
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "a" * 64)

    def test_same_version_canonical_tag_identity_is_accepted(self):
        signer = dict(EVIDENCE["signer"])
        signer["san"] = signer["san"].replace("octet-binaries-v", "v")
        result = self.verify(signer=signer)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_main_dispatch_identity_is_rejected_even_with_original_workflow_binding(self):
        signer = dict(EVIDENCE["signer"])
        signer["san"] = signer["san"].split("@refs/")[0] + "@refs/heads/main"
        result = self.verify(signer=signer)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("provenance verification failed", result.stderr)

    def test_new_version_tag_is_rejected_by_frozen_installer(self):
        signer = dict(EVIDENCE["signer"])
        signer["san"] = signer["san"].replace("v0.10.3", "v0.10.4")
        result = self.verify(signer=signer, manifest_version="0.10.4")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("provenance verification failed", result.stderr)

    def test_new_manifest_is_rejected_even_with_accepted_old_identity(self):
        result = self.verify(manifest_version="0.10.4")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected asset set", result.stderr)

    def test_binding_is_not_just_an_identity_regex(self):
        for key, value in (("sha", "e" * 40), ("issuer", "https://example.com"),
                           ("name", "other workflow"), ("repository", "other/repo")):
            with self.subTest(key=key):
                signer = {**EVIDENCE["signer"], key: value}
                result = self.verify(signer=signer)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("provenance verification failed", result.stderr)

    def test_published_powershell_has_no_sigstore_identity_verifier(self):
        script = (FIXTURES / "install-octet.ps1").read_text()
        self.assertIn("this script does not independently verify the Sigstore signature", script)
        self.assertIn("Get-FileHash -LiteralPath $archive -Algorithm SHA256", script)
        self.assertIn("[string] $Version = '0.10.3'", script)
        self.assertIn("releases/download/v$Version", script)
        self.assertNotIn("--certificate-identity", script)


if __name__ == "__main__":
    unittest.main()
