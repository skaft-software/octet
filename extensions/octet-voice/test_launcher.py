"""Provider/device-free launch and source-bundle closure checks."""
import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import launcher


ROOT = Path(__file__).resolve().parent


class LauncherTests(unittest.TestCase):
    def test_platform_interpreter_paths(self):
        home = Path("profile with spaces")
        for platform in ("linux", "darwin"):
            self.assertEqual(launcher.private_python(home, platform),
                             home / ".local/share/octet-voice/venv/bin/python")
        self.assertEqual(launcher.private_python(home, "win32"),
                         home / ".local/share/octet-voice/venv/Scripts/python.exe")

    def test_missing_private_interpreter_has_no_fallback_or_stdout(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(launcher, "user_home", return_value=Path(directory)), \
                patch.object(launcher.os, "execve") as execute, \
                patch.object(launcher.subprocess, "call") as call, \
                contextlib.redirect_stdout(io.StringIO()) as stdout, \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(launcher.main(), 1)
            self.assertEqual(stdout.getvalue(), "")
            self.assertIn("private voice Python is missing", stderr.getvalue())
            execute.assert_not_called()
            call.assert_not_called()

    def test_posix_execs_exact_private_interpreter_and_host_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            python = launcher.private_python(home, "linux")
            python.parent.mkdir(parents=True)
            python.touch()
            with patch.object(launcher, "user_home", return_value=home), \
                    patch.object(launcher.sys, "platform", "linux"), \
                    patch.dict(os.environ, {"OCTET_EXTENSION_DIR": str(home / "bundle with spaces")}), \
                    patch.object(launcher.os, "execve") as execute:
                launcher.main()
                executable, command, environment = execute.call_args.args
                self.assertEqual(executable, str(python))
                self.assertEqual(command, [str(python), "-B", str(home / "bundle with spaces/extension.py")])
                self.assertEqual(environment["PYTHONDONTWRITEBYTECODE"], "1")

    def test_windows_waits_for_supervised_child(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            python = launcher.private_python(home, "win32")
            python.parent.mkdir(parents=True)
            python.touch()
            with patch.object(launcher, "user_home", return_value=home), \
                    patch.object(launcher.sys, "platform", "win32"), \
                    patch.object(launcher.subprocess, "call", return_value=7) as call, \
                    patch.object(launcher.os, "execve") as execute:
                self.assertEqual(launcher.main(), 7)
                self.assertEqual(call.call_args.args[0][0], str(python))
                self.assertEqual(call.call_args.kwargs["env"]["USERPROFILE"], str(home))
                execute.assert_not_called()

    def test_source_only_package_closure_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory) / "relocated bundle"
            result = subprocess.run([sys.executable, str(ROOT / "build-package.py"), str(stage)],
                                    capture_output=True, text=True, check=True)
            self.assertIn(str(stage), result.stdout)
            self.assertEqual({p.name for p in stage.iterdir()}, {
                "extension.toml", "launcher.py", "extension.py", "settings.py",
                "service.py", "audio.py", "portable_audio.py", "realtime.py", "voice_ui.py",
                "README.md", "requirements.in", "requirements.lock", "LICENSE", "SDK-LICENSE",
                "SDK-PROVENANCE.txt", "HOST-INTEGRATION.md", "SECURITY.md", "vendor"})
            self.assertTrue((stage / "vendor/octet_extension/__init__.py").is_file())
            self.assertFalse(list(stage.rglob("__pycache__")))
            self.assertFalse(list(stage.rglob("test_*.py")))
            self.assertIn('command = "launcher.py"', (stage / "extension.toml").read_text())
            again = subprocess.run([sys.executable, str(ROOT / "build-package.py"), str(stage)],
                                   capture_output=True)
            self.assertNotEqual(again.returncode, 0)


if __name__ == "__main__":
    unittest.main()
