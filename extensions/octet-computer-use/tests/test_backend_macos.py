"""Mocked-native safety regression tests for the macOS backend.

These tests never load a macOS framework or send an input event.  The native
adapter is replaced by a bounded fixture so validation and observation fencing
can be exercised on every host.
"""

from __future__ import annotations

import unittest
from typing import Any

from octet_computer_use.backend_macos import MacOSBackend
from octet_computer_use.macos.model import (
    AccessibilityNode,
    AccessibilityTree,
    MacOSBackendError,
    PermissionReport,
    Point,
    TargetIdentity,
    WindowGeometry,
    WindowSnapshot,
)


TARGET = TargetIdentity("com.example.safe", 120, 7, "start-1")
GEOMETRY = WindowGeometry(10, 20, 800, 600)
OWNER = {"session_id": "session", "extension_instance_id": "test", "process_generation": 1}


class MockNative:
    available = True

    def __init__(self) -> None:
        self.calls: list[tuple[str, Any]] = []
        self.window = WindowSnapshot(TARGET, "Example", GEOMETRY, "Example", True)
        self.tree: Any = AccessibilityTree(
            (
                AccessibilityNode(
                    path=(),
                    role="AXButton",
                    title="Apply",
                    bounds=WindowGeometry(20, 30, 100, 40),
                    actions=("AXPress",),
                ),
            )
        )
        self.permission = PermissionReport(True, "granted", "granted", "granted")
        self.capture = b"mock-png"

    def permission_status(self) -> PermissionReport:
        self.calls.append(("permission_status", None))
        return self.permission

    def list_windows(self) -> list[Any]:
        self.calls.append(("list_windows", None))
        return [self.window]

    def inspect_target(self, target: TargetIdentity) -> WindowSnapshot:
        self.calls.append(("inspect_target", target))
        return self.window

    def accessibility_tree(self, target: TargetIdentity, *, max_nodes: int, max_depth: int) -> Any:
        self.calls.append(("accessibility_tree", (target, max_nodes, max_depth)))
        return self.tree

    def capture_window(self, target: TargetIdentity, *, max_bytes: int) -> bytes:
        self.calls.append(("capture_window", (target, max_bytes)))
        return self.capture

    def click(self, target: TargetIdentity, path: tuple[int, ...], point: Point, **kwargs: Any) -> None:
        self.calls.append(("click", (target, path, point)))

    def release_all(self) -> bool:
        self.calls.append(("release_all", None))
        return True  # Synthetic adapter explicitly acknowledges no held input.


class MacOSBackendMockedNativeTests(unittest.TestCase):
    def make_backend(self, native: MockNative | None = None, **kwargs: Any) -> tuple[MacOSBackend, MockNative]:
        adapter = native or MockNative()
        backend = MacOSBackend(
            opt_in=True,
            allow_input=True,
            require_confirmation=False,
            native=adapter,
            **kwargs,
        )
        return backend, adapter

    def test_opt_in_is_required_without_touching_native(self) -> None:
        native = MockNative()
        backend = MacOSBackend(native=native)

        with self.assertRaises(MacOSBackendError) as raised:
            backend.windows()

        self.assertEqual(raised.exception.code, "native_opt_in_required")
        self.assertEqual(native.calls, [])

    def test_windows_skip_malformed_native_metadata(self) -> None:
        native = MockNative()
        native.list_windows = lambda: [
            {
                "bundle_id": TARGET.bundle_id,
                "pid": TARGET.pid,
                "window_id": TARGET.window_id,
                "bounds": GEOMETRY.as_dict(),
                "title": 42,
                "owner_name": "Example",
            },
            native.window,
        ]
        backend, _ = self.make_backend(native)

        windows = backend.windows()

        self.assertEqual(len(windows), 1)
        self.assertEqual(windows[0]["identity"], TARGET.as_dict())

    def test_observation_is_required_and_click_reobserves(self) -> None:
        backend, native = self.make_backend()
        observation = backend.observe(TARGET, owner=OWNER)

        result = backend.click(
            TARGET,
            "ax:root",
            observation=observation,
            owner=OWNER,
            confirmation=True,
        )

        self.assertEqual(result["state"], "dispatched_and_reobserved")
        self.assertEqual(result["previous_generation"], observation["generation"])
        self.assertTrue(any(name == "click" for name, _ in native.calls))
        self.assertTrue(any(name == "release_all" for name, _ in native.calls))

    def test_replaced_process_is_refused_before_click(self) -> None:
        backend, native = self.make_backend()
        observation = backend.observe(TARGET, owner=OWNER)
        native.window = WindowSnapshot(
            TargetIdentity(TARGET.bundle_id, TARGET.pid, TARGET.window_id, "start-2"),
            "Example",
            GEOMETRY,
            "Example",
            True,
        )

        with self.assertRaises(MacOSBackendError) as raised:
            backend.click(TARGET, "ax:root", observation=observation, owner=OWNER, confirmation=True)

        self.assertEqual(raised.exception.code, "target_replaced")
        self.assertFalse(any(name == "click" for name, _ in native.calls))

    def test_sensitive_control_is_refused_without_reading_a_value(self) -> None:
        native = MockNative()
        native.tree = AccessibilityTree(
            (
                AccessibilityNode(
                    path=(),
                    role="AXTextField",
                    title="Password",
                    bounds=WindowGeometry(20, 30, 100, 40),
                    editable=True,
                ),
            )
        )
        backend, _ = self.make_backend(native)
        observation = backend.observe(TARGET, owner=OWNER)

        with self.assertRaises(MacOSBackendError) as raised:
            backend.type_text(
                TARGET,
                "ax:root",
                "secret",
                observation=observation,
                owner=OWNER,
                confirmation=True,
            )

        self.assertEqual(raised.exception.code, "sensitive_input_refused")
        self.assertFalse(any(name in {"set_text", "type_text"} for name, _ in native.calls))

    def test_malformed_tree_is_rejected_closed(self) -> None:
        native = MockNative()
        native.tree = {"nodes": [{"ref": "ax:root", "role": "AXButton", "enabled": "yes"}]}
        backend, _ = self.make_backend(native)

        with self.assertRaises(MacOSBackendError) as raised:
            backend.observe(TARGET)

        self.assertEqual(raised.exception.code, "native_malformed")

    def test_cancellation_releases_native_input(self) -> None:
        backend, native = self.make_backend()
        observation = backend.observe(TARGET)

        with self.assertRaises(MacOSBackendError) as raised:
            backend.click(
                TARGET,
                "ax:root",
                observation=observation,
                confirmation=True,
                cancellation=lambda: True,
            )

        self.assertEqual(raised.exception.code, "cancelled")
        self.assertTrue(any(name == "release_all" for name, _ in native.calls))

    def test_malformed_permission_text_is_not_a_grant(self) -> None:
        native = MockNative()
        native.permission = {
            "supported": True,
            "accessibility": "granted",
            "screen_recording": "granted",
            "synthetic_input": "granted",
        }
        backend, _ = self.make_backend(native)

        report = backend.permission_report()

        self.assertEqual(report.accessibility, "unknown")
        self.assertEqual(report.screen_recording, "unknown")
        self.assertEqual(report.synthetic_input, "unknown")
        with self.assertRaises(MacOSBackendError) as raised:
            backend.windows()
        self.assertEqual(raised.exception.code, "permission_denied")

    def test_native_integer_and_reference_limits_fail_closed(self) -> None:
        from octet_computer_use.macos.native import MacOSNative

        with self.assertRaises(ValueError):
            MacOSNative._native_integer(float("inf"), minimum=1, maximum=10)
        with self.assertRaises(ValueError):
            MacOSNative._native_integer(2**1000, minimum=1, maximum=2**31 - 1)
        with self.assertRaises(MacOSBackendError):
            MacOSBackend._path_from_ref("ax:" + "1." * 30 + "1")


if __name__ == "__main__":
    unittest.main()
