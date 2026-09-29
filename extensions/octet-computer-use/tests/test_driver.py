"""Driver provisioning tests.

Every test here is offline: ``_run`` is replaced by a recorder, so no venv is
created, no pip index is contacted, and no driver binary is executed.
"""

import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from octet_computer_use import driver as driver_module
from octet_computer_use.driver import DriverPaths, ProvisionError


class _Completed:
    """A stand-in for ``subprocess.CompletedProcess``."""

    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class _Recorder:
    """Records every ``_run`` argv and returns success for each."""

    def __init__(self):
        self.calls = []

    def __call__(self, argv, **kwargs):
        self.calls.append(list(argv))
        return _Completed()

    @property
    def pip_specs(self):
        return [argv[-1] for argv in self.calls if "install" in argv]


class ProvisionVersionTests(unittest.TestCase):
    """An explicit version request must govern reuse of an existing install."""

    def setUp(self):
        self._home = tempfile.TemporaryDirectory()
        self.addCleanup(self._home.cleanup)
        self.paths = DriverPaths.for_home(Path(self._home.name))
        self.existing = Path("/tmp/cua-driver-existing")
        self.rebuilt = Path("/tmp/cua-driver-pinned")
        self.versions = {}
        # Keep these tests independent of the Python that runs them.
        interpreter = patch.object(
            driver_module, "_driver_interpreter", return_value=(sys.executable, (3, 12))
        )
        interpreter.start()
        self.addCleanup(interpreter.stop)

    def _driver_version(self, binary):
        return self.versions.get(binary)

    def _install_script(self, script):
        """Drive provision with a scripted install sequence and a run recorder."""

        recorder = _Recorder()
        probes = iter(script)

        def fake_installed_binary(paths):
            return next(probes, None)

        with patch.object(driver_module, "_run", recorder), patch.object(
            driver_module, "installed_binary", fake_installed_binary
        ), patch.object(driver_module, "driver_version", self._driver_version):
            return recorder, driver_module.provision(self.paths, version=self.request)

    def test_unpinned_request_reuses_the_installed_driver(self):
        self.request = ""
        recorder, binary = self._install_script([self.existing])
        self.assertEqual(binary, self.existing)
        # Reuse is the whole point of an unpinned request: no venv rebuild, no
        # network install.
        self.assertEqual(recorder.calls, [])

    def test_pinned_request_reuses_a_matching_install(self):
        self.request = "0.29.1"
        self.versions[self.existing] = "0.29.1"
        recorder, binary = self._install_script([self.existing])
        self.assertEqual(binary, self.existing)
        self.assertEqual(recorder.calls, [])

    def test_abbreviated_pin_matches_the_installed_release(self):
        # PEP 440 pads release segments, so 0.29 names the same release as the
        # 0.29.0 the driver reports. Reinstalling here would re-download the
        # same wheel on every call.
        self.request = "0.29"
        self.versions[self.existing] = "0.29.0"
        recorder, binary = self._install_script([self.existing])
        self.assertEqual(binary, self.existing)
        self.assertEqual(recorder.calls, [])

    def test_pinned_request_reinstalls_on_version_mismatch(self):
        self.request = "0.29.1"
        self.versions[self.existing] = "0.30.0"
        recorder, binary = self._install_script([self.existing, self.rebuilt])
        self.assertEqual(binary, self.rebuilt)
        # The owned venv is cleared rather than upgraded in place, so the
        # mismatched runtime cannot survive the switch.
        self.assertTrue(
            any(argv[-4:-1] == ["-m", "venv", "--clear"] for argv in recorder.calls),
            recorder.calls,
        )
        self.assertEqual(recorder.pip_specs, ["cua-driver==0.29.1"])

    def test_prefix_version_is_not_treated_as_a_match(self):
        self.request = "0.29.1"
        self.versions[self.existing] = "0.29.10"
        recorder, binary = self._install_script([self.existing, self.rebuilt])
        self.assertEqual(binary, self.rebuilt)
        self.assertEqual(recorder.pip_specs, ["cua-driver==0.29.1"])

    def test_unverifiable_install_does_not_satisfy_a_pin(self):
        # The binary cannot report a version, so the pin is unconfirmed. Reuse
        # would report a runtime that may be any version at all.
        self.request = "0.29.1"
        recorder, binary = self._install_script([self.existing, self.rebuilt])
        self.assertEqual(binary, self.rebuilt)
        self.assertEqual(recorder.pip_specs, ["cua-driver==0.29.1"])

    def test_malformed_pin_is_rejected_instead_of_reusing(self):
        # The pin must be validated before reuse, or a request carrying pip
        # options is reported as a successful no-op.
        with patch.object(driver_module, "_run", _Recorder()) as recorder, patch.object(
            driver_module, "installed_binary", lambda paths: self.existing
        ), patch.object(driver_module, "driver_version", self._driver_version):
            with self.assertRaises(ProvisionError):
                driver_module.provision(self.paths, version="0.29.1 --index-url=http://evil.test")
        self.assertEqual(recorder.calls, [])

    def test_blank_pin_is_rejected(self):
        with patch.object(driver_module, "_run", _Recorder()), patch.object(
            driver_module, "installed_binary", lambda paths: self.existing
        ):
            with self.assertRaises(ProvisionError):
                driver_module.provision(self.paths, version="   ")

    def test_failed_install_surfaces_the_pinned_spec(self):
        recorder = _Recorder()
        recorder.__call__ = lambda argv, **kwargs: (
            _Completed(returncode=1, stderr="no matching distribution")
            if "install" in argv
            else _Completed()
        )
        with patch.object(driver_module, "_run", recorder), patch.object(
            driver_module, "installed_binary", lambda paths: None
        ):
            with self.assertRaises(ProvisionError) as caught:
                driver_module.provision(self.paths, version="9.9.9")
        self.assertIn("cua-driver==9.9.9", str(caught.exception))

    def test_install_without_a_binary_is_reported(self):
        with patch.object(driver_module, "_run", _Recorder()), patch.object(
            driver_module, "installed_binary", lambda paths: None
        ):
            with self.assertRaises(ProvisionError) as caught:
                driver_module.provision(self.paths)
        self.assertIn("no driver executable", str(caught.exception))


class InterpreterPreflightTests(unittest.TestCase):
    """The runtime venv is built only from a Python the driver supports."""

    OLD_HOST = SimpleNamespace(version_info=(3, 9, 6), executable="/usr/bin/python3")

    def setUp(self):
        self._home = tempfile.TemporaryDirectory()
        self.addCleanup(self._home.cleanup)
        self.paths = DriverPaths.for_home(Path(self._home.name))
        self.calls = []

    def _runner(self, probe_versions, install_output=None):
        """Answer interpreter probes from a table; succeed or fail pip install."""

        def run(argv, **kwargs):
            argv = list(argv)
            self.calls.append(argv)
            if argv[1:2] == ["-c"] and "venv" in argv[2]:
                version = probe_versions.get(argv[0])
                return _Completed(returncode=0 if version else 1, stdout=version or "")
            if "install" in argv and install_output is not None:
                return _Completed(returncode=1, stderr=install_output)
            return _Completed()

        return run

    def test_a_compatible_host_interpreter_is_used_without_probing(self):
        host = SimpleNamespace(version_info=(3, 12, 1), executable="/usr/bin/python3.12")
        with patch.object(driver_module, "sys", host), \
                patch.object(driver_module, "_run", self._runner({})):
            self.assertEqual(
                driver_module._driver_interpreter({}), ("/usr/bin/python3.12", (3, 12))
            )
        self.assertEqual(self.calls, [])

    def test_an_old_host_builds_the_venv_with_a_compatible_candidate(self):
        binaries = iter([None, Path("/tmp/cua-driver")])
        with patch.object(driver_module, "sys", self.OLD_HOST), \
                patch.object(driver_module, "_interpreter_candidates",
                             return_value=["/usr/local/bin/python3", "/opt/homebrew/bin/python3"]), \
                patch.object(driver_module, "_run", self._runner({
                    "/usr/local/bin/python3": "3.9", "/opt/homebrew/bin/python3": "3.12"})), \
                patch.object(driver_module, "installed_binary", lambda paths: next(binaries)):
            self.assertEqual(driver_module.provision(self.paths), Path("/tmp/cua-driver"))
        create = [argv for argv in self.calls if argv[1:3] == ["-m", "venv"]]
        self.assertEqual([argv[0] for argv in create], ["/opt/homebrew/bin/python3"])

    def test_no_compatible_interpreter_names_the_minimum_and_leaves_the_venv(self):
        with patch.object(driver_module, "sys", self.OLD_HOST), \
                patch.object(driver_module, "_interpreter_candidates",
                             return_value=["/usr/local/bin/python3"]), \
                patch.object(driver_module, "_run",
                             self._runner({"/usr/local/bin/python3": "3.9"})), \
                patch.object(driver_module, "installed_binary", lambda paths: None):
            with self.assertRaises(ProvisionError) as caught:
                driver_module.provision(self.paths)
        message = str(caught.exception)
        self.assertIn("needs Python 3.10 or newer", message)
        self.assertIn("runs on Python 3.9 (/usr/bin/python3)", message)
        self.assertFalse(any(argv[1:3] == ["-m", "venv"] for argv in self.calls))
        self.assertFalse(self.paths.venv.exists())

    def test_no_matching_distribution_on_a_compatible_interpreter_points_at_the_index(self):
        pip = ("ERROR: Could not find a version that satisfies the requirement cua-driver "
               "(from versions: none)\nERROR: No matching distribution found for cua-driver")
        with patch.object(driver_module, "_driver_interpreter",
                          return_value=("/usr/bin/python3.12", (3, 12))), \
                patch.object(driver_module, "_run", self._runner({}, install_output=pip)), \
                patch.object(driver_module, "installed_binary", lambda paths: None):
            with self.assertRaises(ProvisionError) as caught:
                driver_module.provision(self.paths)
        message = str(caught.exception)
        self.assertIn("No matching distribution found for cua-driver", message)
        self.assertIn("the runtime uses Python 3.12", message)
        self.assertIn("package index", message)

    def test_macos_candidates_include_install_locations_off_the_gui_path(self):
        present = {"/opt/homebrew/bin/python3"}
        with patch.object(driver_module, "sys", self.OLD_HOST), \
                patch.object(driver_module.platform, "system", return_value="Darwin"), \
                patch.object(driver_module.shutil, "which", return_value=None), \
                patch.object(driver_module.os.path, "isfile", side_effect=present.__contains__):
            self.assertEqual(driver_module._interpreter_candidates(), ["/opt/homebrew/bin/python3"])


class VersionComparisonTests(unittest.TestCase):
    def test_release_padding(self):
        self.assertTrue(driver_module._versions_match("0.29", "0.29.0"))
        self.assertTrue(driver_module._versions_match("1.2.3", "1.2.3"))
        self.assertTrue(driver_module._versions_match(" 1.2.3 ", "1.2.3"))
        self.assertFalse(driver_module._versions_match("0.29", "0.30"))

    def test_marked_versions_compare_verbatim(self):
        self.assertTrue(driver_module._versions_match("0.29.1rc1", "0.29.1rc1"))
        self.assertFalse(driver_module._versions_match("0.29.1rc1", "0.29.1"))
        # Not ordered here rather than guessed at, so a pin cannot pass loosely.
        self.assertFalse(driver_module._versions_match("1.0", "1.0.post1"))

    def test_pip_spec_rejects_option_smuggling(self):
        for candidate in ("0.29.1 --index-url=x", "latest", "0.29.1;python<3", " "):
            with self.subTest(candidate=candidate), self.assertRaises(ProvisionError):
                driver_module._pip_spec(candidate)
        self.assertEqual(driver_module._pip_spec("0.29.1"), "cua-driver==0.29.1")
        self.assertEqual(driver_module._pip_spec(""), "cua-driver")


class SitePackagesLayoutTests(unittest.TestCase):
    """A Windows venv has no ``lib/pythonX.Y`` directory."""

    def setUp(self):
        self._home = tempfile.TemporaryDirectory()
        self.addCleanup(self._home.cleanup)
        self.paths = DriverPaths.for_home(Path(self._home.name))

    def test_windows_uses_the_unversioned_lib_layout(self):
        with patch.object(driver_module.platform, "system", return_value="Windows"):
            self.assertEqual(self.paths.site_packages, self.paths.venv / "Lib" / "site-packages")
            # site-packages must sit beside the venv's own Scripts/ interpreter.
            self.assertEqual(
                self.paths.site_packages,
                self.paths.venv_python.parent.parent / "Lib" / "site-packages",
            )

    def test_posix_keeps_the_versioned_lib_layout(self):
        expected = (
            self.paths.venv
            / "lib"
            / f"python{sys.version_info.major}.{sys.version_info.minor}"
            / "site-packages"
        )
        for system in ("Linux", "Darwin"):
            with self.subTest(system=system), patch.object(
                driver_module.platform, "system", return_value=system
            ):
                self.assertEqual(self.paths.site_packages, expected)


if __name__ == "__main__":
    unittest.main()
