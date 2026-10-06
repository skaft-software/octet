"""Offline fresh-home setup regressions; every installer/download is mocked.

These fixtures do not install Cua, run a desktop host, change OS permissions,
write native config, or use a provider. Files are confined to disposable homes.
"""
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
import warnings
from unittest.mock import patch
import zipfile

from octet_extension import CancellationToken, CancelledError
from octet_computer_use import desktop_host, driver, entrypoint


class FreshSetupTests(unittest.TestCase):
    def setUp(self):
        home = tempfile.TemporaryDirectory(prefix="octet-new-user-")
        self.addCleanup(home.cleanup)
        self.paths = driver.DriverPaths.for_home(Path(home.name))
        driver._ensure_directories(self.paths)

    def _runtime(self, text):
        self.paths.venv.mkdir(exist_ok=True)
        (self.paths.venv / "sentinel").write_text(text)

    def _install(self, text="new"):
        self._runtime(text)
        return self.paths.venv / "sentinel"

    def test_private_installs_ignore_ambient_pip_sources_and_credentials(self):
        with patch.dict(os.environ, {
            "HOME": str(self.paths.root.parent.parent),
            "PIP_INDEX_URL": "https://user:credential@evil.invalid/simple",
            "PIP_EXTRA_INDEX_URL": "https://private.invalid/simple",
            "PIP_CONFIG_FILE": str(self.paths.root / "user-pip.conf"),
            "PIP_TRUSTED_HOST": "evil.invalid",
            "PIP_FIND_LINKS": str(self.paths.root / "untrusted-wheels"),
            "PIP_TARGET": str(self.paths.root / "outside"),
            "PIP_KEYRING_PROVIDER": "import",
            "NETRC": str(self.paths.root / "user-netrc"),
            "PYTHONPATH": "untrusted-python", "PYTHONHOME": "untrusted-home",
        }, clear=True):
            environment = driver._install_environment()
        self.assertEqual(environment["PIP_INDEX_URL"], "https://pypi.org/simple")
        self.assertEqual(environment["PIP_CONFIG_FILE"], os.devnull)
        self.assertEqual(environment["NETRC"], os.devnull)
        self.assertEqual(environment["PIP_KEYRING_PROVIDER"], "disabled")
        self.assertEqual(environment["PYTHONDONTWRITEBYTECODE"], "1")
        for name in ("PIP_EXTRA_INDEX_URL", "PIP_TRUSTED_HOST", "PIP_FIND_LINKS",
                     "PIP_TARGET", "PYTHONPATH", "PYTHONHOME"):
            self.assertNotIn(name, environment)
        self.assertNotIn("credential", repr(environment))

    def test_driver_pip_command_uses_official_binary_wheels_only(self):
        installed = self.paths.venv / "mock-driver"
        with patch.object(driver, "_run", return_value=SimpleNamespace(returncode=0)) as run, \
                patch.object(driver, "installed_binary", return_value=installed), \
                patch.object(driver, "driver_version", return_value="0.33.0"):
            driver._install_runtime(self.paths, "0.33.0", "cua-driver==0.33.0",
                                    driver._install_environment(), 30,
                                    "mock-python", (3, 12), lambda _: None)
        calls = [call for call in run.call_args_list if "install" in call.args[0]]
        self.assertEqual(len(calls), 1)
        argv = calls[0].args[0]
        self.assertIn("--only-binary=:all:", argv)
        self.assertEqual(argv[argv.index("--index-url") + 1], "https://pypi.org/simple")

    def test_fresh_home_failed_install_removes_only_its_partial_runtime(self):
        unrelated = self.paths.root / "keep"
        unrelated.write_text("user data")
        def fail(*args):
            self._install("partial")
            raise driver.ProvisionError("mock index offline")
        with patch.object(driver, "installed_binary", return_value=None), \
                patch.object(driver, "_driver_interpreter", return_value=("mock-python", (3, 12))), \
                patch.object(driver, "_install_runtime", side_effect=fail):
            with self.assertRaisesRegex(driver.ProvisionError, "index offline"):
                driver.provision(self.paths)
        self.assertFalse(self.paths.venv.exists())
        self.assertFalse((self.paths.root / ".runtime-installing").exists())
        self.assertEqual(unrelated.read_text(), "user data")

    def test_failed_reinstall_and_cancellation_restore_the_previous_runtime(self):
        for error in (driver.ProvisionError("mock install failure"), CancelledError("cancelled")):
            with self.subTest(error=type(error).__name__):
                self._runtime("previous")
                def fail(*args):
                    self._install("partial")
                    raise error
                with patch.object(driver, "installed_binary", return_value=None), \
                        patch.object(driver, "_driver_interpreter", return_value=("mock-python", (3, 12))), \
                        patch.object(driver, "_install_runtime", side_effect=fail):
                    with self.assertRaises(type(error)):
                        driver.provision(self.paths)
                self.assertEqual((self.paths.venv / "sentinel").read_text(), "previous")
                self.assertFalse((self.paths.root / ".runtime-previous").exists())

    def test_retry_recovers_an_interrupted_existing_install_before_reuse(self):
        # Model process loss after the checkpoint/rename and a partial install.
        self._runtime("previous")
        self.paths.venv.rename(self.paths.root / ".runtime-previous")
        (self.paths.root / ".runtime-installing").write_text("existing")
        self._runtime("partial")
        def installed(paths):
            self.assertEqual((paths.venv / "sentinel").read_text(), "previous")
            return paths.venv / "sentinel"
        with patch.object(driver, "installed_binary", side_effect=installed), \
                patch.object(driver, "driver_version", return_value="0.33.0"), \
                patch.object(driver, "_run") as run:
            self.assertEqual(driver.provision(self.paths), self.paths.venv / "sentinel")
        run.assert_not_called()
        self.assertFalse((self.paths.root / ".runtime-installing").exists())

    def test_retry_recovers_an_interrupted_fresh_install_and_then_installs(self):
        (self.paths.root / ".runtime-installing").write_text("fresh")
        self._runtime("partial")
        def install(*args):
            self.assertFalse(self.paths.venv.exists())
            return self._install()
        with patch.object(driver, "installed_binary", return_value=None), \
                patch.object(driver, "_driver_interpreter", return_value=("mock-python", (3, 12))), \
                patch.object(driver, "_install_runtime", side_effect=install):
            driver.provision(self.paths)
        self.assertEqual((self.paths.venv / "sentinel").read_text(), "new")

    def test_interruption_before_backup_rename_does_not_delete_original(self):
        self._runtime("previous")
        (self.paths.root / ".runtime-installing").write_text("existing")
        driver._recover_runtime(self.paths)
        self.assertEqual((self.paths.venv / "sentinel").read_text(), "previous")

    def test_malformed_checkpoint_leaves_the_runtime_untouched(self):
        self._runtime("previous")
        (self.paths.root / ".runtime-installing").write_text("not an owned state")
        with self.assertRaisesRegex(driver.ProvisionError, "checkpoint"):
            driver.provision(self.paths)
        self.assertEqual((self.paths.venv / "sentinel").read_text(), "previous")

    def test_unfinished_runtime_is_not_probed_or_used_before_retry(self):
        (self.paths.root / ".runtime-installing").write_text("fresh")
        self._runtime("partial")
        computer = entrypoint.ComputerUse(SimpleNamespace())
        computer._paths = self.paths
        with patch.object(driver, "_run") as run, \
                patch.object(driver, "active_runtime") as select:
            status = driver.health(self.paths).as_dict()
            self.assertFalse(status["installed"])
            self.assertIn("retry setup", entrypoint._render_status(status))
            with self.assertRaisesRegex(entrypoint.McpError, "unfinished"):
                computer.client()
        run.assert_not_called()
        select.assert_not_called()

    def test_a_second_installer_is_refused_without_stealing_the_lock(self):
        with driver._setup_lock(self.paths):
            with self.assertRaisesRegex(driver.ProvisionError, "already running"):
                with driver._setup_lock(self.paths):
                    self.fail("the second installer obtained the lock")
        with driver._setup_lock(self.paths):
            pass  # Retry works; no stale PID/lock-file deletion is needed.

    @unittest.skipIf(os.name == "nt", "symlink fixtures need Windows privileges")
    def test_redirected_state_paths_are_refused_without_writing_through_them(self):
        outside = self.paths.root.parent.parent / "outside"
        outside.mkdir()
        for name in ("runtime", ".runtime-previous", ".runtime-installing", "install.lock"):
            path = self.paths.root / name
            if path.exists():
                path.unlink()  # The previous probe created the fixture's lock file.
            path.symlink_to(outside, target_is_directory=True)
            try:
                with self.assertRaisesRegex(driver.ProvisionError, "symlink"):
                    driver.provision(self.paths)
                self.assertEqual(list(outside.iterdir()), [])
            finally:
                path.unlink()


class DownloadBoundaryTests(unittest.TestCase):
    def _wheel(self, members):
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w") as archive:
            for name, value in members:
                archive.writestr(name, value)
        return output.getvalue()

    def _pipless(self, home, content):
        paths = driver.DriverPaths.for_home(home)
        driver._ensure_directories(paths)
        site = paths.venv / "lib/python3.12/site-packages"
        name = "cua_driver-0.33.0-py3-none-manylinux_2_31_x86_64.whl"
        url = "https://files.pythonhosted.org/" + name
        index = json.dumps({"urls": [{"filename": name, "url": url,
                     "digests": {"sha256": hashlib.sha256(content).hexdigest()}}]}).encode()
        with patch.object(driver, "_run", return_value=SimpleNamespace(returncode=0)), \
                patch.object(driver, "_venv_site_packages", return_value=site), \
                patch.object(driver, "_glibc_version", return_value=(2, 39)), \
                patch.object(driver.platform, "machine", return_value="x86_64"), \
                patch.object(driver.urllib.request, "urlopen",
                             side_effect=lambda requested, timeout: io.BytesIO(content if requested == url else index)), \
                patch.object(driver, "installed_binary", return_value=site / "cua_driver/bin/cua-driver"), \
                patch.object(driver, "driver_version", return_value="0.33.0"):
            driver._provision_without_pip(paths, "", {}, 30, "mock-python")
        return site

    def test_wheel_links_traversal_duplicates_and_invalid_zip_are_refused(self):
        link = zipfile.ZipInfo("cua_driver/link")
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        valid = ("cua_driver/bin/cua-driver", b"mock driver")
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            duplicate = self._wheel([valid, valid])
        for content in (self._wheel([valid, (link, b"outside")]),
                        self._wheel([valid, ("cua_driver/../../escape", b"bad")]),
                        duplicate, b"not a zip"):
            with self.subTest(content=content[:12]), tempfile.TemporaryDirectory() as home:
                with self.assertRaises(driver.ProvisionError):
                    self._pipless(Path(home), content)
                root = driver.DriverPaths.for_home(Path(home)).root
                self.assertEqual(list(root.glob("*.part")), [])
                self.assertFalse((root / "runtime/lib/python3.12/site-packages/cua_driver/bin/cua-driver").exists())

    def test_valid_wheel_with_directory_entries_installs_offline(self):
        with tempfile.TemporaryDirectory() as home:
            site = self._pipless(Path(home), self._wheel([
                ("cua_driver/", b""), ("cua_driver/bin/", b""),
                ("cua_driver/bin/cua-driver", b"mock driver")]))
            self.assertEqual((site / "cua_driver/bin/cua-driver").read_bytes(), b"mock driver")

    def test_index_cannot_select_an_arbitrary_or_credentialed_artifact_source(self):
        name = "cua_driver-0.33.0-py3-none-manylinux_2_31_x86_64.whl"
        for url in ("https://attacker.example/wheel", "https://user:secret@files.pythonhosted.org/wheel",
                    "http://files.pythonhosted.org/wheel"):
            with self.subTest(url=url), self.assertRaises(driver.ProvisionError):
                driver._select_wheel([{"filename": name, "url": url,
                                     "digests": {"sha256": "a" * 64}}], "x86_64")

    def test_signed_host_download_observes_cancellation_between_chunks(self):
        token = CancellationToken("mock-download")
        class Response(io.BytesIO):
            def read(self, count):
                data = super().read(count)
                token._cancel("fixture")
                return data
        with tempfile.TemporaryDirectory() as home, \
                patch("octet_extension.current_cancellation", return_value=token), \
                patch.object(desktop_host.urllib.request, "urlopen", return_value=Response(b"mock bytes")):
            with self.assertRaises(CancelledError):
                desktop_host._download("https://github.com/trycua/cua/mock", Path(home) / "download", 100)


class ReadinessTests(unittest.TestCase):
    def test_failed_self_check_and_unknown_permissions_are_not_ready(self):
        report = {"installed": True, "runtime": "direct", "platform": "linux",
                  "permissions": "granted", "doctor_ok": False}
        self.assertEqual(entrypoint._readiness(report), (False, "self-check needs attention"))
        report.update(doctor_ok=True, permissions="unknown")
        self.assertFalse(entrypoint._readiness(report)[0])
        report["permissions"] = "granted"
        self.assertEqual(entrypoint._readiness(report), (True, "ready"))

    def test_default_macos_host_permission_pending_never_falls_back_to_ready(self):
        report = {"installed": True, "runtime": "unavailable", "platform": "darwin",
                  "permissions": "unknown", "doctor_ok": False}
        self.assertEqual(entrypoint._readiness(report), (False, "host unavailable"))
        self.assertIn("Grant Accessibility and Screen Recording", entrypoint._render_status(report))

    def test_negative_macos_daemon_sentences_are_not_counted_as_granted(self):
        from octet_computer_use import driver_client
        for response, expected in (
            ({"content": [{"text": "Accessibility: not granted. Screen Recording: not granted."}]}, "denied"),
            ({"content": [{"text": "Accessibility: granted. Screen Recording: granted."}]}, "granted"),
            ({"isError": True, "content": [{"text": "Accessibility: granted. Screen Recording: granted."}]}, "unknown"),
        ):
            client = SimpleNamespace(start=lambda **_: None, close=lambda: None, call=lambda *args: response)
            with self.subTest(expected=expected), tempfile.TemporaryDirectory() as home, \
                    patch.object(driver, "desktop_app_socket", return_value=Path(home)), \
                    patch.object(driver_client, "DriverClient", return_value=client):
                self.assertEqual(driver.desktop_app_permissions(Path("mock-host-driver")), expected)

    def test_refused_permission_payloads_cannot_report_a_usable_session(self):
        client = SimpleNamespace(call=lambda *args: {"isError": True, "structuredContent": {
            "accessibility": True, "screen_recording": True,
            "x11": True, "wayland": True, "wayland_enabled": True}})
        for system in ("Darwin", "Linux"):
            with self.subTest(system=system), patch.object(driver.platform, "system", return_value=system):
                self.assertEqual(driver.permission_state(client)["permissions"], "unknown")

    def test_direct_status_does_not_keep_cli_grants_after_live_probe_failure(self):
        health = driver.Health(True, "0.33.0", "granted", True, "mock CLI ok", runtime="direct")
        computer = entrypoint.ComputerUse(SimpleNamespace())
        with patch.object(driver, "health", return_value=health), \
                patch.object(computer, "client", side_effect=RuntimeError("mock transport failure")):
            report = computer.status()
        self.assertEqual(report["permissions"], "unknown")
        self.assertFalse(entrypoint._readiness(report)[0])
