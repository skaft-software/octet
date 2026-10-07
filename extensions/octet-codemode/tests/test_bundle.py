"""Release wiring: the bundle is complete, deterministic and Node/Python-free."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

from support import REPOSITORY, ROOT, run_checked

REQUIRED_BUNDLE_FILES = (
    "extension.toml",
    "bin/codemode",
    "Cargo.toml",
    "Cargo.lock",
    "src/main.rs",
    "src/wasi.rs",
    "src/native.rs",
    "src/runner.rs",
    "src/guest.js",
    "src/js/common.js",
    "src/js/discovery.js",
    "README.md",
    "THIRD_PARTY_NOTICES.md",
    "LICENSE",
    "vendor/quickjs-wasi/quickjs.wasm",
    "vendor/pi-codemode/LICENSE",
    "vendor/pi-codemode/dist/runtime/prelude-source.js",
    "vendor/quickjs-wasi/LICENSE",
    "vendor/PROVENANCE.json",
    "vendor/SHA256SUMS",
    "vendor/regenerate.py",
    "tests/test_runner.py",
    "tests/test_adapter.py",
)


class BundleTests(unittest.TestCase):
    def test_vendor_integrity_and_offline_regeneration(self):
        result = run_checked([sys.executable, str(ROOT / "vendor/regenerate.py"), "--check"])
        self.assertIn("offline", result.stdout)

    def test_bundle_has_no_node_or_python_runtime_requirement(self):
        manifest = (ROOT / "extension.toml").read_text()
        self.assertIn('command = "bin/codemode"', manifest)
        self.assertNotIn("extension.py", manifest)
        self.assertFalse((ROOT / "extension.py").exists())
        self.assertFalse((ROOT / ".node-version").exists())
        for pattern in ("*.mjs", "runtime.mjs", "sandbox.mjs"):
            self.assertEqual(list(ROOT.glob(pattern)), [], pattern)

    def staged_bundle(self, directory):
        """A clean bundle copy: the release path packages tracked files only."""
        staging = directory / "source/octet-codemode"
        tracked = subprocess.run(
            ["git", "-C", str(REPOSITORY), "ls-files", "-z", "--", "extensions/octet-codemode"],
            capture_output=True, check=True).stdout.split(b"\0")
        prefix = b"extensions/octet-codemode/"
        copied = 0
        for encoded in tracked:
            if not encoded:
                continue
            source = REPOSITORY / encoded.decode()
            if not source.is_file():
                continue
            relative = Path(encoded.decode()[len(prefix):])
            destination = staging / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
            copied += 1
        if not copied:
            shutil.copytree(ROOT, staging, ignore=shutil.ignore_patterns("target", "__pycache__"))
        return staging

    def test_release_bundle_is_deterministic_complete_and_starts_offline(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            staging = self.staged_bundle(directory)
            self.assertFalse((staging / "target").exists())
            command = [
                "bash",
                str(REPOSITORY / "scripts/package-octet-extension-release.sh"),
                "octet-codemode",
                str(directory / "out"),
                "v0.9.0",
                str(staging),
            ]
            environment = {**os.environ, "SOURCE_DATE_EPOCH": "1700000000"}
            run_checked(command, env=environment)
            archive = directory / "out/octet-codemode-0.9.0.tar.gz"
            first = archive.read_bytes()
            run_checked(command, env=environment)
            self.assertEqual(archive.read_bytes(), first, "archive is not reproducible")

            unpacked = directory / "unpacked"
            with tarfile.open(archive) as bundle:
                names = set(bundle.getnames())
                for required in REQUIRED_BUNDLE_FILES:
                    self.assertIn(f"octet-codemode/{required}", names)
                self.assertTrue(all(member.isfile() or member.isdir() for member in bundle))
                bundle.extractall(unpacked, filter="data")
            extracted = unpacked / "octet-codemode"
            self.assertTrue(os.access(extracted / "bin/codemode", os.X_OK))
            self.assertFalse((extracted / "extension.py").exists())
            self.assertFalse((extracted / "runtime.mjs").exists())

            # Without a prebuilt binary the bundle tells the operator exactly
            # what is missing: Rust. It never falls back to Node or Python.
            missing = subprocess.run(
                [str(extracted / "bin/codemode"), "serve"],
                capture_output=True, text=True, env={"PATH": "", "OCTET_EXTENSION_DIR": str(extracted)},
                timeout=30)
            self.assertEqual(missing.returncode, 1)
            self.assertIn("cargo is unavailable", missing.stderr)
            self.assertNotIn("node", missing.stderr.lower())
            self.assertNotIn("python", missing.stderr.lower())


if __name__ == "__main__":
    unittest.main()
