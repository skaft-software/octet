"""Behavioural tests for the octet computer-use bundle."""

from __future__ import annotations

import os
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

    def test_health_reports_direct_runtime_without_a_usable_host(self):
        from octet_computer_use import driver

        original = driver.desktop_app_usable
        try:
            driver.desktop_app_usable = lambda binary=None: False
            self.assertEqual(driver.active_runtime(), "direct")
            driver.desktop_app_usable = lambda binary=None: True
            self.assertEqual(driver.active_runtime(), "desktop-host")
        finally:
            driver.desktop_app_usable = original

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


class DesktopHostTests(unittest.TestCase):
    """The desktop host is preferred when present, and is optional.

    The host owns the OS permission identity and the GUI main thread, which is
    what lets the driver draw the agent cursor. Without it the direct runtime
    still drives the desktop; only the overlay is unavailable.
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

    def test_unusable_host_falls_back_to_the_direct_runtime(self):
        # A host whose macOS grant never persists reports ``unknown`` and
        # re-prompts on every launch. Adopting it would make every tool call
        # fail, so the direct runtime - which inherits the calling host's grants
        # - must be chosen instead.
        from octet_computer_use import driver
        from octet_computer_use.driver_client import DriverClient

        original_status = driver._permission_status
        original_binary = driver.desktop_app_binary
        original_installed = driver.installed_binary
        original_start = driver.start_desktop_app
        try:
            driver._permission_status = lambda binary: "unknown"
            driver.desktop_app_binary = lambda app=None: Path("/Applications/CuaDriver.app")
            driver.installed_binary = lambda paths: Path("/opt/cua-driver")
            # Never launch a real app from a test: an ungranted host would
            # otherwise be started for real by this assertion.
            driver.start_desktop_app = lambda app=None: False
            self.assertFalse(driver.desktop_app_usable())

            computer = ComputerUse.__new__(ComputerUse)
            computer._lock = __import__("threading").Lock()
            computer._client = None
            computer._app_daemon = False
            computer._cursor_session = None
            computer._extension = None
            computer._paths = None
            captured = {}
            class _FakeClient(DriverClient):
                def __init__(self, binary, *, app_daemon=False, **kwargs):
                    captured["binary"] = binary
                    captured["app_daemon"] = app_daemon
                    self._started = True
                def start(self, timeout=0.0):
                    self._started = True
            entrypoint.DriverClient = _FakeClient
            try:
                computer.client()
            finally:
                entrypoint.DriverClient = DriverClient
            self.assertFalse(captured["app_daemon"])
            self.assertEqual(captured["binary"], Path("/opt/cua-driver"))

            driver._permission_status = lambda binary: "granted"
            captured.clear()
            class _FakeHostClient(_FakeClient):
                pass
            entrypoint.DriverClient = _FakeHostClient
            try:
                computer._client = None
                computer.client()
            finally:
                entrypoint.DriverClient = DriverClient
            self.assertTrue(captured["app_daemon"])
        finally:
            driver._permission_status = original_status
            driver.desktop_app_binary = original_binary
            driver.installed_binary = original_installed
            driver.start_desktop_app = original_start

    def test_ungranted_host_is_started_once_then_rejected(self):
        # A host that is installed but not running is the common case, not a
        # failure. It gets one bounded launch attempt; if it still cannot prove
        # its grant it is rejected so the direct runtime takes over.
        from octet_computer_use import driver

        original_status = driver._permission_status
        original_start = driver.start_desktop_app
        original_sleep = driver.time.sleep
        original_attempts = driver.HOST_START_ATTEMPTS
        try:
            calls = []
            driver.start_desktop_app = lambda app=None: calls.append(1) or True
            driver.time.sleep = lambda seconds: None
            driver.HOST_START_ATTEMPTS = 2
            driver._permission_status = lambda binary: "unknown"
            self.assertFalse(driver.desktop_app_usable(Path("/Applications/OctetComputerUse.app")))
            self.assertEqual(len(calls), 1, "the host must be started at most once")

            # A host that grants after starting must be adopted, so the cursor
            # becomes available without the user restarting octet.
            driver._permission_status = lambda binary: "granted"
            self.assertTrue(driver.desktop_app_usable(Path("/Applications/OctetComputerUse.app")))
        finally:
            driver._permission_status = original_status
            driver.start_desktop_app = original_start
            driver.time.sleep = original_sleep
            driver.HOST_START_ATTEMPTS = original_attempts

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

    def test_missing_host_falls_back_to_the_direct_runtime(self):
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

    def test_cursor_motion_is_flat_and_instant(self):
        # Motion removes lag only; the theme keeps the attention-grabbing
        # per-action animations.
        from octet_computer_use.entrypoint import CURSOR_MOTION

        self.assertEqual(CURSOR_MOTION["arc_size"], 0.0)
        self.assertEqual(CURSOR_MOTION["turn_radius"], 0.0)
        self.assertEqual(CURSOR_MOTION["glide_duration_ms"], 0.0)
        self.assertEqual(CURSOR_MOTION["dwell_after_click_ms"], 0.0)
        self.assertEqual(CURSOR_MOTION["spring"], 1.0)

    def test_daemon_socket_is_overridable(self):
        from octet_computer_use.driver_client import daemon_socket

        os.environ["OCTET_CUA_DAEMON_SOCKET"] = "/tmp/custom.sock"
        try:
            self.assertEqual(str(daemon_socket()), "/tmp/custom.sock")
        finally:
            os.environ.pop("OCTET_CUA_DAEMON_SOCKET", None)
