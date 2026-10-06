"""Driver provisioning tests.

Every test here is offline: ``_run`` is replaced by a recorder, so no venv is
created, no pip index is contacted, and no driver binary is executed.
"""

import hashlib
import io
import json
import subprocess
import sys
import tempfile
import textwrap
import unittest
import zipfile
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
        self.versions[self.existing] = "0.30.4"
        recorder, binary = self._install_script([self.existing])
        self.assertEqual(binary, self.existing)
        # Reuse is the whole point of an unpinned request: no venv rebuild, no
        # network install.
        self.assertEqual(recorder.calls, [])

    def test_pinned_request_reuses_a_matching_install(self):
        self.request = "0.31.1"
        self.versions[self.existing] = "0.31.1"
        recorder, binary = self._install_script([self.existing])
        self.assertEqual(binary, self.existing)
        self.assertEqual(recorder.calls, [])

    def test_abbreviated_pin_matches_the_installed_release(self):
        # PEP 440 pads release segments, so 0.31 names the same release as the
        # 0.31.0 the driver reports. Reinstalling here would re-download the
        # same wheel on every call.
        self.request = "0.31"
        self.versions[self.existing] = "0.31.0"
        recorder, binary = self._install_script([self.existing])
        self.assertEqual(binary, self.existing)
        self.assertEqual(recorder.calls, [])

    def test_pinned_request_reinstalls_on_version_mismatch(self):
        self.request = "0.31.1"
        self.versions[self.existing] = "0.32.0"
        recorder, binary = self._install_script([self.existing, self.rebuilt])
        self.assertEqual(binary, self.rebuilt)
        # The owned venv is cleared rather than upgraded in place, so the
        # mismatched runtime cannot survive the switch.
        self.assertTrue(
            any(argv[-4:-1] == ["-m", "venv", "--clear"] for argv in recorder.calls),
            recorder.calls,
        )
        self.assertEqual(recorder.pip_specs, ["cua-driver==0.31.1"])

    def test_prefix_version_is_not_treated_as_a_match(self):
        self.request = "0.31.1"
        self.versions[self.existing] = "0.31.10"
        recorder, binary = self._install_script([self.existing, self.rebuilt])
        self.assertEqual(binary, self.rebuilt)
        self.assertEqual(recorder.pip_specs, ["cua-driver==0.31.1"])

    def test_unverifiable_install_does_not_satisfy_a_pin(self):
        # The binary cannot report a version, so the pin is unconfirmed. Reuse
        # would report a runtime that may be any version at all.
        self.request = "0.31.1"
        recorder, binary = self._install_script([self.existing, self.rebuilt])
        self.assertEqual(binary, self.rebuilt)
        self.assertEqual(recorder.pip_specs, ["cua-driver==0.31.1"])

    def test_unpinned_request_replaces_an_install_older_than_the_minimum(self):
        # macOS 11 and 12 once resolved the unpinned request to 0.11.0.
        self.request = ""
        self.versions[self.existing] = "0.11.0"
        recorder, binary = self._install_script([self.existing, self.rebuilt])
        self.assertEqual(binary, self.rebuilt)
        self.assertEqual(recorder.pip_specs, ["cua-driver>=0.30.2"])

    def test_pin_below_the_minimum_is_refused(self):
        with patch.object(driver_module, "_run", _Recorder()) as recorder, patch.object(
            driver_module, "installed_binary", lambda paths: None
        ):
            with self.assertRaisesRegex(ProvisionError, "older than 0.30.2"):
                driver_module.provision(self.paths, version="0.11.0")
        self.assertEqual(recorder.calls, [])

    def test_malformed_pin_is_rejected_instead_of_reusing(self):
        # The pin must be validated before reuse, or a request carrying pip
        # options is reported as a successful no-op.
        with patch.object(driver_module, "_run", _Recorder()) as recorder, patch.object(
            driver_module, "installed_binary", lambda paths: self.existing
        ), patch.object(driver_module, "driver_version", self._driver_version):
            with self.assertRaises(ProvisionError):
                driver_module.provision(self.paths, version="0.31.1 --index-url=http://evil.test")
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
                patch.object(driver_module, "_unsupported_platform", return_value=None), \
                patch.object(driver_module, "_run", self._runner({}, install_output=pip)), \
                patch.object(driver_module, "installed_binary", lambda paths: None):
            with self.assertRaises(ProvisionError) as caught:
                driver_module.provision(self.paths)
        message = str(caught.exception)
        self.assertIn("No matching distribution found for cua-driver", message)
        self.assertIn("the runtime uses Python 3.12", message)
        self.assertIn("package index", message)

    def test_direct_wheel_extracts_into_the_venv_interpreters_site_packages(self):
        # The venv can be built by a newer Python than this process, so the
        # lib/pythonX.Y directory comes from the venv, not from sys.version_info.
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as bundle:
            bundle.writestr("cua_driver/__init__.py", "")
        wheel = buffer.getvalue()
        name = "cua_driver-9.9.9-py3-none-manylinux_2_31_x86_64.whl"
        wheel_url = "https://files.pythonhosted.org/" + name
        index = json.dumps({"urls": [{
            "filename": name,
            "url": wheel_url,
            "digests": {"sha256": hashlib.sha256(wheel).hexdigest()},
        }]}).encode()
        responses = {f"{driver_module.PACKAGE_INDEX_JSON}/cua-driver/json": index, wheel_url: wheel}
        site = self.paths.venv / "lib" / "python3.12" / "site-packages"

        def run(argv, **kwargs):
            argv = list(argv)
            self.calls.append(argv)
            if argv[1:3] == ["-m", "venv"]:
                site.mkdir(parents=True, exist_ok=True)
            if "sysconfig" in argv[-1]:
                return _Completed(stdout=str(site))
            return _Completed()

        with patch.object(driver_module, "sys", self.OLD_HOST), \
                patch.object(driver_module, "_run", run), \
                patch.object(driver_module, "_glibc_version", return_value=(2, 39)), \
                patch.object(driver_module.platform, "machine", return_value="x86_64"), \
                patch.object(driver_module.urllib.request, "urlopen",
                             side_effect=lambda url, timeout: io.BytesIO(responses[url])), \
                patch.object(driver_module, "installed_binary",
                             return_value=Path("/tmp/cua-driver")):
            driver_module._ensure_directories(self.paths)
            driver_module._provision_without_pip(
                self.paths, "", {}, 60, "/usr/bin/python3.12")
        self.assertTrue((site / "cua_driver" / "__init__.py").is_file())
        self.assertFalse((self.paths.venv / "lib" / "python3.9").exists())

    def test_venv_site_packages_outside_the_venv_is_refused(self):
        with patch.object(driver_module, "_run",
                          return_value=_Completed(stdout="/usr/lib/python3/dist-packages")):
            with self.assertRaises(ProvisionError):
                driver_module._venv_site_packages(self.paths, {})

    def test_macos_candidates_include_install_locations_off_the_gui_path(self):
        present = {"/opt/homebrew/bin/python3"}
        with patch.object(driver_module, "sys", self.OLD_HOST), \
                patch.object(driver_module.platform, "system", return_value="Darwin"), \
                patch.object(driver_module.shutil, "which", return_value=None), \
                patch.object(driver_module.os.path, "isfile", side_effect=present.__contains__):
            self.assertEqual(driver_module._interpreter_candidates(), ["/opt/homebrew/bin/python3"])


    def test_a_fresh_install_reports_each_phase_as_it_starts(self):
        binaries = iter([None, Path("/tmp/cua-driver")])
        steps = []
        with patch.object(driver_module, "_driver_interpreter",
                          return_value=("/usr/bin/python3.12", (3, 12))), \
                patch.object(driver_module, "_run", self._runner({})), \
                patch.object(driver_module, "installed_binary", lambda paths: next(binaries)), \
                patch.object(driver_module, "driver_version", return_value="0.31.0"):
            driver_module.provision(self.paths, progress=steps.append)
        self.assertEqual(steps, [
            "Checking the installed cua-driver…",
            "Finding Python 3.10 or newer…",
            "Using Python 3.12 (/usr/bin/python3.12)",
            "Creating the driver's private Python environment…",
            "Downloading and installing cua-driver>=0.30.2 (this can take a minute)…",
            "Installed cua-driver 0.31.0",
        ])

    def test_reusing_an_install_says_so(self):
        steps = []
        with patch.object(driver_module, "installed_binary", lambda paths: Path("/tmp/cua-driver")), \
                patch.object(driver_module, "driver_version", return_value="0.31.0"):
            driver_module.provision(self.paths, progress=steps.append)
        self.assertEqual(steps, ["Checking the installed cua-driver…",
                                 "cua-driver 0.31.0 is already installed"])


class PlatformSupportTests(unittest.TestCase):
    """A no-match install names the platform instead of blaming the index."""

    def _describe(self, system, machine, *, mac="", libc=("glibc", "2.39"), bits=64):
        maxsize = 2**63 - 1 if bits == 64 else 2**31 - 1
        with patch.object(driver_module.platform, "system", return_value=system), \
                patch.object(driver_module.platform, "machine", return_value=machine), \
                patch.object(driver_module.platform, "mac_ver", return_value=(mac, ("", "", ""), "")), \
                patch.object(driver_module.platform, "libc_ver", return_value=libc), \
                patch.object(driver_module, "sys", SimpleNamespace(maxsize=maxsize)):
            return driver_module._unsupported_platform()

    def test_supported_platforms_are_not_flagged(self):
        self.assertIsNone(self._describe("Darwin", "arm64", mac="13.6"))
        self.assertIsNone(self._describe("Darwin", "x86_64", mac="15.1"))
        self.assertIsNone(self._describe("Linux", "x86_64", libc=("glibc", "2.31")))
        self.assertIsNone(self._describe("Linux", "aarch64"))
        self.assertIsNone(self._describe("Windows", "AMD64"))
        self.assertIsNone(self._describe("Windows", "ARM64"))

    def test_unsupported_platforms_are_described(self):
        self.assertEqual(self._describe("Darwin", "x86_64", mac="12.7.6"), "macOS 12.7.6 on x86_64")
        self.assertIn("glibc 2.28", self._describe("Linux", "x86_64", libc=("glibc", "2.28")))
        self.assertIn("without glibc", self._describe("Linux", "x86_64", libc=("", "")))
        self.assertIn("armv7l", self._describe("Linux", "armv7l", bits=32))
        self.assertIn("32-bit", self._describe("Windows", "x86", bits=32))
        self.assertIn("FreeBSD", self._describe("FreeBSD", "amd64"))

    def test_no_match_on_an_unsupported_platform_names_what_is_supported(self):
        with tempfile.TemporaryDirectory() as home:
            paths = DriverPaths.for_home(Path(home))
            pip = "ERROR: No matching distribution found for cua-driver>=0.30.2"

            def run(argv, **kwargs):
                return _Completed(returncode=1, stderr=pip) if "install" in argv else _Completed()

            with patch.object(driver_module, "_driver_interpreter",
                              return_value=("/usr/local/bin/python3", (3, 12))), \
                    patch.object(driver_module, "_unsupported_platform",
                                 return_value="macOS 12.7.6 on x86_64"), \
                    patch.object(driver_module, "_run", run), \
                    patch.object(driver_module, "installed_binary", lambda paths: None):
                with self.assertRaises(ProvisionError) as caught:
                    driver_module.provision(paths)
        message = str(caught.exception)
        self.assertIn("not published for this system (macOS 12.7.6 on x86_64)", message)
        self.assertIn("macOS 13 or newer", message)

    def test_minimum_comparison(self):
        for version in ("0.30.2", "0.30.4", "0.31", "1.0.0", "0.31.0rc1"):
            self.assertTrue(driver_module.meets_minimum(version), version)
        for version in ("0.30.1", "0.29.9", "0.11.0", "", "unknown"):
            self.assertFalse(driver_module.meets_minimum(version), version)


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
        self.assertEqual(driver_module._pip_spec("0.31.1"), "cua-driver==0.31.1")
        self.assertEqual(driver_module._pip_spec(""), "cua-driver>=0.30.2")


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


class ProbeIsolationTests(unittest.TestCase):
    """Probes never share the extension's protocol stdin with a child."""

    def test_probes_never_inherit_stdin(self):
        calls = []

        def run(argv, **kwargs):
            calls.append(kwargs)
            return _Completed()

        with patch.object(driver_module.subprocess, "run", run):
            driver_module._run(["cua-driver", "--version"])
        self.assertIs(calls[0]["stdin"], subprocess.DEVNULL)


class HealthFailureTests(unittest.TestCase):
    """A probe that cannot run is a failing self-check, never an exception."""

    def setUp(self):
        self._home = tempfile.TemporaryDirectory()
        self.addCleanup(self._home.cleanup)
        self.paths = DriverPaths.for_home(Path(self._home.name))

    def test_a_probe_that_cannot_run_is_reported_with_its_reason(self):
        self.paths.venv_python.parent.mkdir(parents=True)
        self.paths.venv_python.write_text("")
        failure = ProvisionError("command timed out after 60s: python -c")
        with patch.object(driver_module, "active_runtime", return_value="direct"), \
             patch.object(driver_module, "installed_binary", side_effect=failure):
            health = driver_module.health(self.paths)
        self.assertTrue(health.installed)
        self.assertFalse(health.doctor_ok)
        self.assertEqual(health.permissions, "unknown")
        self.assertEqual(health.runtime, "direct")
        self.assertIn("could not run", health.detail)
        self.assertIn("timed out after 60s", health.detail)

    def test_a_failing_doctor_probe_is_reported_too(self):
        binary = Path(self._home.name) / "cua-driver"

        def run(argv, **kwargs):
            if argv[1:] == ["doctor", "--json"]:
                raise ProvisionError("failed to run cua-driver: access denied")
            return _Completed()

        with patch.object(driver_module, "active_runtime", return_value="direct"), \
             patch.object(driver_module, "installed_binary", return_value=binary), \
             patch.object(driver_module, "driver_version", return_value="0.30.3"), \
             patch.object(driver_module, "_run", run):
            health = driver_module.health(self.paths)
        self.assertFalse(health.doctor_ok)
        self.assertIn("access denied", health.detail)

    def test_a_runtime_that_cannot_be_probed_is_unavailable(self):
        failure = ProvisionError("command timed out after 20s: open")
        with patch.object(driver_module, "active_runtime", side_effect=failure):
            health = driver_module.health(self.paths)
        self.assertFalse(health.installed)
        self.assertEqual(health.runtime, "unavailable")
        self.assertIn("timed out after 20s", health.detail)


@unittest.skipUnless(sys.platform == "win32", "Windows pipe semantics")
class HostedStdinTests(unittest.TestCase):
    """A probe must finish while the protocol reader is blocked on stdin.

    Inside octet the extension reads JSON-RPC from a synchronous pipe on a
    reader thread. This reproduces that: the parent below blocks a thread in a
    read of its stdin, which this test keeps open and never writes to, and then
    runs a probe.
    """

    ROOT = Path(__file__).resolve().parents[1]

    def _hosted(self, probe):
        script = textwrap.dedent(f"""
            import os, subprocess, sys, threading, time
            sys.path[:0] = [{str(self.ROOT)!r}, {str(self.ROOT / "vendor")!r}]
            from octet_computer_use import driver
            threading.Thread(target=sys.stdin.buffer.readline, daemon=True).start()
            time.sleep(0.5)
            started = time.monotonic()
            try:
                {probe}
                outcome = "finished"
            except (subprocess.TimeoutExpired, driver.ProvisionError):
                outcome = "stalled"
            print(outcome, round(time.monotonic() - started, 2), flush=True)
            # The reader thread still holds stdin; skip interpreter shutdown.
            os._exit(0)
        """)
        process = subprocess.Popen(
            [sys.executable, "-c", script],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        try:
            # Wait without closing stdin: closing it would end the blocked read.
            returncode = process.wait(timeout=90)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise
        finally:
            process.stdin.close()
        output = process.stdout.read().strip()
        errors = process.stderr.read().strip()
        process.stdout.close()
        process.stderr.close()
        self.assertEqual(returncode, 0, errors)
        return output

    def test_a_probe_finishes_while_stdin_is_being_read(self):
        output = self._hosted('driver._run([sys.executable, "-c", "pass"], timeout=20)')
        self.assertTrue(output.startswith("finished"), output)

    def test_an_inheriting_child_is_the_stall_this_prevents(self):
        # Diagnostic control: the same child with stdin inherited.
        output = self._hosted(
            'subprocess.run([sys.executable, "-c", "pass"], stdout=subprocess.PIPE, '
            'stderr=subprocess.PIPE, timeout=8)'
        )
        if not output.startswith("stalled"):
            self.skipTest(f"an inheriting child did not stall here ({output})")


if __name__ == "__main__":
    unittest.main()
