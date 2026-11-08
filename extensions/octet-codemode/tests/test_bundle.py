"""Release wiring: the bundle is complete, deterministic and Node/Python-free."""
import asyncio
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest

from support import REPOSITORY, ROOT, run_checked, runner_binary

TARGET = os.environ.get("CODEMODE_RELEASE_TARGET") or next(
    line.removeprefix("host: ") for line in
    subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: ")
)
SUFFIX = ".exe" if "-windows-" in TARGET else ""

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
            manifest = tomllib.loads((REPOSITORY / "Cargo.toml").read_text())
            version = manifest["workspace"]["package"]["version"]
            self.assertFalse((staging / "target").exists())
            native = staging / "target" / TARGET / "release" / f"octet-codemode{SUFFIX}"
            native.parent.mkdir(parents=True)
            shutil.copy2(runner_binary(), native)
            command = [
                "bash",
                str(REPOSITORY / "scripts/package-octet-extension-release.sh"),
                "octet-codemode",
                str(directory / "out"),
                f"v{version}",
                str(staging),
                TARGET,
            ]
            environment = {**os.environ, "SOURCE_DATE_EPOCH": "1700000000"}
            run_checked(command, env=environment)
            archive = directory / f"out/octet-codemode-{version}-{TARGET}.tar.gz"
            first = archive.read_bytes()
            run_checked(command, env=environment)
            self.assertEqual(archive.read_bytes(), first, "archive is not reproducible")

            unpacked = directory / "unpacked"
            with tarfile.open(archive) as bundle:
                names = set(bundle.getnames())
                for required in REQUIRED_BUNDLE_FILES:
                    if required == "bin/codemode":
                        required += SUFFIX
                    self.assertIn(f"octet-codemode/{required}", names)
                self.assertFalse(any("/target/" in name for name in names))
                self.assertTrue(all(member.isfile() or member.isdir() for member in bundle))
                bundle.extractall(unpacked, filter="data")
            extracted = unpacked / "octet-codemode"
            executable = extracted / f"bin/codemode{SUFFIX}"
            self.assertTrue(os.access(executable, os.X_OK))
            self.assertEqual(executable.read_bytes(), native.read_bytes())
            installed_manifest = tomllib.loads((extracted / "extension.toml").read_text())
            self.assertEqual(installed_manifest["entrypoint"], {"command": f"bin/codemode{SUFFIX}", "args": ["serve"]})
            self.assertFalse((extracted / "extension.py").exists())
            self.assertFalse((extracted / "runtime.mjs").exists())

            # Exercise the extracted native entrypoint and its embedded WASM,
            # outside the checkout, with no cargo, Node or Python on PATH.
            from test_adapter import Broker, text
            scratch = directory / "scratch"
            scratch.mkdir()
            async def probe():
                broker = Broker("wasi", scratch, executable,
                                command=[str(executable), "serve"], cwd=directory,
                                env={"OCTET_EXTENSION_DIR": str(extracted)})
                try:
                    await broker.start()
                    result = await broker.run("return 1 + 2;")
                    self.assertFalse(result.get("is_error", False), result)
                    output = text(result)
                    self.assertTrue(output.startswith("Script completed\n"), output)
                    summary, separator, value = output.partition("\nOutput:\n")
                    self.assertIn("Wall time ", summary)
                    self.assertEqual(separator, "\nOutput:\n", output)
                    self.assertEqual(value, "3", "the installed WASM runner must evaluate the script")
                finally:
                    await broker.close()
            asyncio.run(probe())

            native.unlink()
            missing = subprocess.run(command, capture_output=True, text=True, env=environment)
            self.assertNotEqual(missing.returncode, 0)
            self.assertIn("missing regular native codemode executable", missing.stderr)


if __name__ == "__main__":
    unittest.main()
