"""Executable fixture coverage for the Pi 0.84.4 conformance ledger."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

try:
    from .helpers import BridgeProcess, NODE, v03_contract
except ImportError:  # unittest discovery imports this file as a top-level module.
    from helpers import BridgeProcess, NODE, v03_contract


COMPAT_ROOT = Path(__file__).resolve().parents[1]
PROFILE_PATH = COMPAT_ROOT / "profiles/0.84.4.json"
FIXTURE_ROOT = COMPAT_ROOT / "tests/fixtures/conformance"
CONFORMANCE = COMPAT_ROOT / "conformance.py"


def fixture_document(name: str) -> dict:
    return json.loads((FIXTURE_ROOT / name).read_text(encoding="utf-8"))


def conformance_module():
    spec = importlib.util.spec_from_file_location("pi_conformance_test_module", CONFORMANCE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def method_messages(messages: list[dict], method: str) -> list[dict]:
    return [message for message in messages if message.get("method") == method]


class ConformanceHarnessTests(unittest.TestCase):
    def test_checked_in_ledger_gate_is_machine_readable_and_not_a_real_runtime_claim(self) -> None:
        completed = subprocess.run(
            [sys.executable, str(CONFORMANCE), "--check", "--json"],
            cwd=COMPAT_ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, completed.returncode, completed.stderr)
        report = json.loads(completed.stdout)
        self.assertTrue(report["ok"])
        self.assertEqual("not_supplied", report["real_runtime"])
        self.assertEqual(118, report["public_surface_rows"])
        self.assertEqual(78, report["official_examples"])
        self.assertEqual(33, report["tui_audit_rows"])
        self.assertEqual(6, report["plan_journeys"])

    def test_real_runtime_aggregate_fixture_is_ordered_and_explicitly_unrun(self) -> None:
        module = conformance_module()
        fixture = fixture_document("real-runtime-aggregate.json")

        self.assertEqual(fixture, module.check_real_runtime_aggregate_fixture())
        self.assertEqual(
            [source["id"] for source in fixture["sources"]],
            ["hello", "plan-mode"],
        )
        self.assertEqual(
            fixture["evidence"]["status"],
            "unrun_until_explicit_real_package_and_source_root_are_supplied",
        )

    def test_full_gate_refuses_before_loading_without_network_isolation(self) -> None:
        completed = subprocess.run(
            [sys.executable, str(CONFORMANCE), "--full", "--json"],
            cwd=COMPAT_ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(1, completed.returncode)
        self.assertIn("--network-isolated", json.loads(completed.stdout)["error"])

    def test_full_gate_requires_the_pinned_monorepo_example_inventory(self) -> None:
        # Fixture-only path regression: integrity and execution boundaries are
        # mocked. This never claims that a real Pi runtime loaded an example.
        module = conformance_module()
        profile = json.loads(PROFILE_PATH.read_text(encoding="utf-8"))
        examples = profile["official_extension_examples"]
        self.assertEqual(78, len(examples))
        for layout in ("monorepo", "wrong-root", "missing-entry"):
            with self.subTest(layout=layout), tempfile.TemporaryDirectory() as temporary:
                # Match the full gate's canonical source-root boundary, including
                # macOS /var -> /private/var aliases; retain exact path assertions.
                checkout = Path(temporary).resolve() / "source"
                relative = (
                    "examples/extensions" if layout == "wrong-root"
                    else "packages/coding-agent/examples/extensions"
                )
                examples_root = checkout / relative
                for example in examples:
                    path = examples_root / example
                    path.parent.mkdir(parents=True, exist_ok=True)
                    if path.suffix:
                        path.write_text("export default () => {};\n", encoding="utf-8")
                    else:
                        path.mkdir()
                for source in fixture_document("real-runtime.json")["sources"]:
                    path = examples_root / source["path"]
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text("export default () => {};\n", encoding="utf-8")
                if layout == "missing-entry":
                    missing = examples_root / examples[-1]
                    if missing.is_dir():
                        missing.rmdir()
                    else:
                        missing.unlink()
                package = Path(temporary) / "package"
                tui = package / "node_modules/@earendil-works/pi-tui"
                node = Path(temporary).resolve() / "fixture-node"
                # The entrypoint checks executability; execution itself stays mocked.
                node.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")
                node.chmod(0o700)
                arguments = SimpleNamespace(
                    network_isolated=True,
                    network_backend="unshare",
                    coding_agent_tarball=Path(temporary) / "coding-agent.tgz",
                    tui_tarball=Path(temporary) / "tui.tgz",
                    pi_package=package,
                    source_root=checkout,
                )
                with (
                    patch.object(
                        module,
                        "select_network_backend",
                        return_value=module.NetworkBackend(
                            "unshare", "/fixture/unshare", "linux_unshare_net"
                        ),
                    ),
                    patch.object(module, "verify_tarball") as verify_tarball,
                    patch.object(module, "verify_package_root", side_effect=[package, tui]),
                    patch.object(module, "node_resolved_package", return_value=tui),
                    patch.object(module, "git", side_effect=[module.REVISION, ""]),
                    patch.object(module.shutil, "which", return_value=str(node)),
                    patch.object(module, "fingerprint", return_value="f" * 64) as fingerprint,
                    patch.object(module, "runtime_integrity", return_value="r" * 64),
                    patch.object(module, "run_real_aggregate", return_value={"status": "stub"}),
                    patch.object(module, "load_source") as load_source,
                ):
                    if layout != "monorepo":
                        with self.assertRaisesRegex(module.GateFailure, "exact official example inventory"):
                            module.run_full(arguments, {})
                        load_source.assert_not_called()
                        fingerprint.assert_not_called()
                    else:
                        report = {}
                        module.run_full(arguments, report)
                        self.assertEqual(78, load_source.call_count)
                        self.assertEqual(
                            [examples_root / example for example in examples],
                            [call.args[3] for call in load_source.call_args_list],
                        )
                        for call in load_source.call_args_list:
                            self.assertEqual((str(node), package, checkout), call.args[:3])
                            self.assertEqual("f" * 64, call.args[4])
                            self.assertEqual("linux_unshare_net", call.args[6].evidence_name)
                        environment = load_source.call_args_list[0].args[5]
                        self.assertEqual(
                            {
                                "HOME",
                                "TMPDIR",
                                "PATH",
                                "LANG",
                                "LC_ALL",
                                "NO_PROXY",
                                "no_proxy",
                                "HTTP_PROXY",
                                "HTTPS_PROXY",
                                "http_proxy",
                                "https_proxy",
                            },
                            set(environment),
                        )
                        self.assertTrue(environment["HOME"].startswith("/var/tmp/"))
                        self.assertEqual(str(Path(environment["HOME"]) / "tmp"), environment["TMPDIR"])
                        self.assertEqual("linux_unshare_net", report["network_isolation"])
                        self.assertEqual(81, fingerprint.call_count)
                    self.assertEqual(2, verify_tarball.call_count)

    def test_dynamic_runtime_loaders_register_postponed_dataclass_annotations(self) -> None:
        # Exercise the import boundary without launching Node, Pi, or a sandbox.
        module = conformance_module()
        with tempfile.TemporaryDirectory() as temporary, patch.dict(sys.modules):
            root = Path(temporary)
            (root / "real_runtime.py").write_text(
                "from __future__ import annotations\n"
                "from dataclasses import dataclass\n"
                "@dataclass\n"
                "class Marker:\n"
                "    value: str\n"
                "closed = False\n"
                "class JsonRpcPeer:\n"
                "    def __init__(self, *args): self.messages = []\n"
                "    def send_request(self, *args): return 1\n"
                "    def response(self, *args): return {}\n"
                "    def close(self):\n"
                "        global closed\n"
                "        closed = True\n"
                "def run_real_aggregate(**kwargs):\n"
                "    return Marker(kwargs['marker']).value\n",
                encoding="utf-8",
            )
            backend = SimpleNamespace(command=lambda command, **_kwargs: command)
            with patch.object(module, "ROOT", root):
                self.assertEqual("loaded", module.run_real_aggregate(marker="loaded"))
                module.load_source(
                    "fixture-node", root, root, root / "example.ts", "f" * 64,
                    {"HOME": str(root)}, backend,
                )
            self.assertTrue(sys.modules["octet_pi_bounded_protocol"].closed)

    def test_full_gate_compares_the_entire_selected_package_payload(self) -> None:
        module = conformance_module()
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            root = temporary_path / "pi-coding-agent"
            (root / "dist").mkdir(parents=True)
            (root / "package.json").write_text(
                json.dumps({"name": "@earendil-works/pi-coding-agent", "version": "0.84.4"}),
                encoding="utf-8",
            )
            (root / "dist/index.js").write_text("export {};\n", encoding="utf-8")
            tarball = temporary_path / "pi-coding-agent.tgz"
            with tarfile.open(tarball, "w:gz") as archive:
                for relative in ("package.json", "dist/index.js"):
                    archive.add(root / relative, arcname=f"package/{relative}")

            tui = root / "node_modules/@earendil-works/pi-tui"
            tui.mkdir(parents=True)
            self.assertEqual(
                tui,
                module.node_resolved_package(root, "@earendil-works/pi-tui"),
            )
            self.assertEqual(
                root.absolute(),
                module.verify_package_root(
                    tarball,
                    root,
                    "@earendil-works/pi-coding-agent",
                    "dist/index.js",
                ),
            )

            (root / "unexpected.js").write_text("export default null;\n", encoding="utf-8")
            with self.assertRaisesRegex(module.GateFailure, "file inventory differs"):
                module.verify_package_root(
                    tarball,
                    root,
                    "@earendil-works/pi-coding-agent",
                    "dist/index.js",
                )


@unittest.skipUnless(NODE, "node is required for Pi conformance fixture subprocesses")
class PublicSurfaceFixtureTests(unittest.TestCase):
    def test_every_non_event_surface_runs_its_declared_fixture(self) -> None:
        profile = json.loads(PROFILE_PATH.read_text(encoding="utf-8"))
        fixtures = fixture_document("public-surfaces.json")["fixtures"]
        expected = {
            f"{area}.{surface}"
            for area, surfaces in profile["public_surface"].items()
            if area != "events"
            for surface in surfaces
        }
        declared = {
            fixture["surface"]
            for fixture in fixtures
            if fixture["kind"] not in {"event_registration", "lifecycle_or_hook"}
        }
        self.assertEqual(expected, declared)

        with BridgeProcess() as bridge:
            bridge.handlers["input/request"] = lambda _message: {"value": "1"}
            bridge.handlers["confirmation/request"] = lambda _message: {"confirmed": True}
            bridge.initialize("runtime_commands")
            for surface in sorted(declared):
                response = bridge.request(
                    "command/execute",
                    {"name": "surface-probe", "arguments": [surface]},
                )
                self.assertNotIn("error", response, surface)
                self.assertTrue(
                    any(item.startswith(f"surface:{surface}:") for item in bridge.notifications()),
                    surface,
                )

    def test_unbridged_events_and_registration_surfaces_are_diagnosed_at_startup(self) -> None:
        fixtures = fixture_document("public-surfaces.json")["fixtures"]
        unbridged = [
            fixture["target"]
            for fixture in fixtures
            if fixture["kind"] == "event_registration"
        ]
        self.assertGreater(len(unbridged), 0)
        with BridgeProcess(fixture_events=unbridged) as bridge:
            bridge.initialize()
            for event in unbridged:
                self.assertTrue(
                    any(f"event {event} is unavailable" in line for line in bridge.stderr),
                    event,
                )

        with BridgeProcess(fixture_mode="registration") as bridge:
            bridge.initialize()
            for label in ("shortcuts", "flags", "message renderers", "entry renderers", "markdown transformer"):
                self.assertTrue(any(f"{label} is unavailable" in line for line in bridge.stderr), label)

    def test_every_bridged_event_has_an_executable_lifecycle_or_hook_path(self) -> None:
        fixtures = fixture_document("public-surfaces.json")["fixtures"]
        bridged = {
            fixture["target"]
            for fixture in fixtures
            if fixture["kind"] == "lifecycle_or_hook"
        }
        actions = {
            "session_start": lambda bridge: bridge.request("session/started", {}),
            "session_shutdown": lambda bridge: bridge.request("session/settled", {}),
            "context": lambda bridge: bridge.request("context/collect", {"prompt": "fixture"}),
            "before_agent_start": lambda bridge: bridge.request(
                "hook/run", {"hook": "before_prompt", "payload": {"prompt": "fixture"}}
            ),
            "agent_start": lambda bridge: bridge.request("turn/started", {}),
            "agent_end": lambda bridge: bridge.request("turn/settled", {"outcome": "cancelled"}),
            "agent_settled": lambda bridge: bridge.request("turn/settled", {"outcome": "cancelled"}),
            "turn_start": lambda bridge: bridge.request("turn/started", {}),
            "turn_end": lambda bridge: bridge.request("turn/settled", {"outcome": "cancelled"}),
            "tool_execution_start": lambda bridge: bridge.request(
                "tool/started", {"tool_call_id": "fixture-start", "tool_name": "bash"}
            ),
            "tool_execution_update": lambda bridge: bridge.request(
                "tool/call", {"name": "fixture_progress", "arguments": {}, "catalog_revision": 0}
            ),
            "tool_execution_end": lambda bridge: bridge.request(
                "tool/settled", {"tool_call_id": "fixture-end", "tool_name": "bash", "outcome": "completed"}
            ),
            "tool_call": lambda bridge: bridge.request(
                "hook/run", {"hook": "before_tool_call", "payload": {"name": "bash", "arguments": {}}}
            ),
            "tool_result": lambda bridge: bridge.request(
                "tool/call", {"name": "fixture_echo", "arguments": {}, "catalog_revision": 0}
            ),
        }
        self.assertEqual(set(actions), bridged)
        with BridgeProcess() as bridge:
            bridge.initialize("request_progress", "lifecycle_events")
            for event, action in actions.items():
                response = action(bridge)
                self.assertNotIn("error", response, event)
            notifications = bridge.notifications()
            for event in bridged:
                self.assertIn(f"event:{event}:start", notifications, event)


@unittest.skipUnless(NODE, "node is required for Pi conformance fixture subprocesses")
class DeferredPlanModeSurfaceTests(unittest.TestCase):
    DEFERRED_HOST_SEAMS = [
        "tool_policy",
        "session_state",
        "messages",
        "widgets",
        "editor",
        "shortcuts",
        "flags",
    ]

    def test_plan_mode_host_control_seams_remain_explicitly_deferred(self) -> None:
        plan = fixture_document("plan-mode-journey.json")
        self.assertEqual(self.DEFERRED_HOST_SEAMS, plan["deferred_host_seams"])
        self.assertTrue(
            all(row["assertion"].startswith(("Deferred:", "Supported:")) for row in plan["journeys"])
        )

        # A future host may define a bounded projection, but this integration
        # deliberately keeps the Pi bridge's shortcut, session-control, and
        # editor/widget remainder out of the live protocol. Supplying a shape
        # that resembles that future projection must not silently enable it.
        with BridgeProcess(fixture_mode="registration") as bridge:
            bridge.initialize(
                "runtime_commands",
                host={"pi_compat": {"features": self.DEFERRED_HOST_SEAMS}},
            )
            for label in ("shortcuts", "flags", "message renderers", "entry renderers", "markdown transformer"):
                self.assertTrue(any(f"{label} is unavailable" in line for line in bridge.stderr), label)
            for surface in (
                "extension_api.sendMessage",
                "extension_api.sendUserMessage",
                "extension_api.appendEntry",
                "extension_api.setSessionName",
                "extension_api.setActiveTools",
                "ui_context.setWidget",
                "ui_context.editor",
                "context.sessionManager",
            ):
                response = bridge.request(
                    "command/execute",
                    {"name": "surface-probe", "arguments": [surface]},
                )
                self.assertNotIn("error", response, surface)
                self.assertIn(f"surface:{surface}:explicit", bridge.notifications(), surface)


@unittest.skipUnless(NODE, "node is required for Pi conformance fixture subprocesses")
class DeferredControlSurfaceTests(unittest.TestCase):
    """The deferred session/root/model/thinking/compaction/idle remainder.

    Every deferred control surface is refused explicitly on the bindings, and the
    wire carries no child method that could silently serve it: the bridge neither
    consumes a `host.pi_compat`-shaped seam offer nor emits a `pi/*` request.
    """

    DEFERRED_SEAMS = [
        "tool_policy",
        "session_state",
        "messages",
        "widgets",
        "editor",
        "shortcuts",
        "flags",
    ]
    # Plausible child-method spellings for the deferred remainder. None of these
    # is a negotiated method on either wire.
    DEFERRED_METHODS = (
        "pi/session",
        "pi/messages",
        "pi/entry",
        "pi/label",
        "pi/model",
        "pi/thinkingLevel",
        "pi/activeTools",
        "pi/compact",
        "pi/waitForIdle",
        "shortcut/trigger",
        "session/create",
        "session/fork",
        "session/switch",
        "session/reload",
        "tree/navigate",
        "entry/append",
        "label/set",
        "model/set",
        "thinking/set",
        "tools/active",
    )
    DEFERRED_SURFACES = (
        "extension_api.sendMessage",
        "extension_api.sendUserMessage",
        "extension_api.appendEntry",
        "extension_api.setSessionName",
        "extension_api.setLabel",
        "extension_api.setActiveTools",
        "extension_api.setModel",
        "extension_api.setThinkingLevel",
        "context.hasPendingMessages",
        "context.compact",
        "context.getSystemPrompt",
        "context.shutdown",
        "context.newSession",
        "context.fork",
        "context.navigateTree",
        "context.switchSession",
        "context.reload",
        "context.replacement.sendMessage",
        "context.replacement.sendUserMessage",
    )

    def test_deferred_host_seam_offer_enables_no_child_method(self) -> None:
        with BridgeProcess() as bridge:
            baseline = sorted(bridge.initialize("runtime_commands")["protocol"]["features"])
        with BridgeProcess(fixture_mode="registration") as bridge:
            offered = bridge.initialize(
                "runtime_commands",
                host={"pi_compat": {"features": self.DEFERRED_SEAMS}},
            )
            self.assertEqual(baseline, sorted(offered["protocol"]["features"]))
            for surface in self.DEFERRED_SURFACES:
                response = bridge.request(
                    "command/execute",
                    {"name": "surface-probe", "arguments": [surface]},
                )
                self.assertNotIn("error", response, surface)
                self.assertIn(f"surface:{surface}:explicit", bridge.notifications(), surface)
            for method in self.DEFERRED_METHODS:
                response = bridge.request(method, {})
                self.assertIn("error", response, method)
            emitted = {
                message.get("method")
                for message in bridge.messages
                if isinstance(message.get("method"), str)
            }
            self.assertEqual(set(self.DEFERRED_METHODS) & emitted, set())

    def test_api_03_selects_no_deferred_control_method(self) -> None:
        with BridgeProcess(api_version="0.3") as bridge:
            initialized = bridge.initialize(host={"pi_compat": {"features": self.DEFERRED_SEAMS}})
            selected = set(initialized["contract"]["methods"])
            self.assertEqual(set(self.DEFERRED_METHODS) & selected, set())
            self.assertTrue(set(v03_contract(providers=False)["required_methods"]) <= selected)
            for method in self.DEFERRED_METHODS:
                response = bridge.request(method, {})
                self.assertEqual(-32601, response["error"]["code"], method)

    def test_busy_idle_queue_refuses_and_idle_boundary_returns(self) -> None:
        with BridgeProcess() as bridge:
            bridge.initialize("runtime_commands")
            busy = bridge.request("command/execute", {"name": "surface-probe", "arguments": ["context.waitForIdle"]})
            self.assertNotIn("error", busy)
            bridge.request("command/execute", {"name": "surface-probe", "arguments": ["context.isIdle"]})
            self.assertIn("surface:context.isIdle:bounded:true", bridge.notifications())
            bridge.request("turn/started", {})
            while_busy = bridge.request(
                "command/execute", {"name": "surface-probe", "arguments": ["context.waitForIdle"]}
            )
            self.assertIn("error", while_busy)
            self.assertIn("idle-wait service unavailable", while_busy["error"]["message"])
            bridge.request("command/execute", {"name": "surface-probe", "arguments": ["context.isIdle"]})
            self.assertIn("surface:context.isIdle:bounded:false", bridge.notifications())
            bridge.request("turn/settled", {"outcome": "cancelled"})
            after = bridge.request(
                "command/execute", {"name": "surface-probe", "arguments": ["context.waitForIdle"]}
            )
            self.assertNotIn("error", after)


if __name__ == "__main__":
    unittest.main()
