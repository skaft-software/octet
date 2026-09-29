"""Behavioural tests for the octet computer-use bundle."""

from __future__ import annotations

import base64
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
    from . import live_smoke
    from .helpers import FakeClient, RecordingExtension
except ImportError:  # pragma: no cover - exercised by the flat discovery mode
    import live_smoke
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

        from octet_computer_use import driver_client
        from octet_computer_use.driver_client import SESSION_ENVIRONMENT, _child_environment

        ambient = {"DISPLAY": ":0", "PATH": "/usr/bin", "HOME": "/home/u",
                   "OPENAI_API_KEY": "secret", "AWS_SECRET_ACCESS_KEY": "secret"}
        for system in ("Darwin", "Windows"):
            with self.subTest(system=system), mock.patch.dict(os.environ, ambient), \
                    mock.patch.object(driver_client.platform, "system", return_value=system):
                environment = _child_environment()
            self.assertEqual(environment.get("DISPLAY"), ":0")
            self.assertNotIn("OPENAI_API_KEY", environment)
            self.assertNotIn("AWS_SECRET_ACCESS_KEY", environment)
            for name in environment:
                self.assertIn(name, SESSION_ENVIRONMENT)

    def test_linux_child_gets_the_launch_baseline_and_nothing_secret(self):
        # Linux launch_app spawns through the driver's own environment, so the
        # launched app needs the same PATH/HOME/locale octet's tools get.
        import os
        from unittest import mock

        from octet_computer_use import driver_client
        from octet_computer_use.driver_client import (
            LINUX_LAUNCH_ENVIRONMENT, SESSION_ENVIRONMENT, _child_environment)

        ambient = {"DISPLAY": ":0", "PATH": "/usr/bin:/home/u/.local/share/omarchy/bin",
                   "HOME": "/home/u", "LANG": "en_US.UTF-8",
                   "HYPRLAND_INSTANCE_SIGNATURE": "abc_123", "XDG_CURRENT_DESKTOP": "Hyprland",
                   "OPENAI_API_KEY": "secret", "LD_PRELOAD": "/tmp/evil.so"}
        with mock.patch.dict(os.environ, ambient, clear=True), \
                mock.patch.object(driver_client.platform, "system", return_value="Linux"):
            environment = _child_environment()
        self.assertEqual(environment["PATH"], ambient["PATH"])
        self.assertEqual(environment["HOME"], "/home/u")
        self.assertEqual(environment["LANG"], "en_US.UTF-8")
        self.assertEqual(environment["HYPRLAND_INSTANCE_SIGNATURE"], "abc_123")
        self.assertEqual(environment["XDG_CURRENT_DESKTOP"], "Hyprland")
        self.assertNotIn("OPENAI_API_KEY", environment)
        self.assertNotIn("LD_PRELOAD", environment)
        # No Wayland socket, so the native Wayland backend stays off.
        self.assertNotIn(driver_client.WAYLAND_BACKEND_VARIABLE, environment)
        allowed = set(SESSION_ENVIRONMENT) | set(LINUX_LAUNCH_ENVIRONMENT)
        for name in environment:
            self.assertIn(name, allowed)

    def test_wayland_sessions_enable_the_native_backend_only_on_linux(self):
        # Hyprland (Omarchy) is pure Wayland: without the native backend only
        # XWayland windows would be visible to the driver.
        import os
        from unittest import mock

        from octet_computer_use import driver_client
        from octet_computer_use.driver_client import WAYLAND_BACKEND_VARIABLE, _child_environment

        wayland = {"WAYLAND_DISPLAY": "wayland-1", "XDG_RUNTIME_DIR": "/run/user/1000"}
        with mock.patch.dict(os.environ, wayland, clear=True):
            with mock.patch.object(driver_client.platform, "system", return_value="Linux"):
                self.assertEqual(_child_environment()[WAYLAND_BACKEND_VARIABLE], "1")
            with mock.patch.object(driver_client.platform, "system", return_value="Darwin"):
                self.assertNotIn(WAYLAND_BACKEND_VARIABLE, _child_environment())
        with mock.patch.dict(os.environ, {"DISPLAY": ":0"}, clear=True), \
                mock.patch.object(driver_client.platform, "system", return_value="Linux"):
            self.assertNotIn(WAYLAND_BACKEND_VARIABLE, _child_environment())

    def test_explicit_overrides_win_over_inherited_names(self):
        from octet_computer_use.driver_client import _child_environment

        environment = _child_environment({"DISPLAY": ":9"})
        self.assertEqual(environment["DISPLAY"], ":9")


class DriverTransportEncodingTests(unittest.TestCase):
    """The MCP stdio transport is UTF-8 with LF framing on every host.

    On Windows the default text encoding is cp1252, so any byte undefined
    there (window titles, pip banners, lone 0x90) used to kill the reader
    and report the driver as closed. These tests pin the binary-pipe
    contract without needing a live driver.
    """

    def test_frame_is_utf8_with_a_single_lf(self):
        from octet_computer_use.driver_client import _frame

        raw = _frame({"text": "— curly “quotes” —"})
        self.assertTrue(raw.endswith(b"\n"))
        self.assertFalse(raw.endswith(b"\r\n"))
        self.assertTrue(raw.decode("utf-8").endswith('"}\n'))

    def test_reader_decodes_utf8_and_survives_undefined_cp1252_bytes(self):
        import io
        import queue

        from octet_computer_use.driver_client import DriverClient

        stream = io.BytesIO(
            '{"jsonrpc":"2.0","id":1,"result":{}}\n'.encode("utf-8")
            + "—\n".encode("utf-8")
            + b"\x90\n"
            + '{"jsonrpc":"2.0","id":2,"result":{}}\n'.encode("utf-8")
        )
        sink: queue.Queue = queue.Queue()
        DriverClient._read_lines(stream, sink)
        got = [sink.get(timeout=5) for _ in range(5)]
        self.assertTrue(got[0].startswith('{"jsonrpc"'))
        self.assertIn("—", got[1])
        # errors="replace": the lone 0x90 becomes U+FFFD and the stream
        # continues instead of reporting EOF.
        self.assertIn("�", got[2])
        self.assertTrue(got[3].startswith('{"jsonrpc"'))
        self.assertIsNone(got[4])

    def test_run_decodes_subprocess_output_as_utf8(self):
        from unittest import mock

        from octet_computer_use import driver as driver_module

        with mock.patch("subprocess.run") as run:
            driver_module._run(["octet", "--version"])
        _, kwargs = run.call_args
        self.assertEqual(kwargs.get("encoding"), "utf-8")
        self.assertEqual(kwargs.get("errors"), "replace")

    def test_cursor_theme_probes_decode_as_utf8(self):
        from unittest import mock

        from octet_computer_use import cursor_theme

        # Only a driver shipped with Cua's cursor-theme compiler is probed
        # through a subprocess; a wheel driver's store is read directly.
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(cursor_theme.subprocess, "run") as run:
            binary = Path(directory) / "cua-driver"
            binary.write_text("")
            for sidecar in ("cua-cursor-theme", "cua-cursor-theme.exe"):
                (Path(directory) / sidecar).write_text("")
            run.return_value = mock.Mock(stdout="[]", stderr="", returncode=0)
            cursor_theme.installed_theme_ids(binary)
        self.assertTrue(run.call_args_list)
        for call in run.call_args_list:
            self.assertEqual(call.kwargs.get("encoding"), "utf-8")
            self.assertEqual(call.kwargs.get("errors"), "replace")


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

    def setUp(self):
        from unittest import mock
        from octet_computer_use import driver

        patcher = mock.patch.object(driver.platform, "system", return_value="Darwin")
        patcher.start()
        self.addCleanup(patcher.stop)

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


class LinuxSessionProbeTests(unittest.TestCase):
    """Linux readiness is a reachable display session; nothing is granted."""

    # The exact structured payload cua-driver 0.30 returns on Linux.
    HYPRLAND = {"atspi": True, "dbus_session_bus_address": "unix:path=/run/user/1000/bus",
                "wayland": True, "wayland_enabled": True, "x11": True, "xsend_event": True}
    HEADLESS = {"atspi": False, "dbus_session_bus_address": None, "wayland": False,
                "wayland_enabled": False, "x11": False, "xsend_event": False}

    def setUp(self):
        from unittest import mock
        from octet_computer_use import driver

        patcher = mock.patch.object(driver.platform, "system", return_value="Linux")
        patcher.start()
        self.addCleanup(patcher.stop)

    def _state(self, structured, raises=None, prompt=False):
        from octet_computer_use.driver import permission_state

        client = FakeClient(result={"structuredContent": structured}, raises=raises)
        return permission_state(client, prompt=prompt), client

    def test_hyprland_with_xwayland_is_ready(self):
        state, client = self._state(self.HYPRLAND, prompt=True)
        self.assertEqual(state["permissions"], "granted")
        self.assertEqual(state["display_server"], "wayland+x11")
        self.assertTrue(state["atspi"])
        # Linux never prompts, even from setup.
        self.assertEqual(client.calls, [("check_permissions", {"prompt": False})])
        self.assertNotIn("accessibility", state)
        self.assertNotIn("screen_recording", state)

    def test_pure_wayland_without_atspi_is_ready_but_says_so(self):
        state, _ = self._state({**self.HYPRLAND, "x11": False, "atspi": False})
        self.assertEqual(state["permissions"], "granted")
        self.assertEqual(state["display_server"], "wayland")
        self.assertIn("AT-SPI unavailable", state["detail"])

    def test_wayland_with_the_backend_off_and_no_xwayland_is_denied(self):
        state, _ = self._state({**self.HYPRLAND, "x11": False, "wayland_enabled": False})
        self.assertEqual(state["permissions"], "denied")
        self.assertIn("native Wayland backend is off", state["detail"])

    def test_x11_session_is_ready(self):
        state, _ = self._state({**self.HEADLESS, "x11": True, "xsend_event": True})
        self.assertEqual(state["permissions"], "granted")
        self.assertEqual(state["display_server"], "x11")

    def test_no_display_is_denied_and_holds_actions(self):
        state, _ = self._state(self.HEADLESS)
        self.assertEqual(state["permissions"], "denied")
        self.assertIsNone(state["display_server"])
        self.assertIn("inside your graphical session", state["detail"])

    def test_unreadable_or_failed_probe_is_unknown(self):
        self.assertEqual(self._state({})[0]["permissions"], "unknown")
        state, _ = self._state(self.HYPRLAND, raises=McpError("driver gone"))
        self.assertEqual(state["permissions"], "unknown")

    def test_cli_permission_status_is_macos_only(self):
        from unittest import mock
        from octet_computer_use import driver

        with mock.patch.object(driver, "_run") as run:
            self.assertEqual(driver._permission_status(Path("/opt/cua-driver")), "unknown")
            run.assert_not_called()

    def test_status_reports_the_session_without_null_grants(self):
        from unittest import mock
        from octet_computer_use import driver

        computer = ComputerUse(RecordingExtension())
        computer._client = FakeClient(result={"structuredContent": self.HYPRLAND})
        computer._client.started = True
        health = driver.Health(installed=True, version="0.30.2", permissions="unknown",
                               doctor_ok=True, detail="cua-driver 0.30.2")
        with mock.patch.object(driver, "health", return_value=health):
            report = computer.status()
        self.assertEqual(report["platform"], "linux")
        self.assertEqual(report["permissions"], "granted")
        self.assertEqual(report["display_server"], "wayland+x11")
        self.assertNotIn("accessibility", report)
        self.assertNotIn("screen_recording", report)
        text = _render_status(report)
        self.assertIn("Linux desktop session: Wayland (native) and XWayland reachable", text)
        self.assertNotIn("macOS", text)

    def test_rendered_linux_status_names_the_fix(self):
        denied = _render_status({"installed": True, "version": "0.30.2", "doctor_ok": True,
                                 "runtime": "direct", "platform": "linux",
                                 "permissions": "denied",
                                 "permission_detail": "no display session is reachable"})
        self.assertIn("no display session is reachable", denied)
        self.assertIn("WAYLAND_DISPLAY", denied)
        self.assertNotIn("macOS", denied)
        self.assertNotIn("Screen Recording", denied)
        no_atspi = _render_status({"installed": True, "runtime": "direct", "platform": "linux",
                                   "permissions": "granted", "atspi": False,
                                   "permission_detail": "Wayland (native) reachable"})
        self.assertIn("at-spi2-core", no_atspi)

    def test_status_row_names_the_linux_blocker(self):
        extension = StatusRowTests._RecordingStatus()
        computer = ComputerUse(extension)
        computer.status = lambda **_: {"installed": True, "runtime": "direct",
                                       "platform": "linux", "permissions": "denied"}
        computer.publish_status()
        self.assertEqual(extension.statuses[-1]["label"], "computer use · needs a desktop session")
        computer.status = lambda **_: {"installed": True, "runtime": "direct",
                                       "platform": "linux", "permissions": "granted"}
        computer.publish_status()
        self.assertEqual(extension.statuses[-1]["label"], "computer use · ready")

    def test_missing_display_blocks_effectful_actions_with_a_linux_message(self):
        from unittest import mock

        computer = ComputerUse(RecordingExtension())
        client = FakeClient(effectful=["click"], result={"structuredContent": self.HEADLESS})
        computer._client = client
        with mock.patch.object(entrypoint, "confirmations_enabled", return_value=True):
            result = computer.call("click", {"pid": 1, "window_id": 2, "x": 3, "y": 4})
        self.assertTrue(result["is_error"])
        self.assertIn("Linux display session", result["content"][0]["text"])
        self.assertNotIn(("click", mock.ANY), client.calls)


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
        from unittest import mock

        # macOS wording is pinned: the label names the missing grant.
        with mock.patch.object(entrypoint.platform, "system", return_value="Darwin"):
            statuses = self._publish(
                {"installed": True, "permissions": "denied", "screen_recording": False}
            )
        self.assertEqual(statuses[0]["state"], "pending")
        self.assertIn("Screen Recording", statuses[0]["label"])

    def test_pending_label_names_windows_limits_on_windows(self):
        from unittest import mock

        with mock.patch.object(entrypoint.platform, "system", return_value="Windows"):
            statuses = self._publish(
                {"installed": True, "permissions": "denied", "screen_recording": False}
            )
        self.assertEqual(statuses[0]["state"], "pending")
        self.assertIn("non-elevated target", statuses[0]["label"])
        self.assertNotIn("Screen Recording", statuses[0]["label"])
        self.assertNotIn("macOS", statuses[0]["label"])

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
        from unittest import mock

        # macOS wording is pinned so the contract holds on every host.
        with mock.patch.object(entrypoint.platform, "system", return_value="Darwin"):
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

    def test_rendered_status_names_windows_limits_on_windows(self):
        from unittest import mock

        with mock.patch.object(entrypoint.platform, "system", return_value="Windows"):
            denied = _render_status(
                {
                    "installed": True,
                    "version": "0.29.1",
                    "doctor_ok": True,
                    "permissions": "denied",
                    "screen_recording": False,
                    "permission_detail": "still needs: Screen Recording",
                }
            )
            granted = _render_status(
                {
                    "installed": True,
                    "version": "0.29.1",
                    "doctor_ok": True,
                    "permissions": "granted",
                    "runtime": "direct",
                }
            )
            unavailable = _render_status({"installed": True, "runtime": "unavailable"})
        self.assertIn("Windows limits", denied)
        self.assertIn("secure desktop", denied)
        self.assertNotIn("Accessibility", denied)
        self.assertIn("no separate grant", granted)
        self.assertNotIn("Accessibility", granted)
        self.assertIn("non-elevated target", unavailable)
        self.assertNotIn("Grant Accessibility", unavailable)

    def test_status_reports_the_live_runtime(self):
        # The permission fix differs per runtime, so a user must be able to see
        # which one is live instead of guessing. macOS wording is pinned so
        # the contract holds on every host.
        from unittest import mock

        with mock.patch.object(entrypoint.platform, "system", return_value="Darwin"):
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
        from unittest import mock

        # macOS wording is pinned so the contract holds on every host.
        with mock.patch.object(entrypoint.platform, "system", return_value="Darwin"):
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

    def test_setup_reports_provisioning_failures_instead_of_raising(self):
        from unittest import mock
        from octet_computer_use import driver

        extension, computer = entrypoint.create_extension()
        failure = driver.ProvisionError(
            "cua-driver needs Python 3.10 or newer, but octet's computer-use "
            "extension runs on Python 3.9 (/usr/bin/python3)")
        with mock.patch.object(computer, "provision", side_effect=failure):
            for result in (extension._commands["computer-use"].handler(["setup"], {}),
                           extension._tools["computer_use_setup"].handler({}, {})):
                self.assertTrue(result["is_error"])
                text = result["content"][0]["text"]
                self.assertIn("Cua Driver setup failed", text)
                self.assertIn("needs Python 3.10 or newer", text)

    def test_linux_setup_installs_themes_and_the_gnome_helper(self):
        # The Linux wheel has no cursor-theme compiler, so setup must install
        # the bundled artifacts itself rather than fail.
        from unittest import mock
        from octet_computer_use import cursor_theme, gnome_helper

        extension, computer = entrypoint.create_extension()
        with mock.patch.object(computer, "provision", return_value={
                "provisioned": True, "binary": "/tmp/cua-driver", "version": "0.30.2"}), \
             mock.patch.object(computer, "publish_status", return_value={
                "installed": True, "permissions": "granted", "runtime": "direct",
                "platform": "linux", "permission_detail": "Wayland (native) reachable"}), \
             mock.patch.object(entrypoint, "_setup_jev", return_value={"jev_setup": "skipped"}), \
             mock.patch.object(cursor_theme, "install_bundled_themes", return_value=24) as install, \
             mock.patch.object(gnome_helper, "is_gnome_wayland", return_value=True), \
             mock.patch.object(gnome_helper, "install", return_value={
                "gnome_helper": "restart-required",
                "gnome_helper_detail": "log out and back in once"}) as helper:
            result = extension._commands["computer-use"].handler(["setup"], {})
        install.assert_called_once_with(Path("/tmp/cua-driver"))
        helper.assert_called_once_with()
        self.assertFalse(result.get("is_error"))
        self.assertEqual(result["structured_content"]["cursor_themes_installed"], 24)
        self.assertEqual(result["structured_content"]["gnome_helper"], "restart-required")

    def test_installer_uses_only_bundled_artifacts_and_reports_failure(self):
        from unittest import mock
        from octet_computer_use import cursor_theme

        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "cua-driver"
            binary.write_text("")
            (Path(directory) / "cua-cursor-theme").write_text("")
            with mock.patch.object(cursor_theme.subprocess, "run") as run:
                run.return_value.returncode = 0
                self.assertEqual(cursor_theme.install_bundled_themes(binary), 24)
                self.assertEqual(run.call_count, 24)
                for args, _ in run.call_args_list:
                    command = args[0]
                    self.assertEqual(command[:3], [str(binary), "cursor-theme", "install"])
                    self.assertEqual(Path(command[3]).parent, cursor_theme.THEMES)
                run.return_value.returncode = 1
                run.return_value.stderr = "rejected"
                with self.assertRaisesRegex(RuntimeError, "rejected"):
                    cursor_theme.install_bundled_themes(binary)

    def test_without_the_compiler_themes_go_straight_into_the_driver_store(self):
        # Mirrors Cua's own install: <store>/<id>.cua-theme, atomically, with
        # the driver re-validating on load. No subprocess is involved.
        from unittest import mock
        from octet_computer_use import cursor_theme

        with tempfile.TemporaryDirectory() as directory:
            store = Path(directory) / "data" / "cua-driver" / "cursor-themes"
            env = {"XDG_DATA_HOME": str(Path(directory) / "data"), "HOME": directory}
            with mock.patch.dict(os.environ, env, clear=True), \
                 mock.patch.object(cursor_theme.platform, "system", return_value="Linux"), \
                 mock.patch.object(cursor_theme.subprocess, "run") as run:
                binary = Path(directory) / "cua-driver"
                self.assertEqual(cursor_theme.theme_store_root(), store)
                self.assertEqual(cursor_theme.installed_theme_ids(binary), {"cua.default"})
                self.assertEqual(cursor_theme.install_bundled_themes(binary), 24)
                run.assert_not_called()
                entry = cursor_theme.PALETTE["anthropic"]
                target = store / (entry["id"] + ".cua-theme")
                self.assertEqual(target.read_bytes(),
                                 (cursor_theme.THEMES / "anthropic.cua-theme").read_bytes())
                ids = cursor_theme.installed_theme_ids(binary)
                self.assertEqual(ids, {"cua.default"} | {e["id"] for e in cursor_theme.PALETTE.values()})
                # Idempotent, and an outdated copy is replaced.
                target.write_bytes(b"stale")
                cursor_theme.install_bundled_themes(binary)
                self.assertEqual(target.read_bytes()[:8], b"CUATHEM3")
                self.assertEqual([p.name for p in store.iterdir() if p.name.startswith(".")], [])

    def test_store_falls_back_to_home_and_refuses_a_symlinked_store(self):
        from unittest import mock
        from octet_computer_use import cursor_theme

        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.dict(os.environ, {"HOME": directory}, clear=True), \
                 mock.patch.object(cursor_theme.platform, "system", return_value="Linux"):
                root = cursor_theme.theme_store_root()
                self.assertEqual(root, Path(directory) / ".local/share/cua-driver/cursor-themes")
                root.parent.mkdir(parents=True)
                elsewhere = Path(directory) / "elsewhere"
                elsewhere.mkdir()
                root.symlink_to(elsewhere)
                with self.assertRaisesRegex(RuntimeError, "symlink"):
                    cursor_theme.install_bundled_themes(Path(directory) / "cua-driver")
                self.assertEqual(list(elsewhere.iterdir()), [])


class LinuxCursorTests(unittest.TestCase):
    """Linux's direct runtime draws the model-colored cursor, best-effort."""

    def setUp(self):
        from unittest import mock
        from octet_computer_use import driver

        for target, value in ((driver.platform, "Linux"),):
            patcher = mock.patch.object(target, "system", return_value=value)
            patcher.start()
            self.addCleanup(patcher.stop)
        confirm = mock.patch.dict(os.environ, {"OCTET_CUA_CONFIRM": "0"})
        confirm.start()
        self.addCleanup(confirm.stop)

    def _start(self, client, *, theme_ids, installed=None, gnome=False):
        """Run ComputerUse.client() for a Linux direct runtime with fakes."""

        from unittest import mock
        from octet_computer_use import cursor_theme, driver, gnome_helper

        computer = ComputerUse(RecordingExtension())
        computer.select_model({"host": {"model": "claude-sonnet-4"}})
        inventories = [set(theme_ids), set(installed if installed is not None else theme_ids)]
        with mock.patch.object(driver, "desktop_app_binary", return_value=None), \
             mock.patch.object(driver, "installed_binary", return_value=Path("/opt/cua-driver")), \
             mock.patch.object(entrypoint, "DriverClient", return_value=client), \
             mock.patch.object(cursor_theme, "installed_theme_ids",
                               side_effect=lambda binary: inventories.pop(0) if len(inventories) > 1
                               else inventories[0]), \
             mock.patch.object(cursor_theme, "install_bundled_themes", return_value=24) as install, \
             mock.patch.object(gnome_helper, "is_gnome_wayland", return_value=gnome), \
             mock.patch.object(gnome_helper, "set_theme_color", return_value=True) as color:
            computer.client()
        return computer, install, color

    def _client(self, **kwargs):
        client = CursorSessionTests._CursorClient(**kwargs)
        client.start = lambda **_: None
        return client

    def test_direct_runtime_shows_the_model_colored_theme(self):
        every = {e["id"] for e in entrypoint.cursor_theme.PALETTE.values()} | {"cua.default"}
        client = self._client()
        computer, install, color = self._start(client, theme_ids=every)
        install.assert_not_called()
        self.assertFalse(computer._app_daemon)
        self.assertTrue(computer._cursor_ready)
        self.assertEqual(computer._selected_theme, "com.octet.computeruse.anthropic")
        color.assert_not_called()

    def test_missing_themes_are_installed_on_first_use(self):
        # Provisioned through the agent tool, never through /computer-use setup.
        every = {e["id"] for e in entrypoint.cursor_theme.PALETTE.values()} | {"cua.default"}
        client = self._client()
        computer, install, _ = self._start(client, theme_ids={"cua.default"}, installed=every)
        install.assert_called_once_with(Path("/opt/cua-driver"))
        self.assertEqual(computer._selected_theme, "com.octet.computeruse.anthropic")

    def test_gnome_pins_the_shell_helper_to_the_model_color(self):
        every = {e["id"] for e in entrypoint.cursor_theme.PALETTE.values()} | {"cua.default"}
        computer, _, color = self._start(self._client(), theme_ids=every, gnome=True)
        color.assert_called_once_with(entrypoint.cursor_theme.PALETTE["anthropic"]["color"])

    def test_a_rejected_theme_falls_back_to_the_default_cursor(self):
        every = {e["id"] for e in entrypoint.cursor_theme.PALETTE.values()} | {"cua.default"}
        client = self._client()
        original = client.call

        def call(tool, arguments=None, **kwargs):
            if tool == "set_agent_cursor_theme" and arguments["theme_id"] != "cua.default":
                client.calls.append((tool, dict(arguments)))
                return {"isError": True, "content": [{"type": "text", "text": "artifact version"}]}
            return original(tool, arguments, **kwargs)

        client.call = call
        computer, _, _ = self._start(client, theme_ids=every)
        self.assertTrue(computer._cursor_ready)
        self.assertEqual(computer._selected_theme, "cua.default")

    def test_a_cursor_failure_never_blocks_actions(self):
        every = {e["id"] for e in entrypoint.cursor_theme.PALETTE.values()} | {"cua.default"}
        client = self._client(enabled=False)
        computer, _, _ = self._start(client, theme_ids=every)
        self.assertFalse(computer._cursor_ready)
        self.assertIn("refusing to report readiness", computer._cursor_failure)
        attempts = sum(tool == "set_agent_cursor_enabled" for tool, _ in client.calls)
        computer.call("click", {"pid": 17, "window_id": 5, "x": 10, "y": 20})
        self.assertIn("click", [tool for tool, _ in client.calls])
        # The failure is not retried on every action, only after a model switch.
        self.assertEqual(sum(tool == "set_agent_cursor_enabled" for tool, _ in client.calls), attempts)
        text = _render_status({"installed": True, "runtime": "direct", "platform": "linux",
                               "permissions": "granted", "cursor_available": True,
                               "cursor_enabled": False, "cursor_detail": computer._cursor_failure})
        self.assertIn("agent cursor: not shown", text)

    def test_native_wayland_readiness_uses_the_setter_acknowledgements(self):
        # Cua 0.30 reads cursor state back from its X11 overlay, so a pure
        # Wayland session reports defaults although the layer-shell overlay
        # draws the configured cursor (verified on headless Sway).
        every = {e["id"] for e in entrypoint.cursor_theme.PALETTE.values()} | {"cua.default"}
        client = self._client()

        def call(tool, arguments=None, **kwargs):
            args = dict(arguments or {})
            client.calls.append((tool, args))
            if tool == "set_agent_cursor_motion":
                return {"structuredContent": {"motion": {k: v for k, v in args.items() if k != "session"}}}
            if tool == "set_agent_cursor_theme":
                return {"structuredContent": {"theme": {"id": args["theme_id"]}}}
            if tool == "set_agent_cursor_enabled":
                return {"structuredContent": {"enabled": True}}
            if tool == "get_agent_cursor_state":
                return {"structuredContent": {"enabled": True, "motion": {"idle_hide_ms": 20000.0},
                                              "theme": {"id": "cua.default"}}}
            return {"content": [{"type": "text", "text": "ok"}]}

        client.call = call
        computer, _, _ = self._start(client, theme_ids=every)
        self.assertTrue(computer._cursor_ready)
        self.assertEqual(computer._selected_theme, "com.octet.computeruse.anthropic")
        # The macOS host never accepts acknowledgements in place of read-back.
        host = ComputerUse(RecordingExtension())
        host._app_daemon = True
        host._theme_ids = every
        with self.assertRaises(McpError):
            host._configure_cursor(client, "s")

    def test_status_renders_the_linux_cursor_and_gnome_helper(self):
        text = _render_status({"installed": True, "runtime": "direct", "platform": "linux",
                               "permissions": "granted", "cursor_available": True,
                               "cursor_enabled": True,
                               "cursor_theme": "com.octet.computeruse.anthropic",
                               "gnome_helper": "restart-required"})
        self.assertIn("agent cursor: on (theme com.octet.computeruse.anthropic)", text)
        self.assertIn("log out and back in", text)


class GnomeHelperTests(unittest.TestCase):
    def test_gnome_wayland_detection_covers_derivatives(self):
        from unittest import mock
        from octet_computer_use import gnome_helper

        with mock.patch.object(gnome_helper.platform, "system", return_value="Linux"):
            for desktop, expected in (("ubuntu:GNOME", True), ("GNOME", True), ("pop:GNOME", True),
                                      ("Hyprland", False), ("KDE", False)):
                with self.subTest(desktop=desktop), mock.patch.dict(
                        os.environ, {"WAYLAND_DISPLAY": "wayland-0",
                                     "XDG_CURRENT_DESKTOP": desktop}, clear=True):
                    self.assertEqual(gnome_helper.is_gnome_wayland(), expected)
            with mock.patch.dict(os.environ, {"XDG_CURRENT_DESKTOP": "GNOME", "DISPLAY": ":0"},
                                 clear=True):
                self.assertFalse(gnome_helper.is_gnome_wayland(), "GNOME on Xorg needs no helper")

    def test_bundled_helper_is_the_pinned_release_plus_the_theme_pin(self):
        from octet_computer_use import gnome_helper

        source = (gnome_helper.BUNDLE / "extension.js").read_text()
        metadata = __import__("json").loads((gnome_helper.BUNDLE / "metadata.json").read_text())
        self.assertEqual(metadata["uuid"], gnome_helper.UUID)
        self.assertEqual(metadata["version"], 8)
        self.assertIn('<method name="SetThemeColor">', source)
        self.assertIn("if (this._themeColor) return;", source)

    def test_install_writes_the_bundle_and_enables_it(self):
        from unittest import mock
        from octet_computer_use import gnome_helper

        calls = []

        def run(argv):
            calls.append(argv)
            result = mock.Mock(returncode=0, stdout="")
            if argv[:2] == ["gnome-extensions", "enable"]:
                result.returncode = 2  # not yet known to the running Shell
            elif argv[:3] == ["gsettings", "get", "org.gnome.shell"] and argv[3] == "enabled-extensions":
                result.stdout = "['user-theme@gnome-shell-extensions.gcampax.github.com']\n"
            elif argv[:3] == ["gsettings", "get", "org.gnome.shell"]:
                result.stdout = "false\n"
            elif argv[0] == "gdbus":
                result.returncode = 1  # not loaded until the next login
            return result

        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.dict(os.environ, {"HOME": directory}, clear=True), \
                mock.patch.object(gnome_helper, "_run", side_effect=run):
            outcome = gnome_helper.install()
            target = Path(directory) / ".local/share/gnome-shell/extensions/winrects@cua"
            for name in gnome_helper.FILES:
                self.assertEqual((target / name).read_bytes(),
                                 (gnome_helper.BUNDLE / name).read_bytes())
        self.assertEqual(outcome["gnome_helper"], "restart-required")
        self.assertIn("log out and back in", outcome["gnome_helper_detail"])
        self.assertIn(["gsettings", "set", "org.gnome.shell", "enabled-extensions",
                       "['user-theme@gnome-shell-extensions.gcampax.github.com', 'winrects@cua']"],
                      calls)

    def test_theme_color_is_validated_before_reaching_dbus(self):
        from unittest import mock
        from octet_computer_use import gnome_helper

        with mock.patch.object(gnome_helper, "_run") as run:
            run.return_value = mock.Mock(returncode=0, stdout="()")
            self.assertFalse(gnome_helper.set_theme_color("red; rm -rf ~"))
            run.assert_not_called()
            self.assertTrue(gnome_helper.set_theme_color("#a9634c"))
            self.assertEqual(run.call_args[0][0][-2:], ["org.cua.WinRects.SetThemeColor", "#a9634c"])


class PiplessProvisionTests(unittest.TestCase):
    """Debian/Ubuntu Pythons without python3-venv still provision the driver."""

    def test_wheel_selection_matches_arch_and_requires_a_digest(self):
        from octet_computer_use import driver

        files = [
            {"filename": "cua_driver-0.30.2-py3-none-macosx_13_0_universal2.whl",
             "url": "https://files/a", "digests": {"sha256": "a"}},
            {"filename": "cua_driver-0.30.2-py3-none-manylinux_2_31_aarch64.whl",
             "url": "https://files/b", "digests": {"sha256": "b"}},
            {"filename": "cua_driver-0.30.2-py3-none-manylinux_2_31_x86_64.whl",
             "url": "http://files/insecure", "digests": {"sha256": "c"}},
            {"filename": "cua_driver-0.30.2-py3-none-manylinux_2_31_x86_64.whl",
             "url": "https://files/d", "digests": {"sha256": "d"}},
        ]
        self.assertEqual(driver._select_wheel(files, "x86_64")["url"], "https://files/d")
        self.assertEqual(driver._select_wheel(files, "aarch64")["url"], "https://files/b")
        with self.assertRaises(driver.ProvisionError):
            driver._select_wheel(files[:1], "x86_64")

    def test_only_the_driver_package_can_be_extracted(self):
        from octet_computer_use import driver

        for name in ("cua_driver/bin/cua-driver", "cua_driver-0.30.2.dist-info/RECORD"):
            self.assertTrue(driver._safe_member(name), name)
        for name in ("/etc/passwd", "../x", "cua_driver/../../x", "cua_driver/..",
                     "other/module.py", "cua_driver\\..\\x", "cua_driver_evil/x"):
            self.assertFalse(driver._safe_member(name), name)

    def test_a_missing_ensurepip_falls_back_to_the_direct_wheel(self):
        import subprocess
        from unittest import mock
        from octet_computer_use import driver

        failed = subprocess.CompletedProcess([], 1, "", "ensurepip is not available")
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(driver, "installed_binary", return_value=None), \
                mock.patch.object(driver, "_run", return_value=failed), \
                mock.patch.object(driver, "_driver_interpreter",
                                  return_value=(sys.executable, (3, 12))), \
                mock.patch.object(driver.platform, "system", return_value="Linux"), \
                mock.patch.object(driver.platform, "machine", return_value="x86_64"), \
                mock.patch.object(driver, "_provision_without_pip",
                                  return_value=Path("/opt/cua-driver")) as fallback:
            paths = driver.DriverPaths.for_home(Path(directory))
            self.assertEqual(driver.provision(paths, version="0.30.2"), Path("/opt/cua-driver"))
        fallback.assert_called_once()
        self.assertEqual(fallback.call_args[0][1], "0.30.2")

    def test_other_platforms_still_report_the_venv_failure(self):
        import subprocess
        from unittest import mock
        from octet_computer_use import driver

        failed = subprocess.CompletedProcess([], 1, "", "no venv")
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(driver, "installed_binary", return_value=None), \
                mock.patch.object(driver, "_run", return_value=failed), \
                mock.patch.object(driver, "_driver_interpreter",
                                  return_value=(sys.executable, (3, 12))), \
                mock.patch.object(driver.platform, "system", return_value="Darwin"), \
                mock.patch.object(driver, "_provision_without_pip") as fallback:
            with self.assertRaisesRegex(driver.ProvisionError, "no venv"):
                driver.provision(driver.DriverPaths.for_home(Path(directory)))
        fallback.assert_not_called()


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
        from octet_extension.protocol import RpcError

        class RejectedPublisher:
            negotiated_features = {"artifacts"}

            def publish_artifact(self, **kwargs):
                raise RpcError(-32002, "artifact publication requires a host-owned session context")

        computer = ComputerUse(RejectedPublisher())
        result = computer._format_result("get_window_state", {
            "content": [{"type": "text", "text": "window captured"},
                        {"type": "image", "data": "QUJD", "mimeType": "image/png"}],
        })
        self.assertTrue(any("-32002: artifact publication requires" in part.get("text", "")
                            for part in result["content"]))
        self.assertFalse(result["is_error"])  # The tree can still be used.

    def test_small_and_large_screenshots_publish_by_inline_and_scratch(self):
        try:
            from .test_screenshots import RecordingPublisher, FRAME_A, _png
        except ImportError:
            from test_screenshots import RecordingPublisher, FRAME_A, _png

        with tempfile.TemporaryDirectory() as directory:
            publisher = RecordingPublisher(Path(directory))
            computer = ComputerUse(publisher)
            old = os.environ.get("OCTET_EXTENSION_SCRATCH")
            os.environ["OCTET_EXTENSION_SCRATCH"] = directory
            try:
                large = _png(marker=os.urandom(150000).hex().encode())
                for raw, source in ((FRAME_A, "data"), (large, "path")):
                    result = computer._format_result("get_window_state", {
                        "content": [{"type": "image", "data": base64.b64encode(raw).decode(),
                                     "mimeType": "image/png"}],
                    })
                    self.assertFalse(result["is_error"])
                    self.assertTrue(any(part.get("type") == "image" for part in result["content"]))
                    self.assertIn(source, publisher.calls[-1])
                self.assertEqual(publisher.path_bytes, [large])
                self.assertEqual(list((Path(directory) / "octet-computer-use-screenshots").iterdir()), [])
            finally:
                if old is None:
                    os.environ.pop("OCTET_EXTENSION_SCRATCH", None)
                else:
                    os.environ["OCTET_EXTENSION_SCRATCH"] = old

    def test_artifacts_are_negotiated(self):
        from octet_computer_use.entrypoint import create_extension

        extension, _ = create_extension()
        try:
            result = extension._initialize({
                "api_version": "0.4",
                "protocol": {"version": "0.4", "required_features": [],
                             "optional_features": ["artifacts", "content_parts"],
                             "limits": {"max_concurrent_requests": 1}},
            })
            self.assertIn("artifacts", result["protocol"]["features"])
        finally:
            if extension._executor is not None:
                extension._executor.shutdown(wait=True)

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
        # Hidden structured details are not model-visible. Narrow or use the
        # markdown tree's index with this snapshot rather than referring to them.
        self.assertIn("... 12 more in this snapshot", hint)
        self.assertIn("narrow with query", hint)
        self.assertNotIn("structured_content.elements", hint)

    def test_empty_query_is_not_reported_as_degraded(self):
        from octet_computer_use.entrypoint import _targeting_hint

        snapshot = {"element_count": 149, "returned_element_count": 0,
                    "filtered_element_count": 0, "elements": []}
        hint = _targeting_hint(snapshot, query="not-a-control")
        self.assertIn("No matching", hint)
        self.assertNotIn("degraded", hint)
        self.assertIn("retry", _targeting_hint(snapshot))

    def test_window_and_app_handles_are_model_visible(self):
        computer = ComputerUse(RecordingExtension())
        for tool, collection, record, handle in (
            ("list_windows", "windows", {"pid": 42, "window_id": 7, "app_name": "Calculator"}, "window_id=7"),
            ("list_apps", "apps", {"pid": 42, "name": "Calculator", "bundle_id": "com.apple.calculator"}, "com.apple.calculator"),
        ):
            with self.subTest(tool=tool):
                result = computer._format_result(tool, {
                    "content": [{"type": "text", "text": "Found 1 item(s)."}],
                    "structuredContent": {collection: [record]},
                })
                self.assertTrue(any(handle in part.get("text", "") for part in result["content"]))



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

    The other tests here are macOS host semantics too, so ``setUp`` pins the
    platform to Darwin rather than depending on the machine the suite runs on.
    """

    def setUp(self):
        from unittest import mock
        from octet_computer_use import driver

        patcher = mock.patch.object(driver.platform, "system", return_value="Darwin")
        patcher.start()
        self.addCleanup(patcher.stop)

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
            self.assertEqual(daemon_socket(), Path("/tmp/custom.sock"))
        finally:
            os.environ.pop("OCTET_CUA_DAEMON_SOCKET", None)



class NoDriverFailClosedTests(unittest.TestCase):
    """With no provisioned driver, status explains setup and nothing dispatches.

    This is the state of a fresh Windows (or any) install and of every CI
    runner. It never provisions, prompts, or starts a driver process.
    """

    def test_status_points_at_setup_and_every_driver_tool_fails_closed(self):
        from unittest import mock
        from octet_computer_use import driver

        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.dict(os.environ, {"OCTET_CUA_DESKTOP_HOST": "0"}), \
             mock.patch.object(driver, "desktop_app_binary", return_value=None), \
             mock.patch.object(entrypoint.DriverClient, "start",
                               side_effect=AssertionError("no driver process may start")):
            home = Path(directory)
            extension, _ = entrypoint.create_extension(home=home)
            context = {"host": {"model": "claude-sonnet-4"}}

            status = extension._tools["computer_use_status"].handler({}, context)
            self.assertFalse(status.get("is_error"))
            self.assertFalse(status["structured_content"]["installed"])
            self.assertIn("/computer-use setup", status["content"][0]["text"])

            window = {"pid": 4242, "window_id": 4242}
            for tool, arguments in (
                ("computer_use_windows", {}),
                ("computer_use_window_state", dict(window)),
                ("computer_use_click", {**window, "x": 10, "y": 10}),
                ("computer_use_type_text", {**window, "text": "never typed"}),
                ("computer_use_press_key", {**window, "key": "enter"}),
                ("computer_use_launch_app", {"name": "notepad"}),
            ):
                result = extension._tools[tool].handler(arguments, context)
                self.assertTrue(result.get("is_error"), tool)
                self.assertIn("not installed", result["content"][0]["text"], tool)
            # Status and refusals are read-only: nothing was provisioned.
            self.assertEqual(list(home.iterdir()), [])



class LiveSmokeGuardTests(unittest.TestCase):
    """The attended live smoke must never run unattended."""

    def test_refuses_ci_unconfirmed_and_non_interactive_runs(self):
        self.assertIn("CI", live_smoke.refusal_reason(True, {"CI": "true"}, True))
        self.assertIn("CI", live_smoke.refusal_reason(True, {"GITHUB_ACTIONS": "true"}, True))
        self.assertIn(live_smoke.CONFIRMATION_FLAG, live_smoke.refusal_reason(False, {}, True))
        self.assertIn("interactive", live_smoke.refusal_reason(True, {}, False))
        self.assertIsNone(live_smoke.refusal_reason(True, {}, True))

    def test_main_refuses_before_touching_the_driver(self):
        from unittest import mock

        with mock.patch.dict(os.environ, {"CI": "true"}), \
             mock.patch.object(live_smoke, "Smoke", side_effect=AssertionError("must not start")):
            self.assertEqual(live_smoke.main([live_smoke.CONFIRMATION_FLAG]), 2)

    def test_window_lookup_uses_the_bounded_window_projection(self):
        listing = {"windows": [
            {"window_id": 11, "pid": 7, "app_name": "Explorer", "title": "Downloads"},
            {"window_id": 12, "pid": 8, "app_name": "Notepad", "title": "Untitled - Notepad"},
        ]}
        self.assertEqual(live_smoke.find_window(listing, "notepad")[:2], (8, 12))
        self.assertIsNone(live_smoke.find_window(listing, "calculator"))
        self.assertIsNone(live_smoke.find_window({"windows": [{"pid": "8", "window_id": 12,
                                                                 "title": "notepad"}]}, "notepad"))


if __name__ == "__main__":
    unittest.main()
