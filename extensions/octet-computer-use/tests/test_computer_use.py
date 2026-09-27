"""Behavioural tests for the octet computer-use bundle."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

from octet_computer_use import entrypoint, service
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
        with self.assertRaises(ArgumentError):
            sanitize("launch_app", {"urls": ["https://example.invalid"] * 9})

    def test_hotkey_menu_and_cursor_inputs_are_bounded_and_typed(self):
        self.assertEqual(
            sanitize("hotkey", {"keys": ["CTRL", "S"], "snapshot_id": "snap-1"}),
            {"keys": ["CTRL", "S"], "snapshot_id": "snap-1"},
        )
        self.assertEqual(
            sanitize("invoke_menu", {"pid": 12, "window_id": 3, "path": ["File", "Save"]})["path"],
            ["File", "Save"],
        )
        self.assertEqual(
            sanitize("move_cursor", {"pid": 12, "window_id": 3, "x": 15, "y": 20}),
            {"pid": 12, "window_id": 3, "x": 15, "y": 20},
        )
        with self.assertRaises(ArgumentError):
            sanitize("hotkey", {"keys": "CTRL+S"})
        with self.assertRaises(ArgumentError):
            sanitize("invoke_menu", {"path": [""]})
        with self.assertRaises(ArgumentError):
            sanitize("move_cursor", {"x": -1})

    def test_click_preserves_snapshot_and_capture_handles(self):
        from octet_computer_use.entrypoint import _schema_for

        values = {"pid": 12, "window_id": 3, "element_index": 4,
                  "snapshot_id": "s00000001", "capture_id": "capture-1"}
        self.assertEqual(sanitize("click", values), values)
        schema = _schema_for("click")
        self.assertEqual(schema["properties"]["snapshot_id"]["maxLength"], 128)
        self.assertIn("capture_id", schema["properties"])
        self.assertEqual(_schema_for("move_cursor")["properties"]["x"]["minimum"], 0)
        self.assertEqual(_schema_for("get_window_state")["properties"]["max_elements"]["maximum"],
                         service.MAX_ELEMENTS)

    def test_jev_arrays_are_projected_to_typed_bounds(self):
        clean = sanitize("jev_choose", {
            "goal": "open settings",
            "candidates": [{"identifier": "settings", "description": "Open Settings"}],
            "regions": [{"id": "r1", "role": "button", "label": "Settings", "enabled": True,
                         "geometry": {"x": 2}}],
            "history": ["observed settings button"],
            "unexpected": "dropped",
        })
        self.assertEqual(clean["candidates"], [{"identifier": "settings", "description": "Open Settings"}])
        self.assertEqual(clean["regions"], [{"id": "r1", "role": "button", "label": "Settings", "enabled": True}])
        self.assertNotIn("unexpected", clean)
        with self.assertRaises(ArgumentError):
            sanitize("jev_choose", {"goal": "x", "candidates": [{"identifier": "a", "description": "a"}],
                                     "history": "not an array"})

    def test_oversized_structured_state_keeps_targeting_metadata_under_host_limit(self):
        from octet_computer_use.service import bound_structured_content, _json_size

        snapshot = {
            "snapshot_id": "snap-1", "pid": 42, "window_id": 7,
            "elements": [{"element_index": i, "element_token": f"snap-1:{i}",
                          "role": "AXButton", "label": "x" * 1000,
                          "frame": {"x": i, "y": 0, "w": 10, "h": 10},
                          "value": "v" * 1000} for i in range(1000)],
            "markdown": "m" * 500000,
        }
        bounded = bound_structured_content(snapshot)
        self.assertLessEqual(_json_size(bounded), service.STRUCTURED_CONTENT_BUDGET)
        self.assertEqual(bounded["snapshot_id"], "snap-1")
        self.assertEqual(bounded["window_id"], 7)
        self.assertTrue(bounded["truncated"])
        self.assertIn("element_token", bounded["elements"][0])

    def test_pathological_target_labels_cannot_exceed_host_limit(self):
        from octet_computer_use.service import bound_structured_content, _json_size

        bounded = bound_structured_content({
            "snapshot_id": "snap-2",
            "pid": 42,
            "window_id": 7,
            "title": "t" * 500_000,
        })
        self.assertLessEqual(_json_size(bounded), service.STRUCTURED_CONTENT_BUDGET)
        self.assertEqual(bounded["snapshot_id"], "snap-2")
        self.assertEqual(bounded["window_id"], 7)
        self.assertTrue(bounded["truncated"])


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


class ConfirmationModeTests(unittest.TestCase):
    """Full access must not prompt; a gated profile must.

    SECURITY.md: full access is the default and does not ask, and --safe-mode is
    the mode that asks before every effectful action.
    """

    def setUp(self):
        self._saved = {
            name: os.environ.pop(name, None)
            for name in ("OCTET_CUA_CONFIRM", "OCTET_EFFECT_POLICY")
        }

    def tearDown(self):
        for name, value in self._saved.items():
            os.environ.pop(name, None)
            if value is not None:
                os.environ[name] = value

    def test_defaults_to_open_under_full_access(self):
        from octet_computer_use.entrypoint import confirmations_enabled

        self.assertFalse(confirmations_enabled())

    def test_explicit_opt_in_gates_every_action(self):
        from octet_computer_use.entrypoint import confirmations_enabled

        for value in ("1", "true", "on", "yes"):
            os.environ["OCTET_CUA_CONFIRM"] = value
            self.assertTrue(confirmations_enabled(), value)

    def test_explicit_opt_out_wins_over_a_gated_profile(self):
        from octet_computer_use.entrypoint import confirmations_enabled

        os.environ["OCTET_EFFECT_POLICY"] = "safe"
        os.environ["OCTET_CUA_CONFIRM"] = "0"
        self.assertFalse(confirmations_enabled())

    def test_gated_profile_turns_the_gate_on(self):
        from octet_computer_use.entrypoint import confirmations_enabled

        os.environ["OCTET_EFFECT_POLICY"] = "safe"
        self.assertTrue(confirmations_enabled())


class ConfirmationGateTests(unittest.TestCase):
    def setUp(self):
        # These exercise the gate itself, so opt into it explicitly.
        self._saved_confirm = os.environ.get("OCTET_CUA_CONFIRM")
        os.environ["OCTET_CUA_CONFIRM"] = "1"
        self._temporary = tempfile.TemporaryDirectory()
        self.home = Path(self._temporary.name)

    def tearDown(self):
        self._temporary.cleanup()
        os.environ.pop("OCTET_CUA_CONFIRM", None)
        if self._saved_confirm is not None:
            os.environ["OCTET_CUA_CONFIRM"] = self._saved_confirm

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
    """Permission truth comes from the selected live MCP session, not the CLI.

    The CLI can report unknown for a direct runtime or an alternate app host;
    check_permissions reports the actual selected runtime's grant state.
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

    def test_unverified_cursor_never_reports_ready(self):
        statuses = self._publish({"installed": True, "runtime": "desktop-host",
                                  "permissions": "granted", "cursor_enabled": False})
        self.assertEqual(statuses[0], {"state": "pending", "label": "computer use · cursor not verified"})

    def test_cursor_start_failure_cannot_retain_a_granted_health_probe(self):
        from unittest import mock

        computer = ComputerUse(RecordingExtension())
        health = mock.Mock()
        health.as_dict.return_value = {"installed": True, "runtime": "desktop-host",
                                       "permissions": "granted", "cursor_enabled": False,
                                       "doctor_ok": True}
        with mock.patch.object(entrypoint.driver_module, "health", return_value=health), \
             mock.patch.object(computer, "client", side_effect=McpError("cursor refused")):
            report = computer.status()
        self.assertEqual(report["permissions"], "unknown")
        self.assertFalse(report["doctor_ok"])

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
        # The fix must name the app the user actually grants: the one running
        # octet. Pointing at a helper app would send them to grant the wrong
        # identity and never succeed.
        self.assertIn("app you run octet from", text)

    def test_status_reports_the_live_runtime(self):
        # The permission fix differs per runtime, so a user must be able to see
        # which one is live instead of guessing.
        direct = _render_status(
            {
                "installed": True,
                "version": "0.29.1",
                "doctor_ok": True,
                "permissions": "granted",
                "runtime": "direct",
            }
        )
        self.assertIn("runtime: direct", direct)
        host = _render_status(
            {
                "installed": True,
                "version": "0.29.1",
                "doctor_ok": True,
                "permissions": "granted",
                "runtime": "desktop-host",
            }
        )
        self.assertIn("runtime: desktop host", host)
        self.assertIn("cursor", host)
        unavailable = _render_status({"installed": True, "runtime": "unavailable"})
        self.assertIn("that host", unavailable)
        self.assertNotIn("app you run octet from", unavailable)
        denied_host = _render_status({"installed": True, "runtime": "desktop-host",
                                      "permissions": "denied"})
        self.assertIn("selected Cua Driver desktop host", denied_host)
        self.assertNotIn("app you run octet from", denied_host)

    def test_runtime_selection_fails_closed_by_default_on_macos(self):
        # The permission fix differs per runtime, so a user must be able to see
        # which one is live instead of guessing.
        from unittest import mock
        from octet_computer_use import driver

        original_env = os.environ.get("OCTET_CUA_DESKTOP_HOST")
        try:
            with mock.patch.object(driver.platform, "system", return_value="Darwin"), \
                 mock.patch.object(driver, "desktop_app_usable", return_value=False):
                os.environ["OCTET_CUA_DESKTOP_HOST"] = "0"
                self.assertEqual(driver.active_runtime(), "direct")
                os.environ["OCTET_CUA_DESKTOP_HOST"] = "1"
                self.assertEqual(driver.active_runtime(), "unavailable")
            with mock.patch.object(driver.platform, "system", return_value="Darwin"), \
                 mock.patch.object(driver, "desktop_app_usable", return_value=True):
                os.environ["OCTET_CUA_DESKTOP_HOST"] = "1"
                self.assertEqual(driver.active_runtime(), "desktop-host")
        finally:
            if original_env is None:
                os.environ.pop("OCTET_CUA_DESKTOP_HOST", None)
            else:
                os.environ["OCTET_CUA_DESKTOP_HOST"] = original_env

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




class CursorThemeTests(unittest.TestCase):
    def test_palette_matches_model_families_and_bundles_every_artifact(self):
        from octet_computer_use import cursor_theme

        examples = {"gpt-5.6": "openai", "claude-sonnet-4": "anthropic",
                    "gemini-3": "google", "deepseek-v4": "deepseek",
                    "qwen3": "alibaba", "grok-4": "xai", "opaque": "unknown"}
        for model, expected in examples.items():
            self.assertEqual(cursor_theme.model_lab(model), expected)
            self.assertTrue((cursor_theme.THEMES / (expected + ".cua-theme")).is_file())
        self.assertEqual(cursor_theme.model_lab("opaque", "anthropic"), "anthropic")
        self.assertEqual(cursor_theme.theme_for_host({"host": {"model": "gpt-5.6"}}), "openai")
        self.assertEqual(len(cursor_theme.PALETTE), 24)
        self.assertEqual(cursor_theme.PALETTE["openai"]["color"], "#767676")

    def test_only_local_setup_installs_the_bundled_themes(self):
        from unittest import mock
        from octet_computer_use import cursor_theme

        extension, computer = entrypoint.create_extension()
        context = {"host": {"model": "claude-sonnet-4"}}
        with mock.patch.object(computer, "provision", return_value={
                "provisioned": True, "binary": "/tmp/cua-driver", "version": "0.29.1"}), \
             mock.patch.object(computer, "publish_status", return_value={
                "installed": True, "permissions": "granted", "runtime": "desktop-host",
                "cursor_enabled": True, "cursor_theme": "com.octet.computeruse.anthropic",
                "cursor_personalized": True}), \
             mock.patch.object(entrypoint, "_setup_jev", return_value={"jev_setup": "skipped"}), \
             mock.patch.object(cursor_theme, "install_bundled_themes", return_value=24) as install:
            command = extension._commands["computer-use"].handler
            result = command(["setup"], context)
            self.assertEqual(result["structured_content"]["cursor_themes_installed"], 24)
            install.assert_called_once_with(Path("/tmp/cua-driver"))
            install.reset_mock()
            extension._tools["computer_use_setup"].handler({}, context)
            install.assert_not_called()
            install.side_effect = RuntimeError("invalid artifact")
            failed = command(["setup"], context)
            self.assertTrue(failed["is_error"])
            self.assertIn("cursor theme setup failed", failed["content"][0]["text"])

    def test_installer_uses_only_bundled_artifacts_and_reports_failure(self):
        from unittest import mock
        from octet_computer_use import cursor_theme

        with mock.patch.object(cursor_theme.subprocess, "run") as run:
            run.return_value.returncode = 0
            self.assertEqual(cursor_theme.install_bundled_themes(Path("/tmp/cua-driver")), 24)
            self.assertEqual(run.call_count, 24)
            for args, _ in run.call_args_list:
                command = args[0]
                self.assertEqual(command[:3], ["/tmp/cua-driver", "cursor-theme", "install"])
                self.assertEqual(Path(command[3]).parent, cursor_theme.THEMES)
            run.return_value.returncode = 1
            run.return_value.stderr = "rejected"
            with self.assertRaisesRegex(RuntimeError, "rejected"):
                cursor_theme.install_bundled_themes(Path("/tmp/cua-driver"))


class CursorSessionTests(unittest.TestCase):
    class _CursorClient:
        started = True

        def __init__(self, *, enabled=True, fail_tool=None, motion=None, stale_reads=0):
            self.enabled = enabled
            self.fail_tool = fail_tool
            self.motion = dict(entrypoint.CURSOR_MOTION if motion is None else motion)
            self.stale_reads = stale_reads
            self.selected_theme = "cua.default"
            self.calls = []

        def call(self, tool, arguments=None, **kwargs):
            args = dict(arguments or {})
            self.calls.append((tool, args))
            if tool == self.fail_tool:
                return {"isError": True, "content": [{"type": "text", "text": "rejected"}]}
            if tool == "set_agent_cursor_theme":
                self.selected_theme = args["theme_id"]
            if tool == "get_agent_cursor_state":
                if self.stale_reads:
                    self.stale_reads -= 1
                    return {"structuredContent": {"enabled": self.enabled,
                                                   "motion": {"idle_hide_ms": 20000.0},
                                                   "theme": {"id": self.selected_theme}}}
                return {"structuredContent": {"enabled": self.enabled,
                                               "motion": self.motion,
                                               "theme": {"id": self.selected_theme}}}
            return {"content": [{"type": "text", "text": "ok"}]}

        def requires_confirmation(self, tool):
            return False

        def tools(self):
            return []

        def close(self):
            self.started = False

    def _computer(self, *, enabled=True, fail_tool=None, motion=None, stale_reads=0):
        computer = ComputerUse(RecordingExtension())
        client = self._CursorClient(enabled=enabled, fail_tool=fail_tool,
                                    motion=motion, stale_reads=stale_reads)
        computer._client = client
        computer._app_daemon = True
        return computer, client

    def test_initialization_and_window_action_share_the_verified_session(self):
        original = os.environ.get("OCTET_CUA_CONFIRM")
        os.environ["OCTET_CUA_CONFIRM"] = "0"
        try:
            computer, client = self._computer()
            computer.call("click", {"pid": 17, "window_id": 5, "x": 10, "y": 20})
        finally:
            if original is None:
                os.environ.pop("OCTET_CUA_CONFIRM", None)
            else:
                os.environ["OCTET_CUA_CONFIRM"] = original
        session = computer._cursor_session
        self.assertEqual(session, entrypoint.cursor_session())
        self.assertTrue(computer._cursor_ready)
        for tool in ("start_session", "set_agent_cursor_motion", "set_agent_cursor_enabled",
                     "get_agent_cursor_state", "click"):
            args = next(args for name, args in client.calls if name == tool)
            if tool == "click":
                self.assertEqual(args["session"], session)
            elif tool != "start_session":
                self.assertEqual(args["session"], session)

    def test_explicit_session_switch_end_and_reinitialization(self):
        original = os.environ.get("OCTET_CUA_CONFIRM")
        os.environ["OCTET_CUA_CONFIRM"] = "0"
        try:
            computer, client = self._computer()
            computer.call("press_key", {"pid": 17, "window_id": 5, "key": "ENTER"})
            computer.call("start_session", {"session": "review-session"})
            computer.call("press_key", {"pid": 17, "window_id": 5, "key": "ENTER"})
            computer.call("end_session", {"session": "review-session"})
            computer.call("press_key", {"pid": 17, "window_id": 5, "key": "ENTER"})
        finally:
            if original is None:
                os.environ.pop("OCTET_CUA_CONFIRM", None)
            else:
                os.environ["OCTET_CUA_CONFIRM"] = original

        dispatched = [(tool, args) for tool, args in client.calls if tool == "press_key"]
        self.assertEqual([args["session"] for _tool, args in dispatched],
                         [entrypoint.cursor_session(), "review-session", entrypoint.cursor_session()])
        self.assertTrue(any(tool == "end_session" and args.get("session") == "review-session"
                            for tool, args in client.calls))

    def test_cursor_initialization_failure_fails_closed_before_action(self):
        computer, client = self._computer(enabled=False)
        with self.assertRaises(McpError):
            computer.call("click", {"pid": 17, "window_id": 5, "x": 10, "y": 20})
        self.assertFalse(computer._cursor_ready)
        self.assertNotIn("click", [tool for tool, _args in client.calls])

    def test_model_switch_reselects_installed_theme_on_next_tool_boundary(self):
        computer, client = self._computer()
        computer._theme_ids.update({"com.octet.computeruse.anthropic", "com.octet.computeruse.openai"})
        computer.select_model({"host": {"model": "claude-sonnet-4"}})
        computer.client()
        self.assertEqual(computer._selected_theme, "com.octet.computeruse.anthropic")
        computer.select_model({"host": {"model": "gpt-5.6"}})
        computer.client()
        self.assertEqual(computer._selected_theme, "com.octet.computeruse.openai")
        self.assertEqual([args["theme_id"] for tool, args in client.calls
                          if tool == "set_agent_cursor_theme"],
                         ["com.octet.computeruse.anthropic", "com.octet.computeruse.openai"])

    def test_uninstalled_theme_falls_back_without_claiming_personalization(self):
        computer, _ = self._computer()
        computer.select_model({"host": {"model": "gpt-5.6"}})
        computer.client()
        self.assertEqual(computer._selected_theme, "cua.default")
        self.assertTrue(computer._cursor_ready)

    def test_cursor_motion_readback_can_lag_once(self):
        computer, client = self._computer(stale_reads=1)
        computer.client()
        self.assertTrue(computer._cursor_ready)
        self.assertEqual(sum(tool == "get_agent_cursor_state" for tool, _ in client.calls), 2)

    def test_cursor_motion_readback_must_match_before_action(self):
        computer, client = self._computer(motion={"glide_duration_ms": 180.0})
        with self.assertRaises(McpError):
            computer.call("click", {"pid": 17, "window_id": 5, "x": 10, "y": 20})
        self.assertFalse(computer._cursor_ready)
        self.assertNotIn("click", [tool for tool, _args in client.calls])

    def test_cursor_driver_error_is_not_reported_as_ready(self):
        computer, client = self._computer(fail_tool="set_agent_cursor_enabled")
        with self.assertRaises(McpError):
            computer.call("click", {"pid": 17, "window_id": 5, "x": 10, "y": 20})
        self.assertFalse(computer._cursor_ready)
        self.assertNotIn("click", [tool for tool, _args in client.calls])

    def test_move_cursor_is_window_scoped_and_uses_driver_target_shape(self):
        original = os.environ.get("OCTET_CUA_CONFIRM")
        os.environ["OCTET_CUA_CONFIRM"] = "0"
        try:
            computer, client = self._computer()
            computer.call("move_cursor", {"pid": 17, "window_id": 5, "x": 10, "y": 20})
        finally:
            if original is None:
                os.environ.pop("OCTET_CUA_CONFIRM", None)
            else:
                os.environ["OCTET_CUA_CONFIRM"] = original
        _tool, arguments = next((tool, args) for tool, args in client.calls if tool == "move_cursor")
        self.assertEqual(arguments["target"], {"kind": "window", "pid": 17, "window_id": 5})
        self.assertEqual(arguments["scope"], "window")
        self.assertNotIn("pid", arguments)
        self.assertNotIn("window_id", arguments)
        self.assertEqual(arguments["session"], computer._cursor_session)


class ResultFidelityTests(unittest.TestCase):
    """The tool must forward what the driver actually returned.

    Reducing a driver result to a summary line is what made computer use blind
    and untargetable: the screenshot was counted and dropped, and the per-window
    and per-app records were discarded in favour of a count.
    """

    def test_image_blocks_survive_the_summary(self):
        from octet_computer_use.service import summarize_result

        result = {
            "content": [
                {"type": "text", "text": "desktop screenshot 100x100 px"},
                {"type": "image", "data": "QUJD", "mimeType": "image/png"},
            ],
            "isError": False,
        }
        summary = summarize_result(result)
        self.assertEqual(summary["image_count"], 1)
        self.assertEqual(len(summary["images"]), 1)
        self.assertEqual(summary["images"][0]["data"], "QUJD")

    def test_failed_screenshot_publication_is_visible(self):
        computer = ComputerUse(RecordingExtension())  # no artifact publisher
        result = computer._format_result("get_window_state", {
            "content": [{"type": "text", "text": "window captured"},
                        {"type": "image", "data": "QUJD", "mimeType": "image/png"}],
        })
        self.assertTrue(any("could not be delivered" in part.get("text", "")
                            for part in result["content"]))

    def test_structured_payload_survives_the_summary(self):
        from octet_computer_use.service import summarize_result

        result = {
            "content": [{"type": "text", "text": "Found 2 window(s)."}],
            "structuredContent": {
                "windows": [
                    {"window_id": 11, "app_name": "Finder", "pid": 726},
                    {"window_id": 12, "app_name": "Firefox", "pid": 697},
                ]
            },
            "isError": False,
        }
        summary = summarize_result(result)
        windows = summary["structured"]["windows"]
        self.assertEqual([w["window_id"] for w in windows], [11, 12])
        # The count alone is what left the agent unable to target anything.
        self.assertNotIn("window_id", summary["text"])


class WindowResolutionTests(unittest.TestCase):
    """Naming a process must be enough to act on its window."""

    def _client(self, windows):
        class Client:
            def call(self, tool, arguments):
                if tool == "list_windows":
                    return {
                        "structuredContent": {"windows": windows},
                        "content": [{"type": "text", "text": ""}],
                    }
                raise AssertionError(tool)

        return Client()

    def test_explicit_window_id_wins(self):
        from octet_computer_use.service import resolve_window_id

        self.assertEqual(resolve_window_id(self._client([]), 726, window_id=99), 99)

    def test_pid_resolves_to_a_window_id(self):
        from octet_computer_use.service import resolve_window_id

        client = self._client([{"window_id": 44, "pid": 726, "is_on_screen": True}])
        self.assertEqual(resolve_window_id(client, 726), 44)

    def test_frontmost_on_screen_window_is_preferred(self):
        from octet_computer_use.service import resolve_window_id

        client = self._client(
            [
                {"window_id": 5, "pid": 726, "is_on_screen": False, "z_index": 9},
                {"window_id": 6, "pid": 726, "is_on_screen": True, "z_index": 1},
            ]
        )
        self.assertEqual(resolve_window_id(client, 726), 6)

    def test_no_window_yields_none_rather_than_a_guess(self):
        from octet_computer_use.service import resolve_window_id

        self.assertIsNone(resolve_window_id(self._client([]), 726))


class OutputSchemaTests(unittest.TestCase):
    """A tool that returns structured content must declare an output schema.

    The host rejects the result otherwise with -32603. Every republished tool now
    forwards the driver's payload, so every one needs a declared shape.
    """

    def test_every_published_tool_declares_an_output_schema(self):
        from octet_computer_use import entrypoint, service

        for tool, driver_tool, _ in service.PUBLISHED_TOOLS:
            with self.subTest(tool=tool):
                self.assertIsNotNone(
                    entrypoint._output_schema_for(driver_tool),  # noqa: SLF001
                    f"{tool} returns structured content but declares no output schema",
                )

    def test_driver_schema_permits_the_drivers_own_fields(self):
        from octet_computer_use import entrypoint

        schema = entrypoint._output_schema_for("list_windows")  # noqa: SLF001
        # The driver's per-tool fields change between releases, so the declaration
        # must not pin them closed.
        self.assertTrue(schema.get("additionalProperties"))


class TargetingHintTests(unittest.TestCase):
    """The addressing fields must be in the text the agent already reads."""

    def test_hint_names_the_snapshot_and_tokens(self):
        from octet_computer_use.entrypoint import _targeting_hint

        structured = {
            "snapshot_id": "s00000006",
            "elements": [
                {"element_index": 1, "element_token": "s00000006:1", "role": "AXButton", "label": "Delete"},
                {"element_index": 2, "element_token": "s00000006:2", "role": "AXButton", "label": "Clear"},
            ],
        }
        hint = _targeting_hint(structured)
        self.assertIn("s00000006", hint)
        self.assertIn("s00000006:2", hint)
        self.assertIn("Clear", hint)
        # Without this the agent cannot address a control by identity.
        self.assertIn("element_token", hint)

    def test_hint_is_empty_without_elements(self):
        from octet_computer_use.entrypoint import _targeting_hint

        self.assertEqual(_targeting_hint({"snapshot_id": "s1"}), "")

    def test_hint_bounds_the_sample_and_states_the_real_total(self):
        from octet_computer_use.entrypoint import _TARGETING_HINT_ROWS, _targeting_hint

        total = _TARGETING_HINT_ROWS + 12
        structured = {
            "snapshot_id": "s1",
            "elements": [
                {
                    "element_index": index,
                    "element_token": f"s1:{index}",
                    "role": "AXButton",
                    "label": f"Button {index}",
                }
                for index in range(1, total + 1)
            ],
        }
        hint = _targeting_hint(structured)
        rows = [line for line in hint.splitlines() if line.startswith("  token=")]
        self.assertEqual(len(rows), _TARGETING_HINT_ROWS)
        # A truncated sample must never read as the whole table: the model has to
        # be told what it is not seeing and where the rest is.
        self.assertIn(f"... 12 more addressable elements", hint)
        self.assertIn("structured_content.elements", hint)


class ObservationScreenshotTests(unittest.TestCase):
    """Window-state reads default to structure; other tools retain their API."""

    def _forwarded(self, tool, values):
        from octet_computer_use.entrypoint import ComputerUse

        client = FakeClient(read_only=[tool])
        use = ComputerUse.__new__(ComputerUse)
        use._client = client
        use._lock = __import__("threading").Lock()
        use._app_daemon = False
        use._cursor_ready = True
        use._cursor_session = None
        use._extension = RecordingExtension()
        use.call(tool, {**values, "pid": 1, "window_id": 1})
        return client.calls[-1][1]

    def test_screenshot_is_opt_in_on_a_window_read(self):
        forwarded = self._forwarded("get_window_state", {})
        self.assertIs(forwarded["include_screenshot"], False)

    def test_an_explicit_screenshot_request_is_honored(self):
        forwarded = self._forwarded("get_window_state", {"include_screenshot": True})
        self.assertIs(forwarded["include_screenshot"], True)

    def test_other_observations_keep_their_reviewed_arguments(self):
        for tool in ("get_desktop_state", "list_windows", "list_apps"):
            with self.subTest(tool=tool):
                forwarded = self._forwarded(tool, {})
                self.assertNotIn("include_screenshot", forwarded)

    def test_an_effectful_tool_is_not_given_a_default(self):
        forwarded = self._forwarded("click", {})
        self.assertNotIn("include_screenshot", forwarded)


MACOS_BUNDLE_ONLY = "macOS .app bundle layout and Info.plist resolution are darwin-only"


class DesktopHostTests(unittest.TestCase):
    """macOS requires the selected host unless direct mode is explicit.

    The host owns the OS permission identity and the GUI main thread needed
    for the agent cursor. A missing or ungranted required host fails closed.

    Bundle-layout tests are macOS-only: the driver resolves an ``.app`` host
    through ``DESKTOP_APP_CANDIDATES[platform.system()]`` and reads
    ``CFBundleExecutable`` with ``plutil``, so off darwin the correct answer is
    ``None`` and only the darwin behaviour is meaningful to pin.
    """

    def test_desktop_app_is_found_only_when_installed(self):
        from octet_computer_use import driver

        with tempfile.TemporaryDirectory() as directory:
            present = Path(directory) / "CuaDriver.app"
            present.mkdir()
            os.environ["OCTET_CUA_DESKTOP_APP"] = str(present)
            try:
                self.assertEqual(driver.desktop_app(), present)
                # No embedded binary yet: not usable as a host.
                self.assertIsNone(driver.desktop_app_binary())
            finally:
                os.environ.pop("OCTET_CUA_DESKTOP_APP", None)

        os.environ["OCTET_CUA_DESKTOP_APP"] = str(Path("/nonexistent/CuaDriver.app"))
        try:
            self.assertIsNone(driver.desktop_app())
        finally:
            os.environ.pop("OCTET_CUA_DESKTOP_APP", None)

    @unittest.skipUnless(sys.platform == "darwin", MACOS_BUNDLE_ONLY)
    def test_bundle_executable_is_read_from_its_own_info_plist(self):
        # Cua ships two official macOS bundles whose executable name differs:
        # the release bundle declares ``cua-driver`` and the source-built local
        # bundle declares ``cua-driver-local``. Octet must resolve whichever is
        # installed instead of assuming a single layout.
        from octet_computer_use import driver

        original = driver.DESKTOP_APP_CANDIDATES
        with tempfile.TemporaryDirectory() as directory:
            for declared, bundle in (("cua-driver", "CuaDriver.app"),
                                     ("cua-driver-local", "CuaDriverLocal.app")):
                host = Path(directory) / bundle
                macos = host / "Contents" / "MacOS"
                macos.mkdir(parents=True)
                executable = macos / declared
                executable.write_text("")
                (host / "Contents" / "Info.plist").write_text(
                    '<?xml version="1.0" encoding="UTF-8"?>\n'
                    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
                    '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
                    '<plist version="1.0"><dict>'
                    f"<key>CFBundleExecutable</key><string>{declared}</string>"
                    "</dict></plist>\n"
                )
                try:
                    driver.DESKTOP_APP_CANDIDATES = {"darwin": (str(host),)}
                    self.assertEqual(driver.desktop_app(), host)
                    self.assertEqual(driver.desktop_app_binary(), executable)
                finally:
                    driver.DESKTOP_APP_CANDIDATES = original

    @unittest.skipUnless(sys.platform == "darwin", MACOS_BUNDLE_ONLY)
    def test_bundle_executable_falls_back_to_known_names(self):
        # An unreadable or absent Info.plist must not hide an installed host.
        from octet_computer_use import driver

        original = driver.DESKTOP_APP_CANDIDATES
        with tempfile.TemporaryDirectory() as directory:
            host = Path(directory) / "CuaDriver.app"
            macos = host / "Contents" / "MacOS"
            macos.mkdir(parents=True)
            executable = macos / "cua-driver"
            executable.write_text("")
            try:
                driver.DESKTOP_APP_CANDIDATES = {"darwin": (str(host),)}
                self.assertEqual(driver.desktop_app_binary(), executable)
            finally:
                driver.DESKTOP_APP_CANDIDATES = original

    @unittest.skipUnless(sys.platform == "darwin", MACOS_BUNDLE_ONLY)
    def test_release_bundle_is_preferred_over_the_local_build(self):
        # A stock install must win over a source build, so the notarized
        # release identity is used whenever both bundles are present.
        from octet_computer_use import driver

        original = driver.DESKTOP_APP_CANDIDATES
        with tempfile.TemporaryDirectory() as directory:
            release = Path(directory) / "CuaDriver.app"
            local = Path(directory) / "CuaDriverLocal.app"
            release.mkdir()
            local.mkdir()
            try:
                driver.DESKTOP_APP_CANDIDATES = {"darwin": (str(release), str(local))}
                self.assertEqual(driver.desktop_app(), release)
                driver.DESKTOP_APP_CANDIDATES = {"darwin": (str(local),)}
                self.assertEqual(driver.desktop_app(), local)
            finally:
                driver.DESKTOP_APP_CANDIDATES = original

    def test_shipped_candidate_is_only_the_signed_cua_app(self):
        from octet_computer_use import driver

        self.assertEqual(driver.DESKTOP_APP_CANDIDATES["darwin"], ("/Applications/CuaDriver.app",))

    def test_unusable_host_is_unavailable_when_cursor_is_required(self):
        from unittest import mock
        from octet_computer_use import driver

        original_env = os.environ.get("OCTET_CUA_DESKTOP_HOST")
        try:
            os.environ.pop("OCTET_CUA_DESKTOP_HOST", None)
            with mock.patch.object(driver.platform, "system", return_value="Darwin"), \
                 mock.patch.object(driver, "desktop_app_binary", return_value=None), \
                 mock.patch.object(driver, "installed_binary") as direct_binary:
                computer = ComputerUse(RecordingExtension())
                with self.assertRaises(McpError) as caught:
                    computer.client()
                self.assertIn("refusing to fall back", str(caught.exception))
                direct_binary.assert_not_called()
        finally:
            if original_env is None:
                os.environ.pop("OCTET_CUA_DESKTOP_HOST", None)
            else:
                os.environ["OCTET_CUA_DESKTOP_HOST"] = original_env

    def test_direct_runtime_is_used_only_after_explicit_opt_out(self):
        from unittest import mock
        from octet_computer_use import driver
        from octet_computer_use.driver_client import DriverClient

        original_env = os.environ.get("OCTET_CUA_DESKTOP_HOST")
        original_installed = driver.installed_binary
        original_desktop = driver.desktop_app_binary
        original_usable = driver.desktop_app_usable
        try:
            os.environ["OCTET_CUA_DESKTOP_HOST"] = "0"
            driver.installed_binary = lambda paths: Path("/opt/cua-driver")
            driver.desktop_app_binary = lambda app=None: Path("/Applications/CuaDriver.app")
            driver.desktop_app_usable = lambda binary=None: False
            captured = {}

            class _FakeClient(DriverClient):
                def __init__(self, binary, *, app_daemon=False, **kwargs):
                    captured["binary"] = binary
                    captured["app_daemon"] = app_daemon
                    self._started = True

                def start(self, timeout=0.0):
                    self._started = True

            with mock.patch.object(driver.platform, "system", return_value="Darwin"), \
                 mock.patch.object(entrypoint, "DriverClient", _FakeClient):
                ComputerUse(RecordingExtension()).client()
            self.assertFalse(captured["app_daemon"])
            self.assertEqual(captured["binary"], Path("/opt/cua-driver"))
        finally:
            driver.installed_binary = original_installed
            driver.desktop_app_binary = original_desktop
            driver.desktop_app_usable = original_usable
            if original_env is None:
                os.environ.pop("OCTET_CUA_DESKTOP_HOST", None)
            else:
                os.environ["OCTET_CUA_DESKTOP_HOST"] = original_env

    def test_ungranted_host_is_started_once_then_rejected(self):
        # A host that is installed but not running is the common case, not a
        # failure. It gets one bounded launch attempt; if it still cannot prove
        # its grant it is rejected so the direct runtime takes over.
        from octet_computer_use import driver

        original_status = driver._permission_status
        original_start = driver.start_desktop_app
        original_sleep = driver.time.sleep
        original_attempts = driver.HOST_START_ATTEMPTS
        original_perms = driver.desktop_app_permissions
        try:
            calls = []
            driver.start_desktop_app = lambda app=None: calls.append(1) or True
            driver.time.sleep = lambda seconds: None
            driver.HOST_START_ATTEMPTS = 2
            driver.desktop_app_permissions = lambda binary=None: "denied"
            driver._permission_status = lambda binary: "unknown"
            self.assertFalse(driver.desktop_app_usable(Path("/Applications/OctetComputerUse.app")))
            self.assertEqual(len(calls), 1, "the host must be started at most once")

            # A host that grants after starting must be adopted, so the cursor
            # becomes available without the user restarting octet.
            driver.desktop_app_permissions = lambda binary=None: "granted"
            self.assertTrue(driver.desktop_app_usable(Path("/Applications/OctetComputerUse.app")))
        finally:
            driver._permission_status = original_status
            driver.start_desktop_app = original_start
            driver.time.sleep = original_sleep
            driver.HOST_START_ATTEMPTS = original_attempts
            driver.desktop_app_permissions = original_perms

    @unittest.skipUnless(sys.platform == "darwin", MACOS_BUNDLE_ONLY)
    def test_octet_host_resolves_its_driver_not_its_own_executable(self):
        # Octet's host app declares CFBundleExecutable as the host itself and
        # ships the driver beside it. Resolving the declared name would hand the
        # client an app that cannot speak MCP, so the driver must be preferred.
        from octet_computer_use import driver

        original = driver.DESKTOP_APP_CANDIDATES
        with tempfile.TemporaryDirectory() as directory:
            host = Path(directory) / "OctetComputerUse.app"
            macos = host / "Contents" / "MacOS"
            macos.mkdir(parents=True)
            host_binary = macos / "OctetComputerUseHost"
            host_binary.write_text("")
            driver_binary = macos / "cua-driver"
            driver_binary.write_text("")
            (host / "Contents" / "Info.plist").write_text(
                '<?xml version="1.0" encoding="UTF-8"?>\n'
                '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
                '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
                '<plist version="1.0"><dict>'
                "<key>CFBundleExecutable</key><string>OctetComputerUseHost</string>"
                "<key>CFBundleIdentifier</key><string>com.octet.computeruse</string>"
                "</dict></plist>\n"
            )
            try:
                driver.DESKTOP_APP_CANDIDATES = {"darwin": (str(host),)}
                self.assertEqual(driver.desktop_app(), host)
                self.assertEqual(driver.desktop_app_binary(), driver_binary)
            finally:
                driver.DESKTOP_APP_CANDIDATES = original

    def test_desktop_host_use_can_be_disabled(self):
        # An explicit opt-out must win over an installed host, so a user on an
        # OS where the host is broken can force the direct runtime.
        from octet_computer_use.entrypoint import ComputerUse

        original = os.environ.get("OCTET_CUA_DESKTOP_HOST")
        try:
            os.environ["OCTET_CUA_DESKTOP_HOST"] = "0"
            self.assertFalse(ComputerUse.use_desktop_host())
            os.environ["OCTET_CUA_DESKTOP_HOST"] = "1"
            self.assertTrue(ComputerUse.use_desktop_host())
        finally:
            if original is None:
                os.environ.pop("OCTET_CUA_DESKTOP_HOST", None)
            else:
                os.environ["OCTET_CUA_DESKTOP_HOST"] = original

    def test_driver_client_records_direct_and_host_transport_modes(self):
        from octet_computer_use.driver_client import DriverClient

        direct = DriverClient("cua-driver")
        self.assertFalse(direct._app_daemon)
        host = DriverClient("cua-driver", app_daemon=True)
        self.assertTrue(host._app_daemon)

    def test_cursor_session_is_unique_per_transport(self):
        # A session name belongs permanently to the transport that claimed it
        # first, so a shared name is refused on every later launch. Each
        # transport must therefore take its own name.
        import os

        from octet_computer_use.entrypoint import CURSOR_SESSION_PREFIX, cursor_session

        name = cursor_session()
        self.assertTrue(name.startswith(CURSOR_SESSION_PREFIX + "-"))
        self.assertNotEqual(name, CURSOR_SESSION_PREFIX)
        self.assertEqual(name, cursor_session())
        self.assertIn(str(os.getpid()), name)

    def test_cursor_motion_is_short_straight_and_fades_later(self):
        from octet_computer_use.entrypoint import CURSOR_MOTION

        self.assertEqual(CURSOR_MOTION["arc_size"], 0.0)
        self.assertEqual(CURSOR_MOTION["turn_radius"], 1.0)
        self.assertEqual(CURSOR_MOTION["glide_duration_ms"], 80.0)
        self.assertEqual(CURSOR_MOTION["dwell_after_click_ms"], 40.0)
        self.assertEqual(CURSOR_MOTION["spring"], 1.0)
        self.assertEqual(CURSOR_MOTION["idle_hide_ms"], 5000.0)

    def test_daemon_socket_is_overridable(self):
        from octet_computer_use.driver_client import daemon_socket

        os.environ["OCTET_CUA_DAEMON_SOCKET"] = "/tmp/custom.sock"
        try:
            self.assertEqual(str(daemon_socket()), "/tmp/custom.sock")
        finally:
            os.environ.pop("OCTET_CUA_DAEMON_SOCKET", None)


if __name__ == "__main__":
    unittest.main()
