"""Hermetic executable coverage for network backends and the bounded peer."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


TESTS_ROOT = Path(__file__).resolve().parent
COMPAT_ROOT = TESTS_ROOT.parent
CONFORMANCE = COMPAT_ROOT / "conformance.py"
REAL_RUNTIME = COMPAT_ROOT / "real_runtime.py"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class NetworkBackendTests(unittest.TestCase):
    def test_unshare_command_preserves_explicit_net_namespace(self) -> None:
        module = load_module("pi_conformance_backend_test", CONFORMANCE)
        child = ["/usr/bin/node", "/opt/bridge.mjs", "--fixture"]
        command = module.NetworkBackend("unshare", "/usr/bin/unshare", "linux_unshare_net").command(
            child,
            cwd=Path("/var/tmp/pi-checkout"),
            env={"HOME": "/var/tmp/pi-home"},
            writable_dir=Path("/var/tmp/pi-home"),
            read_only_paths=(Path("/opt/bridge.mjs"),),
        )
        self.assertEqual(["/usr/bin/unshare", "--net", "--", *child], command)

    def test_bubblewrap_command_is_minimal_and_clears_the_environment(self) -> None:
        module = load_module("pi_conformance_bubblewrap_test", CONFORMANCE)
        child = ["/usr/bin/node", "/opt/bridge.mjs", "--fixture"]
        env = {
            "HOME": "/var/tmp/pi-home",
            "LANG": "C.UTF-8",
            "PATH": "/usr/bin:/bin",
        }
        command = module.NetworkBackend(
            "bubblewrap", "/usr/bin/bwrap", "linux_bubblewrap_unshare_net"
        ).command(
            child,
            cwd=Path("/var/tmp/pi-checkout"),
            env=env,
            writable_dir=Path("/var/tmp/pi-home"),
            read_only_paths=(Path("/opt/bridge.mjs"), Path("/var/tmp/pi-package")),
        )
        self.assertEqual("/usr/bin/bwrap", command[0])
        self.assertIn("--unshare-net", command)
        self.assertIn("--clearenv", command)
        self.assertIn("--dev", command)
        self.assertIn("--proc", command)
        self.assertIn("--tmpfs", command)
        self.assertEqual("/tmp", command[command.index("--tmpfs") + 1])
        self.assertEqual(
            env,
            {
                command[index + 1]: command[index + 2]
                for index, value in enumerate(command)
                if value == "--setenv"
            },
        )
        self.assertIn(
            ["--ro-bind", "/opt/bridge.mjs", "/opt/bridge.mjs"],
            [command[index : index + 3] for index, value in enumerate(command) if value == "--ro-bind"],
        )
        self.assertIn(
            ["--bind", "/var/tmp/pi-home", "/var/tmp/pi-home"],
            [command[index : index + 3] for index, value in enumerate(command) if value == "--bind"],
        )
        self.assertLess(command.index("--bind"), command.index("--ro-bind"))
        self.assertIn(
            ["--dir", "/var/tmp/pi-checkout"],
            [command[index : index + 2] for index, value in enumerate(command) if value == "--dir"],
        )
        self.assertEqual(Path("/var/tmp/pi-checkout").as_posix(), command[command.index("--chdir") + 1])
        self.assertEqual(child, command[-len(child) :])

    def test_backend_selection_has_no_silent_fallback(self) -> None:
        module = load_module("pi_conformance_selection_test", CONFORMANCE)
        with (
            patch.object(module.sys, "platform", "linux"),
            patch.object(module.shutil, "which", return_value="/usr/bin/bwrap"),
            patch.object(module.os, "access", return_value=True),
        ):
            backend = module.select_network_backend("bwrap")
        self.assertEqual("bubblewrap", backend.name)
        self.assertEqual("linux_bubblewrap_unshare_net", backend.evidence_name)

        with (
            patch.object(module.sys, "platform", "linux"),
            patch.object(module.shutil, "which", return_value=None),
        ):
            with self.assertRaisesRegex(module.GateFailure, "selected bubblewrap launcher"):
                module.select_network_backend("bubblewrap")

    def test_missing_selected_launcher_fails_closed_before_loading_source(self) -> None:
        module = load_module("pi_conformance_missing_launcher_test", CONFORMANCE)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            backend = module.NetworkBackend("unshare", "/definitely/missing/unshare", "linux_unshare_net")
            with self.assertRaisesRegex(module.GateFailure, "could not launch"):
                module.load_source(
                    sys.executable,
                    Path(__file__),
                    root,
                    Path(__file__),
                    "f" * 64,
                    {"HOME": str(root)},
                    backend,
                )


class BoundedPeerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.module = load_module("pi_real_runtime_peer_test", REAL_RUNTIME)

    def peer(self, code: str):
        return self.module.JsonRpcPeer([sys.executable, "-c", code], Path.cwd(), {})

    def test_launcher_exit_is_a_hard_failure(self) -> None:
        peer = self.peer("import sys; sys.exit(23)")
        try:
            with self.assertRaisesRegex(self.module.RealRuntimeFailure, "exited"):
                peer.response(1, timeout=2)
        finally:
            peer.close()

    def test_outbound_frames_are_bounded(self) -> None:
        peer = self.peer("import time; time.sleep(5)")
        try:
            with self.assertRaisesRegex(self.module.RealRuntimeFailure, "request exceeds"):
                peer.send({"payload": "x" * self.module.MAX_FRAME_BYTES})
        finally:
            peer.close()

    def test_inbound_frame_is_bounded(self) -> None:
        code = (
            "import sys; sys.stdout.buffer.write(b'x' * %d); sys.stdout.buffer.flush()"
            % (self.module.MAX_FRAME_BYTES + 1)
        )
        peer = self.peer(code)
        try:
            with self.assertRaisesRegex(self.module.RealRuntimeFailure, "frame exceeds"):
                peer.response(1, timeout=5)
        finally:
            peer.close()

    def test_message_count_is_bounded(self) -> None:
        peer = self.peer("import sys; sys.stdout.write('{}\\n' * 513); sys.stdout.flush()")
        try:
            with self.assertRaisesRegex(self.module.RealRuntimeFailure, "message limit"):
                peer.response(1, timeout=5)
        finally:
            peer.close()

    def test_stderr_is_bounded(self) -> None:
        code = (
            "import sys; sys.stderr.buffer.write(b'e' * %d); sys.stderr.buffer.flush()"
            % (self.module.MAX_STDERR_BYTES + 1)
        )
        peer = self.peer(code)
        try:
            with self.assertRaisesRegex(self.module.RealRuntimeFailure, "stderr exceeds"):
                peer.response(1, timeout=5)
        finally:
            peer.close()

    def test_partial_frame_obeys_deadline(self) -> None:
        peer = self.peer("import sys,time; sys.stdout.write('{\\\"id\\\":1'); sys.stdout.flush(); time.sleep(5)")
        try:
            with self.assertRaisesRegex(self.module.RealRuntimeFailure, "timed out"):
                peer.response(1, timeout=0.1)
        finally:
            peer.close()


if __name__ == "__main__":
    unittest.main()
