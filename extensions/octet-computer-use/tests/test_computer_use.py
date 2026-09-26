"""Behavioural tests for the octet computer-use bundle."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from octet_computer_use import service
from octet_computer_use.driver_client import DriverClient, McpError, ToolInfo
from octet_computer_use.entrypoint import ComputerUse, _DRIVER_TOOLS, _render_status
from octet_computer_use.service import ArgumentError, sanitize, summarize_result

try:  # the prototype suite discovers tests flat; the bundle suite uses packages
    from .helpers import FakeClient, RecordingExtension
except ImportError:  # pragma: no cover - exercised by the flat discovery mode
    from helpers import FakeClient, RecordingExtension


class SanitizeTests(unittest.TestCase):
    def test_only_reviewed_arguments_reach_the_driver(self):
        values = {"pid": 42, "window_id": 7, "sneaky": "value", "shell": "; rm -rf /"}
        forwarded = sanitize("get_window_state", values)
        self.assertEqual(forwarded, {"pid": 42, "window_id": 7})

    def test_types_and_bounds_are_enforced(self):
        with self.assertRaises(ArgumentError):
            sanitize("get_window_state", {"pid": "42"})
        with self.assertRaises(ArgumentError):
            sanitize("get_window_state", {"pid": True})
        with self.assertRaises(ArgumentError):
            sanitize("get_window_state", {"include_screenshot": "yes"})
        with self.assertRaises(ArgumentError):
            sanitize("type_text", {"text": "x" * 5000})
        with self.assertRaises(ArgumentError):
            sanitize("get_window_state", {"max_elements": 10_000})
        # A false boolean is a real value, not an absent one.
        self.assertIs(
            sanitize("get_window_state", {"include_screenshot": False})["include_screenshot"],
            False,
        )

    def test_unreviewed_tool_is_refused_outright(self):
        with self.assertRaises(ArgumentError):
            sanitize("totally_unknown_tool", {})

    def test_urls_are_bounded(self):
        forwarded = sanitize("launch_app", {"urls": ["https://example.invalid"]})
        self.assertEqual(forwarded["urls"], ["https://example.invalid"])
        with self.assertRaises(ArgumentError):
            sanitize("launch_app", {"urls": [{"nested": True}]})


class SummarizeTests(unittest.TestCase):
    def test_large_text_is_bounded_and_images_are_counted(self):
        result = {
            "content": [
                {"type": "text", "text": "a" * 500_000},
                {"type": "image", "data": "x" * 1000},
                {"type": "image", "data": "y" * 1000},
            ],
            "isError": False,
        }
        summary = summarize_result(result)
        self.assertTrue(summary["truncated"])
        self.assertLessEqual(len(summary["text"]), 200_100)
        self.assertEqual(summary["image_count"], 2)
        # The payload itself is never inlined.
        self.assertNotIn("xxxx", summary["text"])


class ConfirmationGateTests(unittest.TestCase):
    def setUp(self):
        self._temporary = tempfile.TemporaryDirectory()
        self.home = Path(self._temporary.name)

    def tearDown(self):
        self._temporary.cleanup()

    def use(self, client, *, confirm=True):
        extension = RecordingExtension(confirm=confirm)
        computer_use = ComputerUse(extension, home=self.home)
        computer_use._client = client
        return computer_use, extension

    def test_read_only_tool_never_prompts(self):
        client = FakeClient(read_only=["get_window_state"])
        computer_use, extension = self.use(client)
        computer_use.call("get_window_state", {"pid": 1, "window_id": 2})
        self.assertEqual(extension.confirmations, [])
        self.assertEqual(client.calls[0][0], "get_window_state")

    def test_effectful_tool_prompts_once_and_dispatches_when_approved(self):
        client = FakeClient(effectful=["click"])
        computer_use, extension = self.use(client, confirm=True)
        computer_use.call("click", {"pid": 1, "x": 5, "y": 6})
        self.assertEqual(len(extension.confirmations), 1)
        self.assertFalse(extension.confirmations[0]["default"])
        self.assertIn("click", extension.confirmations[0]["detail"])
        # An effectful action may probe the OS grant once before confirming;
        # the driver must still receive exactly the action itself, never a
        # second dispatch.
        self.assertEqual([name for name, _ in client.calls].count("click"), 1)

    def test_declined_confirmation_does_not_dispatch(self):
        client = FakeClient(effectful=["click"])
        computer_use, extension = self.use(client, confirm=False)
        result = computer_use.call("click", {"pid": 1, "x": 5, "y": 6})
        self.assertNotIn("click", [name for name, _ in client.calls])
        self.assertTrue(result["is_error"])
        self.assertIn("did not confirm", result["content"][0]["text"])

    def test_missing_confirmation_surface_fails_closed(self):
        client = FakeClient(effectful=["click"])
        computer_use, extension = self.use(client, confirm=None)
        result = computer_use.call("click", {"pid": 1, "x": 5, "y": 6})
        self.assertNotIn("click", [name for name, _ in client.calls])
        self.assertTrue(result["is_error"])

    def test_unknown_tool_is_treated_as_effectful(self):
        client = FakeClient(read_only=["get_window_state"])
        self.assertFalse(client.requires_confirmation("get_window_state"))
        self.assertTrue(client.requires_confirmation("click"))
        self.assertTrue(client.requires_confirmation("never_described"))

    def test_destructive_tools_are_flagged_destructive(self):
        client = FakeClient(effectful=["kill_app"])
        computer_use, extension = self.use(client, confirm=True)
        # kill_app is not a republished tool, so drive the gate directly.
        client.requires_confirmation = lambda tool: True  # type: ignore[assignment]
        computer_use.call("launch_app", {"name": "Calculator"})
        self.assertTrue(extension.confirmations[-1]["destructive"] is False)


class RegistrationTests(unittest.TestCase):
    def test_manifest_tools_match_the_runtime(self):
        import re

        manifest = (Path(__file__).resolve().parents[1] / "extension.toml").read_text(
            encoding="utf-8"
        )
        declared = set(re.findall(r'"(computer_use_[a-z_]+)"', manifest))
        runtime = {name for name, _driver, _desc in service.PUBLISHED_TOOLS}
        self.assertEqual(declared, runtime, "manifest and runtime tool sets must match")

    def test_every_published_tool_has_a_driver_mapping_and_schema(self):
        for name, driver_tool, description in service.PUBLISHED_TOOLS:
            self.assertIn(name, _DRIVER_TOOLS)
            self.assertTrue(description.strip())
            self.assertIn(driver_tool, service._ARGUMENTS, f"{name} has no argument allowlist")

    def test_manifest_environment_matches_the_runtime_allowlist(self):
        import tomllib

        from octet_computer_use.driver_client import SESSION_ENVIRONMENT

        manifest = (Path(__file__).resolve().parents[1] / "extension.toml").read_bytes()
        declared = set(tomllib.loads(manifest.decode())["capabilities"]["environment"])
        # The two-stage grant: the manifest gates what the host may forward, the
        # runtime gates what the bundle forwards. They must not drift, or a name
        # could be declared but unreachable (or reachable without review).
        self.assertEqual(declared, set(SESSION_ENVIRONMENT))

    def test_local_tools_are_never_dispatched_to_the_driver(self):
        import tempfile

        from octet_computer_use.entrypoint import LOCAL_ONLY_TOOLS

        with tempfile.TemporaryDirectory() as directory:
            for tool in LOCAL_ONLY_TOOLS:
                with self.subTest(tool=tool):
                    computer_use = ComputerUse(
                        RecordingExtension(), home=Path(directory)
                    )
                    client = FakeClient(effectful=[tool])
                    computer_use._client = client
                    # The runtime refuses a locally-handled tool before it can
                    # reach the driver process.
                    with self.assertRaises(ArgumentError):
                        computer_use.call(tool, {})
                    self.assertEqual(client.calls, [])
                    self.assertNotIn(tool, _DRIVER_TOOLS)


class ClientClassificationTests(unittest.TestCase):
    def test_requires_confirmation_is_fail_closed(self):
        client = DriverClient.__new__(DriverClient)
        client._tools = {
            "get_window_state": ToolInfo("get_window_state", "", {}, True),
            "click": ToolInfo("click", "", {}, False),
        }
        self.assertFalse(client.requires_confirmation("get_window_state"))
        self.assertTrue(client.requires_confirmation("click"))
        self.assertTrue(client.requires_confirmation("absent"))

    def test_child_environment_is_limited_to_reviewed_session_names(self):
        import os
        from unittest import mock

        from octet_computer_use.driver_client import SESSION_ENVIRONMENT, _child_environment

        with mock.patch.dict(
            os.environ,
            {"DISPLAY": ":0", "OPENAI_API_KEY": "secret", "AWS_SECRET_ACCESS_KEY": "secret"},
        ):
            environment = _child_environment()
        self.assertEqual(environment.get("DISPLAY"), ":0")
        self.assertNotIn("OPENAI_API_KEY", environment)
        self.assertNotIn("AWS_SECRET_ACCESS_KEY", environment)
        for name in environment:
            self.assertIn(name, SESSION_ENVIRONMENT)

    def test_explicit_overrides_win_over_inherited_names(self):
        from octet_computer_use.driver_client import _child_environment

        environment = _child_environment({"DISPLAY": ":9"})
        self.assertEqual(environment["DISPLAY"], ":9")


class VersionSpecTests(unittest.TestCase):
    def test_pip_spec_is_validated(self):
        from octet_computer_use.driver import _pip_spec, ProvisionError

        self.assertEqual(_pip_spec(""), "cua-driver")
        self.assertEqual(_pip_spec("0.29.1"), "cua-driver==0.29.1")
        for bad in ("--index-url=http://evil", "1.0; rm -rf /", "a b", "1.0 2.0"):
            with self.subTest(bad=bad):
                with self.assertRaises(ProvisionError):
                    _pip_spec(bad)


class PermissionProbeTests(unittest.TestCase):
    """Permission truth must come from the live direct session, not the CLI.

    ``cua-driver permissions status`` only answers from a CuaDriver daemon. The
    pip-provisioned macOS path ships a bare binary and never installs
    ``/Applications/CuaDriver.app``, so the CLI probe reported ``unknown`` even
    when both grants were present. The direct MCP session sees the real host
    state, so that is what the bundle must use.
    """

    def _state(self, accessibility, screen_recording, raises=None):
        from octet_computer_use.driver import permission_state

        structured = {
            "accessibility": accessibility,
            "screen_recording": screen_recording,
        }
        client = FakeClient(result={"structuredContent": structured}, raises=raises)
        return permission_state(client), client

    def test_both_grants_report_granted(self):
        state, client = self._state(True, True)
        self.assertEqual(state["permissions"], "granted")
        self.assertEqual(client.calls, [("check_permissions", {"prompt": False})])

    def test_one_missing_grant_is_named_and_not_collapsed(self):
        state, _ = self._state(True, False)
        self.assertEqual(state["permissions"], "denied")
        self.assertIn("Screen Recording", state["detail"])
        self.assertNotIn("Accessibility", state["detail"])

    def test_unknown_booleans_stay_unknown(self):
        state, _ = self._state(None, None)
        self.assertEqual(state["permissions"], "unknown")

    def test_prompt_is_staged_and_never_probes_direct_capture(self):
        from octet_computer_use.driver import permission_state

        client = FakeClient(
            result={"structuredContent": {"accessibility": True, "screen_recording": True}}
        )
        permission_state(client, prompt=True)
        self.assertEqual(
            client.calls,
            [("check_permissions", {"prompt": True, "probe_direct_capture": False})],
        )

    def test_a_driver_failure_degrades_to_unknown_instead_of_raising(self):
        state, _ = self._state(True, True, raises=McpError("driver gone"))
        self.assertEqual(state["permissions"], "unknown")
        self.assertIn("did not answer", state["detail"])


class StatusRowTests(unittest.TestCase):
    """An installed bundle reports its own readiness, like web-search does."""

    class _RecordingStatus(RecordingExtension):
        def __init__(self):
            super().__init__()
            self.statuses = []

        def set_status(self, payload):
            self.statuses.append(payload)

    def _publish(self, status):
        extension = self._RecordingStatus()
        computer_use = ComputerUse(extension)
        computer_use.status = lambda **_: dict(status)
        computer_use.publish_status()
        return extension.statuses

    def test_ready_state_reports_active(self):
        statuses = self._publish({"installed": True, "permissions": "granted"})
        self.assertEqual(
            statuses,
            [{"state": "active", "label": "computer use · ready"}],
        )

    def test_missing_screen_recording_is_visible_before_any_action(self):
        statuses = self._publish(
            {"installed": True, "permissions": "denied", "screen_recording": False}
        )
        self.assertEqual(statuses[0]["state"], "pending")
        self.assertIn("Screen Recording", statuses[0]["label"])

    def test_unprovisioned_bundle_says_so(self):
        statuses = self._publish({"installed": False})
        self.assertIn("not set up", statuses[0]["label"])

    def test_a_host_without_the_status_surface_still_reports(self):
        computer_use = ComputerUse(RecordingExtension())
        computer_use.status = lambda **_: {"installed": True, "permissions": "granted"}
        # No set_status on this double: publishing must not raise.
        self.assertEqual(computer_use.publish_status()["permissions"], "granted")

    def test_rendered_status_names_the_missing_grant_and_the_fix(self):
        text = _render_status(
            {
                "installed": True,
                "version": "0.29.1",
                "doctor_ok": True,
                "permissions": "denied",
                "screen_recording": False,
                "permission_detail": "still needs: Screen Recording",
            }
        )
        self.assertIn("still needs: Screen Recording", text)
        self.assertIn("/computer-use setup", text)

    def test_granted_status_stops_short(self):
        text = _render_status(
            {
                "installed": True,
                "version": "0.29.1",
                "doctor_ok": True,
                "permissions": "granted",
            }
        )
        self.assertIn("Accessibility and Screen Recording allowed", text)
        self.assertNotIn("cannot grant a system permission", text)

    def test_not_installed_points_at_setup(self):
        self.assertIn("/computer-use setup", _render_status({"installed": False}))


if __name__ == "__main__":
    unittest.main()
