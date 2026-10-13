"""Offline coverage for the signed app half of macOS setup."""

import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from octet_computer_use import desktop_host, driver

VERSION = "0.31.0"
PREFIX = f"cua-driver-rs-{VERSION}-darwin-universal"
ARCHIVE_NAME = PREFIX + ".tar.gz"


def archive_bytes(*, unsafe=None):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        member = tarfile.TarInfo(PREFIX + "/CuaDriver.app/Contents/MacOS/cua-driver")
        contents = b"test driver"
        member.size = len(contents)
        member.mode = 0o755
        archive.addfile(member, io.BytesIO(contents))
        if unsafe:
            member = tarfile.TarInfo(unsafe)
            member.type = tarfile.SYMTYPE
            member.linkname = "/tmp/outside"
            archive.addfile(member)
    return output.getvalue()


class DesktopHostTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.paths = driver.DriverPaths.for_home(Path(temporary.name))
        self.archive = archive_bytes()
        self.commands = []
        for context in [
            patch.object(driver, "cursor_host_required", return_value=True),
            patch.object(driver, "desktop_app", return_value=None),
            patch.object(driver, "driver_version", return_value=VERSION),
            patch.object(driver, "_run", side_effect=self.run_command),
            patch.object(desktop_host, "_download", side_effect=self.download),
        ]:
            context.start()
            self.addCleanup(context.stop)

    def run_command(self, argv, **kwargs):
        self.commands.append(argv)
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    def download(self, url, path, limit):
        content = self.archive if url.endswith(".tar.gz") else json.dumps({"assets": [{
            "name": ARCHIVE_NAME, "bytes": len(self.archive),
            "sha256": hashlib.sha256(self.archive).hexdigest(),
        }]}).encode()
        self.assertLessEqual(len(content), limit)
        path.write_bytes(content)
        return hashlib.sha256(content).hexdigest()

    def provision(self):
        return desktop_host.provision(self.paths, Path("/test/wheel-driver"))

    def test_fresh_setup_installs_only_a_version_matched_verified_private_app(self):
        app = self.provision()
        self.assertEqual(app, self.paths.root / "desktop/CuaDriver.app")
        binary = app / "Contents/MacOS/cua-driver"
        self.assertEqual(binary.read_bytes(), b"test driver")
        if os.name != "nt":
            self.assertTrue(binary.stat().st_mode & 0o111)
        self.assertEqual(len(self.commands), 1)
        self.assertEqual(self.commands[0][:4], [
            "/usr/bin/codesign", "--verify", "--deep", "--strict",
        ])
        self.assertIn('YCK386LBJ7', self.commands[0][4])
        self.assertEqual(list(app.parent.iterdir()), [app])

    def test_reuses_a_matching_global_app_without_downloading_or_replacing_it(self):
        app = Path("/Applications/CuaDriver.app")
        with patch.object(driver, "desktop_app", return_value=app), \
                patch.object(driver, "desktop_app_binary", return_value=app / "Contents/MacOS/cua-driver"), \
                patch.object(desktop_host, "_download") as download:
            self.assertEqual(self.provision(), app)
            download.assert_not_called()
        self.assertFalse(self.paths.root.exists())

    def test_existing_host_signature_is_checked_before_executing_its_version_probe(self):
        app = Path("/Applications/CuaDriver.app")
        with patch.object(driver, "desktop_app", return_value=app), \
                patch.object(driver, "desktop_app_binary", return_value=app / "Contents/MacOS/cua-driver"), \
                patch.object(driver, "driver_version", return_value=VERSION) as versions, \
                patch.object(driver, "_run", return_value=SimpleNamespace(returncode=1)):
            with self.assertRaisesRegex(driver.ProvisionError, "signature verification"):
                self.provision()
            versions.assert_called_once_with(Path("/test/wheel-driver"))
        self.assertFalse(self.paths.root.exists())

    def test_private_host_is_launched_by_exact_path_not_a_global_bundle_identifier(self):
        app = self.paths.root / "desktop/CuaDriver.app"
        with patch.object(driver.platform, "system", return_value="Darwin"):
            self.assertTrue(driver.start_desktop_app(app))
        self.assertEqual(self.commands, [["/usr/bin/open", "-n", "-g", "-a", str(app)]])

    def test_other_platforms_and_explicit_direct_mode_do_no_app_work(self):
        with patch.object(driver, "cursor_host_required", return_value=False), \
                patch.object(desktop_host, "_download") as download:
            self.assertIsNone(self.provision())
            download.assert_not_called()
        self.assertFalse(self.paths.root.exists())

    def test_an_explicit_missing_override_is_not_silently_replaced(self):
        with patch.dict("os.environ", {"OCTET_CUA_DESKTOP_APP": "/missing/app"}):
            with self.assertRaisesRegex(driver.ProvisionError, "explicitly selected"):
                self.provision()
        self.assertFalse(self.paths.root.exists())

    def test_a_signature_failure_preserves_the_existing_private_app(self):
        existing = self.paths.root / "desktop/CuaDriver.app"
        existing.mkdir(parents=True)
        (existing / "sentinel").write_text("old app")
        with patch.object(driver, "_run", return_value=SimpleNamespace(returncode=1)):
            with self.assertRaisesRegex(driver.ProvisionError, "signature verification"):
                self.provision()
        self.assertEqual((existing / "sentinel").read_text(), "old app")
        self.assertEqual(list(existing.parent.iterdir()), [existing])

    def test_checksum_mismatch_does_not_install_an_app(self):
        download = self.download
        def corrupt(url, path, limit):
            digest = download(url, path, limit)
            return "0" * 64 if url.endswith(".tar.gz") else digest
        with patch.object(desktop_host, "_download", side_effect=corrupt):
            with self.assertRaisesRegex(driver.ProvisionError, "checksum mismatch"):
                self.provision()
        self.assertFalse((self.paths.root / "desktop/CuaDriver.app").exists())

    def test_traversal_and_link_members_are_refused(self):
        for unsafe in (PREFIX + "/CuaDriver.app/../../escape", PREFIX + "/CuaDriver.app/link"):
            self.archive = archive_bytes(unsafe=unsafe)
            with self.assertRaises(driver.ProvisionError):
                self.provision()
            self.assertFalse((self.paths.root / "desktop/CuaDriver.app").exists())

    def test_a_planted_host_directory_link_is_refused_before_download(self):
        self.paths.root.mkdir(parents=True)
        (self.paths.root / "desktop").symlink_to(self.paths.root.parent, target_is_directory=True)
        with self.assertRaisesRegex(driver.ProvisionError, "must not be a symlink"):
            self.provision()

    def test_discovery_prefers_the_setup_owned_app_but_explicit_overrides_win(self):
        app = self.provision()
        with patch.object(driver, "desktop_app", wraps=DesktopHostTests.original_desktop_app), \
                patch.object(driver.DriverPaths, "for_home", return_value=self.paths), \
                patch.object(driver.platform, "system", return_value="Darwin"), \
                patch.dict("os.environ", {}, clear=True):
            self.assertEqual(driver.desktop_app(), app)
            with patch.dict("os.environ", {"OCTET_CUA_DESKTOP_APP": str(self.paths.root)}):
                self.assertEqual(driver.desktop_app(), self.paths.root)

    original_desktop_app = staticmethod(driver.desktop_app)
