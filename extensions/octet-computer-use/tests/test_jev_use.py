"""Offline managed-runtime tests: never install dependencies or touch a desktop."""
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch, Mock

from octet_computer_use import jev_use as j


class JevUseTests(unittest.TestCase):
    def setUp(self):
        j.reset_shutdown_state()
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(j.reset_shutdown_state)
        self.home = Path(self.temp.name).resolve()
        self.binary = self.home / "driver"
        self.binary.touch()

    def prepared(self, ts=False):
        runtime = j._root(self.home) / j.COMMIT
        j._private_dir(runtime)
        j._python(runtime).parent.mkdir(parents=True)
        j._python(runtime).touch()
        (runtime / "verify_setup.py").touch()
        if ts:
            (runtime / "node_modules/tsx").mkdir(parents=True)
        (runtime / "octet-runtime.json").write_text(json.dumps({
            "commit": j.COMMIT, "sha256": j.ARCHIVE_SHA256, "typescript": ts}))
        return runtime

    def summary(self, live=False, ts=False):
        return {"complete": True, "checks": [
            {"language": language, "provider": provider, "outcome": "verified",
             "token": f"jev-guide-{provider}", "observed": {"submitted": f"jev-guide-{provider}"},
             "acted_path": "page_structure", "submit_tool": "browser_click", "visual_statuses": ["skipped"]}
            for language in (["python", "typescript"] if ts else ["python"])
            for provider in (["mock", "live"] if live else ["mock"])]}

    def fake_run(self, summary, code=0):
        def run(argv, **kwargs):
            output = Path(argv[argv.index("--output-dir") + 1])
            output.mkdir()
            (output / "summary.json").write_text(json.dumps(summary))
            return code
        return run

    def test_status_inert_and_not_verified(self):
        with patch.object(j, "_process") as process, patch.object(j, "_download") as download:
            self.assertFalse(j.status(self.home)["prepared"])
            self.assertFalse(j._root(self.home).exists())
            self.prepared()
            self.assertTrue(j.status(self.home)["prepared"])
            self.assertFalse(j.status(self.home)["verified"])
            process.assert_not_called()
            download.assert_not_called()

    def test_run_requires_explicit_setup(self):
        with patch.object(j, "_process") as process:
            result = j.run(self.home, driver_binary=self.binary)
            self.assertEqual(result["error"], "explicit_setup_required")
            process.assert_not_called()

    def test_run_readback_not_exit_code(self):
        self.prepared()
        summary = self.summary()
        with patch.object(j, "_process", side_effect=self.fake_run(summary)):
            result = j.run(self.home, driver_binary=self.binary)
            self.assertTrue(result["verified"])
            self.assertEqual(Path(result["artifacts"]).stat().st_mode & 0o777, 0o700)
        for mutation in ("readback", "outcome", "empty", "complete", "duplicate"):
            summary = self.summary()
            if mutation == "readback":
                summary["checks"][0]["observed"]["submitted"] = "wrong"
            elif mutation == "outcome":
                summary["checks"][0]["outcome"] = "unknown"
            elif mutation == "empty":
                summary["checks"] = []
            elif mutation == "duplicate":
                summary["checks"] *= 2
            else:
                summary["complete"] = False
            with patch.object(j, "_process", side_effect=self.fake_run(summary)):
                result = j.run(self.home, driver_binary=self.binary)
                self.assertFalse(result["verified"], mutation)
                self.assertTrue(Path(result["artifacts"]).exists())

    def test_live_flags_and_key_environment_only(self):
        self.prepared(ts=True)
        with patch.object(j, "_process", side_effect=self.fake_run(self.summary(True, True))) as process:
            result = j.run(self.home, driver_binary=self.binary, live=True, typescript=True, api_key="secret")
            self.assertTrue(result["verified"])
            args, kwargs = process.call_args
            self.assertIn("--live", args[0])
            self.assertIn("--typescript", args[0])
            self.assertNotIn("secret", repr(args))
            self.assertEqual(kwargs["env"]["TYPESAFE_API_KEY"], "secret")
            self.assertNotIn("secret", json.dumps(result))
        with patch.dict(os.environ, {"TYPESAFE_API_KEY": "secret", "AWS_SECRET_ACCESS_KEY": "hidden"}):
            self.assertNotIn("TYPESAFE_API_KEY", j._key_env(self.home, False, None))
            self.assertNotIn("AWS_SECRET_ACCESS_KEY", j._environment())

    def test_unsupported_visual_mode_and_cancel(self):
        self.prepared()
        with patch.object(j, "_process") as process:
            self.assertEqual("invalid_visual_observation", j.run(self.home, driver_binary=self.binary, visual_observation="invalid")["error"])
            token = Mock(cancelled=True)
            self.assertEqual(j.run(self.home, driver_binary=self.binary, cancellation=token)["error"], "cancelled")
            self.assertEqual(j.setup(self.home, cancellation=token)["error"], "cancelled")
            process.assert_not_called()

    def test_visual_fallback_is_not_task_verified(self):
        self.prepared()
        summary = self.summary()
        summary["checks"][0].update(outcome="budget_exhausted", observed={"submitted": None},
                                     submit_tool=None, acted_path=None, visual_statuses=["skipped", "not_installed"])
        with patch.object(j, "_process", side_effect=self.fake_run(summary)):
            result = j.run(self.home, driver_binary=self.binary, visual_fixture=True, expect_visual_status="not_installed")
            self.assertTrue(result["ok"])
            self.assertTrue(result["fallback_verified"])
            self.assertFalse(result["verified"])

    def archive(self, names):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            for name, kind in names:
                member = tarfile.TarInfo(name)
                member.type = kind
                if kind == tarfile.REGTYPE:
                    member.size = 1
                    archive.addfile(member, io.BytesIO(b"x"))
                else:
                    member.linkname = "/etc/passwd"
                    archive.addfile(member)
        return data.getvalue()

    def test_download_digest_bound_and_exact_url(self):
        data = b"archive"
        with patch.object(j.urllib.request, "urlopen", return_value=io.BytesIO(data)) as network:
            with self.assertRaisesRegex(j.RuntimeFailure, "digest"):
                j._download(self.home / "download", None)
            self.assertEqual(network.call_args.args[0], j.ARCHIVE_URL)
        with patch.object(j.urllib.request, "urlopen", return_value=io.BytesIO(data)), patch.object(j, "MAX_ARCHIVE", 2):
            with self.assertRaisesRegex(j.RuntimeFailure, "archive_limit"):
                j._download(self.home / "oversized", None)

    def test_extract_only_recipe_and_refuse_traversal_links(self):
        for index, name in enumerate((j.PREFIX + "../escape", j.PREFIX + "link", j.PREFIX + "normal")):
            archive = self.home / f"source-{index}"
            kind = tarfile.SYMTYPE if index == 1 else tarfile.REGTYPE
            archive.write_bytes(self.archive([(name, kind), ("unrelated/file", tarfile.REGTYPE)]))
            target = self.home / f"extract-{index}"
            target.mkdir()
            if index < 2:
                with self.assertRaises(j.RuntimeFailure):
                    j._extract(archive, target, None)
            else:
                j._extract(archive, target, None)
                self.assertEqual([p.name for p in target.iterdir()], ["normal"])
                self.assertEqual((target / "normal").stat().st_mode & 0o777, 0o600)

    def test_setup_frozen_and_ignore_scripts_no_overwrite(self):
        data = self.archive([(j.PREFIX + "verify_setup.py", tarfile.REGTYPE)])
        def process(argv, **kwargs):
            runtime = kwargs["cwd"]
            j._python(runtime).parent.mkdir(parents=True, exist_ok=True)
            j._python(runtime).touch()
            (runtime / "node_modules/tsx").mkdir(parents=True, exist_ok=True)
            return 0
        with patch.object(j.urllib.request, "urlopen", return_value=io.BytesIO(data)), \
             patch.object(j, "ARCHIVE_SHA256", hashlib.sha256(data).hexdigest()), \
             patch.object(j.shutil, "which", side_effect=lambda n: "/bin/" + n), \
             patch.object(j, "_process", side_effect=process) as runner:
            result = j.setup(self.home, typescript=True)
            self.assertTrue(result["prepared"], result)
            self.assertFalse(result["verified"])
            self.assertIn("--frozen", runner.call_args_list[0].args[0])
            self.assertIn("--ignore-scripts", runner.call_args_list[1].args[0])
            self.assertTrue(j.setup(self.home, typescript=True)["ok"])
            self.assertEqual(runner.call_count, 2)
        # Mismatched existing provenance may not be overwritten.
        self.assertEqual(j.setup(self.home)["error"], "existing_runtime_not_overwritten_use_fresh_home")

    def test_choose_validates_response_and_keeps_diagnostics_private(self):
        self.prepared()
        request = {"schema": "cua.jev_choice_request_v1", "goal": "test", "capture_id": "c", "regions": [], "history": [],
                   "candidates": [{"id": x, "description": x} for x in ("a", "reobserve", "abstain")]}
        response = {"schema": "cua.jev_choice_v1", "selected_id": "a", "model": "mock", "confidence": 1,
                    "probabilities": {"a": 1}}
        def process(argv, **kwargs):
            self.assertIn("--mock", argv)
            self.assertEqual(json.loads(kwargs["stdin"]), request)
            (kwargs["artifacts"] / "stdout.log").write_text(json.dumps(response))
            return 0
        with patch.object(j, "_process", side_effect=process):
            self.assertTrue(j.choose(request, self.home, mock=True)["ok"])
            response["selected_id"] = "invented"
            self.assertFalse(j.choose(request, self.home, mock=True)["ok"])

    def test_process_cancellation_kills_owned_tree(self):
        process = Mock(pid=1234, stdout=io.BytesIO(b"bounded"), stderr=io.BytesIO(b"secret"))
        process.poll.return_value = None
        calls = 0
        def cancelled():
            nonlocal calls
            calls += 1
            return calls > 1
        logs = self.home / "logs"
        logs.mkdir()
        with patch.object(j.subprocess, "Popen", return_value=process) as start, patch.object(j.os, "killpg") as kill, patch.object(j, "_descendants", return_value=[]):
            with self.assertRaisesRegex(j.RuntimeFailure, "cancelled"):
                j._process(["fake"], cwd=self.home, env={}, artifacts=logs, timeout=2, cancellation=cancelled)
            kill.assert_any_call(1234, j.signal.SIGKILL)
            self.assertTrue(start.call_args.kwargs["start_new_session"])
            self.assertNotIn("shell", start.call_args.kwargs)
            self.assertEqual((logs / "stderr.log").stat().st_mode & 0o777, 0o600)


    def test_visual_mode_forwarded_through_adapter(self):
        self.prepared()
        for mode in ("auto", "always", "off"):
            with patch.object(j, "_process", side_effect=self.fake_run(self.summary())) as process:
                self.assertTrue(j.run(self.home, driver_binary=self.binary, visual_observation=mode)["complete"])
                command = process.call_args.args[0]
                self.assertTrue(command[1].endswith("jev_use_verifier.py"))
                self.assertEqual(command[command.index("--visual-observation") + 1], mode)

    def test_add_typescript_without_redownload_or_python_sync(self):
        runtime = self.prepared()
        def install(argv, **kwargs):
            (runtime / "node_modules/tsx").mkdir(parents=True)
            return 0
        with patch.object(j, "_download") as download, patch.object(j, "_process", side_effect=install) as process, \
             patch.object(j.shutil, "which", side_effect=lambda name: "/bin/" + name):
            result = j.setup(self.home, typescript=True)
            self.assertTrue(result["typescript_prepared"], result)
            self.assertEqual(process.call_count, 1)
            self.assertIn("--ignore-scripts", process.call_args.args[0])
            download.assert_not_called()

    def test_dependency_failure_can_be_explicitly_retried(self):
        runtime = j._root(self.home) / j.COMMIT
        j._private_dir(runtime)
        (runtime / "verify_setup.py").touch()
        (runtime / "octet-source.json").write_text(json.dumps({"commit": j.COMMIT, "sha256": j.ARCHIVE_SHA256}))
        def install(argv, **kwargs):
            j._python(runtime).parent.mkdir(parents=True)
            j._python(runtime).touch()
            return 0
        with patch.object(j, "_download") as download, patch.object(j.shutil, "which", return_value="/bin/uv"):
            with patch.object(j, "_process", return_value=1):
                self.assertFalse(j.setup(self.home)["ok"])
            with patch.object(j, "_process", side_effect=install):
                self.assertTrue(j.setup(self.home)["prepared"])
            download.assert_not_called()

    def test_process_timeout_and_output_limit(self):
        for name, data, running in (("timeout", b"", True), ("output_limit", b"x" * 100, False)):
            process = Mock(pid=1234, stdout=io.BytesIO(data), stderr=io.BytesIO(b""), returncode=0)
            process.poll.return_value = None if running else 0
            logs = self.home / name
            logs.mkdir()
            with patch.object(j.subprocess, "Popen", return_value=process), patch.object(j, "_kill_tree") as kill, \
                 patch.object(j, "MAX_LOG", 20):
                with self.assertRaisesRegex(j.RuntimeFailure, name):
                    j._process(["fake"], cwd=self.home, env={}, artifacts=logs, timeout=0)
                kill.assert_called_once_with(process)
                self.assertLessEqual((logs / "stdout.log").stat().st_size, 20)

    def test_windows_suspended_job_lifecycle_and_paths(self):
        process = Mock(pid=1234, stdout=io.BytesIO(b""), stderr=io.BytesIO(b""), returncode=0)
        process.poll.return_value = 0
        fake_os = Mock(wraps=os)
        fake_os.name = "nt"
        close = Mock()
        logs = self.home / "windows"
        logs.mkdir()
        with patch.object(j, "os", fake_os), patch.object(j, "_windows_job", return_value=close) as job, \
             patch.object(j.subprocess, "Popen", return_value=process) as start:
            self.assertEqual(j._python(self.home), self.home / ".venv/Scripts/python.exe")
            self.assertEqual(j._process(["fake.exe"], cwd=self.home, env={}, artifacts=logs, timeout=1), 0)
            self.assertEqual(start.call_args.kwargs["creationflags"], 4)
            job.assert_called_once_with(process)
            close.assert_called_once()

    def test_posix_detached_mcp_descendant_is_terminated(self):
        process = Mock(pid=1234)
        process.poll.return_value = None
        with patch.object(j, "_descendants", return_value=[1235]), patch.object(j.os, "kill") as kill, \
             patch.object(j.os, "killpg") as killpg:
            j._kill_tree(process)
            kill.assert_any_call(1235, j.signal.SIGSTOP)
            kill.assert_any_call(1235, j.signal.SIGKILL)
            killpg.assert_any_call(1234, j.signal.SIGKILL)


    def test_shutdown_signals_only_owned_handles_without_waiting(self):
        process = Mock(pid=1234)
        process.poll.return_value = None
        job = Mock()
        other = Mock(pid=3456)
        with patch.dict(j._ACTIVE_PROCESSES, {1: (process, None), 2: (other, job)}, clear=True), \
             patch.object(j.os, "killpg") as kill, patch.object(j, "_descendants") as scan:
            result = j.cancel_all_processes()
            self.assertEqual(result["signalled"], 2)
            self.assertFalse(result["cleanup_complete"])
            kill.assert_called_once_with(1234, j.signal.SIGINT)
            job.assert_called_once()
            process.wait.assert_not_called()
            other.wait.assert_not_called()
            scan.assert_not_called()
            with self.assertRaisesRegex(j.RuntimeFailure, "cancelled"):
                j._check(None)

    def test_empty_jobs_shutdown_does_not_poison_runtime(self):
        from octet_computer_use import jev_use_jobs as jobs_module
        from types import SimpleNamespace
        from unittest.mock import Mock as MockCls
        extension = SimpleNamespace(cancellation=None, confirm=MockCls(return_value=True))
        manager = jobs_module.Jobs(object(), extension)
        manager.shutdown()
        self.assertFalse(j._STOPPING.is_set())
        # Direct runtime still works after an empty manager shutdown.
        self.assertFalse(j.status(self.home)["prepared"])


if __name__ == "__main__":
    unittest.main()
