"""Exercise release packaging without modifying Git or executing extensions."""

import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import tomllib
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "package-octet-extension-release.sh"


class ExtensionBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "fixture-extension"
        self.source.mkdir()
        self.output = self.root / "output"
        self.entrypoint = self.source / "extension.py"
        self.entrypoint.write_text("#!/usr/bin/env python3\nraise RuntimeError('must not execute')\n")
        self.entrypoint.chmod(0o755)
        self.archive = self.output / "fixture-extension-0.8.0.tar.gz"

    def manifest(self, api="0.4", requirement="=0.8.0", command="extension.py"):
        (self.source / "extension.toml").write_text(
            'name = "fixture-extension"\nversion = "1.2.3"\n'
            f'api_version = "{api}"\nrequires_octet = "{requirement}"\n'
            f'[entrypoint]\ncommand = "{command}"\n'
            '[contributes]\ntools = []\n'
        )

    def package(self):
        return subprocess.run(
            ["bash", str(SCRIPT), "fixture-extension", str(self.output), "v0.8.0", str(self.source)],
            env={**os.environ, "SOURCE_DATE_EPOCH": "1700000000"},
            capture_output=True, text=True, timeout=30,
        )

    def test_legacy_and_current_api_bundle_bytes_are_deterministic(self):
        for api in ("0.2", "0.3", "0.4"):
            with self.subTest(api=api):
                self.manifest(api)
                result = self.package()
                self.assertEqual(result.returncode, 0, result.stderr)
                first = self.archive.read_bytes()
                self.assertEqual(self.package().returncode, 0)
                self.assertEqual(first, self.archive.read_bytes())
                with tarfile.open(self.archive) as archive:
                    self.assertEqual(archive.getnames(), [
                        "fixture-extension", "fixture-extension/extension.py",
                        "fixture-extension/extension.toml",
                    ])
                    self.assertEqual(archive.getmember("fixture-extension/extension.py").mode, 0o755)
                    self.assertEqual(archive.extractfile("fixture-extension/extension.toml").read(),
                                     (self.source / "extension.toml").read_bytes())
                self.archive.unlink()

    def test_unknown_and_unpackaged_only_apis_are_refused(self):
        for api in ("0.1", "0.3.0", "0.5", "", "latest"):
            with self.subTest(api=api):
                self.manifest(api)
                result = self.package()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("api_version", result.stderr)
                self.assertFalse(self.archive.exists())

    def test_current_api_still_requires_exact_host_version(self):
        for requirement in ("=0.7.6", ">=0.8.0", "0.8.0", "*"):
            with self.subTest(requirement=requirement):
                self.manifest(requirement=requirement)
                result = self.package()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("requires_octet", result.stderr)
                self.assertFalse(self.archive.exists())

    def test_current_api_does_not_relax_entrypoint_or_symlink_checks(self):
        self.manifest()
        self.entrypoint.chmod(0o644)
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("executable", result.stderr)
        self.assertFalse(self.archive.exists())
        self.entrypoint.chmod(0o755)
        (self.source / "linked.py").symlink_to(self.entrypoint)
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("links or special files", result.stderr)
        self.assertFalse(self.archive.exists())

    def test_directory_links_and_linked_command_ancestors_are_refused(self):
        outside = self.root / "outside"
        (outside / "bin").mkdir(parents=True)
        executable = outside / "bin" / "extension.py"
        executable.write_bytes(self.entrypoint.read_bytes())
        executable.chmod(0o755)
        linked = self.source / "linked"
        linked.symlink_to(outside, target_is_directory=True)
        for api in ("0.2", "0.3", "0.4"):
            for command in ("extension.py", "linked/bin/extension.py"):
                with self.subTest(api=api, command=command):
                    self.manifest(api=api, command=command)
                    result = self.package()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("links or special files", result.stderr)
                    self.assertFalse(self.archive.exists())

    def test_filtered_local_entrypoint_cannot_produce_an_incomplete_archive(self):
        cache = self.source / "__pycache__"
        cache.mkdir()
        executable = cache / "extension.py"
        executable.write_bytes(self.entrypoint.read_bytes())
        executable.chmod(0o755)
        self.manifest(command="__pycache__/extension.py")
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("entrypoint.command is missing from the bundle", result.stderr)
        self.assertFalse(self.archive.exists())

    def test_nested_regular_entrypoint_is_present_and_executable(self):
        directory = self.source / "bin"
        directory.mkdir()
        self.entrypoint.rename(directory / "extension.py")
        self.manifest(command="bin/extension.py")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.archive) as archive:
            member = archive.getmember("fixture-extension/bin/extension.py")
            self.assertTrue(member.isfile())
            self.assertEqual(member.mode, 0o755)


class PiCompatBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repository = SCRIPT.parent.parent
        self.source = self.root / "extensions/octet-pi-compat"
        self.source.mkdir(parents=True)
        upstream = self.repository / "extensions/octet-pi-compat"
        # Copy tracked inputs only; never copy the workstation's npm/cache tree.
        tracked = subprocess.run(
            ["git", "-C", str(self.repository), "ls-files", "-z", "--", "extensions/octet-pi-compat"],
            check=True, capture_output=True,
        ).stdout.split(b"\0")
        self.tracked = [os.fsdecode(encoded) for encoded in tracked if encoded]
        for encoded in tracked:
            if encoded:
                original = self.repository / os.fsdecode(encoded)
                target = self.source / original.relative_to(upstream)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(original, target)
        self.output = self.root / "output"
        self.archive = self.output / "octet-pi-compat-0.9.0.tar.gz"
        self.env = {**os.environ, "SOURCE_DATE_EPOCH": "1700000000"}

    def package(self):
        return subprocess.run(
            ["bash", str(SCRIPT), "octet-pi-compat", str(self.output), "v0.9.0", str(self.source)],
            env=self.env, capture_output=True, text=True, timeout=90,
        )

    def fake_npm(self, body):
        directory = self.root / "bin"
        directory.mkdir()
        npm = directory / "npm"
        npm.write_text("#!/usr/bin/env python3\n" + body)
        npm.chmod(0o755)
        self.env["PATH"] = str(directory) + os.pathsep + os.environ["PATH"]

    def inventory(self, archive=None):
        workflow = (self.repository / ".github/workflows/release-serve.yml").read_text()
        gate = textwrap.dedent(workflow.split(
            'python3 - "$extension" "$archive" <<\'PY\'\n', 1,
        )[1].split("          PY\n", 1)[0])
        tools = self.root / "inventory-tools"
        tools.mkdir(exist_ok=True)
        git = tools / "git"
        git.write_text(
            "#!/usr/bin/env python3\nimport sys\n"
            "assert sys.argv[1:] == ['ls-files', '--', 'extensions/octet-pi-compat']\n"
            f"print({chr(10).join(self.tracked)!r})\n"
        )
        git.chmod(0o755)
        return subprocess.run(
            [sys.executable, "-", "octet-pi-compat", str(archive or self.archive)],
            input=gate, cwd=self.root,
            env={**self.env, "PATH": str(tools) + os.pathsep + self.env["PATH"]},
            capture_output=True, text=True, timeout=90,
        )

    def test_manifest_joins_official_catalog_with_exact_host_pin(self):
        catalog = (self.repository / "extensions/release-catalog.txt").read_text().splitlines()
        self.assertEqual(catalog.count("octet-pi-compat"), 1)
        manifest = tomllib.loads((self.source / "extension.toml").read_text())
        self.assertEqual(manifest["version"], "0.9.0")
        self.assertEqual(manifest["api_version"], "0.4")
        self.assertEqual(manifest["requires_octet"], "=0.9.0")

    def test_configured_factories_and_missing_lockfile_are_refused_before_npm(self):
        self.fake_npm("raise RuntimeError('npm must not run')\n")
        bridge = self.source / "bridge.json"
        original = bridge.read_bytes()
        for config in ({"extensions": ["/reviewed/factory.ts"]},
                       {"extensions": [], "pi_runtime": "installed"}):
            bridge.write_text(json.dumps(config))
            result = self.package()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("empty reviewed-factory configuration", result.stderr)
            self.assertFalse(self.archive.exists())
        bridge.write_bytes(original)
        (self.source / "package-lock.json").unlink()
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("regular package-lock.json", result.stderr)
        self.assertFalse(self.archive.exists())

    def test_dependencies_are_staged_without_scripts_or_bin_links_and_links_still_fail(self):
        self.fake_npm(
            "import pathlib, sys\n"
            f"assert pathlib.Path.cwd() != pathlib.Path({str(self.source)!r})\n"
            "assert sys.argv[1:] == ['ci', '--omit=dev', '--ignore-scripts', '--no-audit', '--no-fund', '--bin-links=false']\n"
            "assert pathlib.Path('package-lock.json').is_file()\n"
            "modules = pathlib.Path('node_modules'); modules.mkdir()\n"
            "(modules / 'link').symlink_to('/outside')\n"
        )
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("links or special files", result.stderr)
        self.assertFalse(self.archive.exists())
        self.assertFalse((self.source / "node_modules").exists())

    def test_failed_dependency_install_does_not_publish_an_archive(self):
        self.fake_npm("import sys\nsys.exit(23)\n")
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.archive.exists())

    def test_inventory_rejects_untracked_files_missing_inputs_and_changed_dependencies(self):
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = self.inventory()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for extra, remove, change in (
            ("octet-pi-compat/untracked.txt", None, None),
            ("octet-pi-compat/node_modules/unlocked/package.json", None, None),
            ("octet-pi-compat/node_modules/jiti/untracked.js", None, None),
            (None, "octet-pi-compat/runner.mjs", None),
            (None, "octet-pi-compat/node_modules/jiti/package.json", None),
            (None, None, "octet-pi-compat/node_modules/jiti/package.json"),
            (None, None, "octet-pi-compat/node_modules/@earendil-works/pi-tui/LICENSE"),
        ):
            with self.subTest(extra=extra, remove=remove, change=change):
                mutated = self.output / "mutated.tar.gz"
                with tarfile.open(self.archive) as original, tarfile.open(mutated, "w:gz") as output:
                    for member in original:
                        if member.name == remove:
                            continue
                        contents = original.extractfile(member).read() if member.isfile() else None
                        if member.name == change:
                            contents = b"changed dependency"
                            member.size = len(contents)
                        output.addfile(member, io.BytesIO(contents) if contents is not None else None)
                    if extra:
                        member = tarfile.TarInfo(extra)
                        member.size = 5
                        output.addfile(member, io.BytesIO(b"extra"))
                result = self.inventory(mutated)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("content mismatch" if change else "inclusion mismatch", result.stderr)

    def test_real_bundle_is_deterministic_complete_inert_and_inspects_offline(self):
        # Poison ambient dependencies and caches: packaging must rebuild from the lock.
        modules = self.source / "node_modules"
        modules.mkdir()
        (modules / "ambient").symlink_to("/outside")
        cache = self.source / ".npm-cache"
        cache.mkdir()
        (cache / "ambient-cache").write_text("must not ship")
        marker = self.root / "lifecycle-ran"
        package_path = self.source / "package.json"
        package = json.loads(package_path.read_text())
        package["scripts"]["preinstall"] = f"touch {marker}"
        package_path.write_text(json.dumps(package))
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        first = self.archive.read_bytes()
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(first, self.archive.read_bytes())
        result = self.inventory()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(marker.exists())
        lock = json.loads((self.source / "package-lock.json").read_text())
        unpacked = self.root / "unpacked"
        with tarfile.open(self.archive) as bundle:
            names = set(bundle.getnames())
            for name in ("runner.mjs", "configure.mjs", "setup.mjs", "bridge.json", "LICENSE.pi", "package-lock.json"):
                self.assertIn(f"octet-pi-compat/{name}", names)
            for path, pinned in lock["packages"].items():
                if not path:
                    continue
                member = f"octet-pi-compat/{path}/package.json"
                metadata = json.load(bundle.extractfile(member))
                self.assertEqual(metadata["version"], pinned["version"])
                self.assertTrue(any(name.startswith(f"octet-pi-compat/{path}/") and
                                    "license" in name.lower() for name in names))
            self.assertFalse(any("/.bin/" in name or "/.npm-cache/" in name or
                                 name.endswith("/ambient") for name in names))
            self.assertTrue(all(member.isfile() or member.isdir() for member in bundle))
            self.assertLess(len(names), 4096, "host installer entry-count limit")
            bundle.extractall(unpacked, filter="data")
        installed = unpacked / "octet-pi-compat"
        original_bridge = (self.source / "bridge.json").read_bytes()
        result = subprocess.run(
            ["node", str(installed / "runner.mjs"), "--inspect"], cwd=self.root,
            capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(value == [] for value in json.loads(result.stdout)["result"].values()))
        # An unchanged bundled TS fixture proves jiti and Pi module resolution work offline.
        result = subprocess.run(
            ["node", str(installed / "runner.mjs"), "--inspect",
             str(installed / "test/fixtures/core.ts")], cwd=self.root,
            capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("core", [tool["name"] for tool in json.loads(result.stdout)["result"]["tools"]])
        self.assertEqual((installed / "bridge.json").read_bytes(), original_bridge)
        self.assertTrue((modules / "ambient").is_symlink())
        self.assertEqual((cache / "ambient-cache").read_text(), "must not ship")


if __name__ == "__main__":
    unittest.main()
