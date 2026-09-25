from __future__ import annotations

import json
import unittest
from dataclasses import replace
from pathlib import Path
from typing import Any, Dict, Iterable, Optional, Tuple

from octet_computer_use.backend_windows import (
    INTEGRITY_HIGH,
    INTEGRITY_MEDIUM,
    ApplicationIdentity,
    BackendError,
    DesktopIdentity,
    InputFieldInfo,
    Rect,
    Screenshot,
    TargetSpec,
    WindowIdentity,
    WindowsComputerUseBackend,
)


FIXTURES = Path(__file__).with_name("fixtures")


class FixtureSystem:
    """Deterministic adapter for contract tests; it is never the production adapter."""

    def __init__(self, windows: Iterable[WindowIdentity], *, desktop: Optional[DesktopIdentity] = None) -> None:
        self.windows: Dict[int, WindowIdentity] = {item.hwnd: item for item in windows}
        self.desktop = desktop or next(iter(self.windows.values())).desktop
        self.controller_level = INTEGRITY_MEDIUM
        self.foreground = next(iter(self.windows), 0)
        self.events = []
        self.fields: Dict[Tuple[int, int, int], InputFieldInfo] = {}
        self.capture = Screenshot(b"fixture-frame", 100, 80, 400, "bmp")
        self.snapshot_calls = 0

    def input_desktop(self) -> DesktopIdentity:
        return self.desktop

    def controller_integrity_level(self) -> int:
        return self.controller_level

    def enumerate_top_level_windows(self) -> Iterable[int]:
        return tuple(self.windows)

    def snapshot_window(self, hwnd: int, include_binary_hash: bool = False) -> WindowIdentity:
        _ = include_binary_hash
        self.snapshot_calls += 1
        if hwnd not in self.windows:
            raise BackendError("stale_window", "fixture HWND no longer exists")
        return self.windows[hwnd]

    def foreground_window(self) -> int:
        return self.foreground

    def activate_window(self, hwnd: int) -> bool:
        self.events.append(("activate", hwnd))
        if hwnd not in self.windows:
            return False
        self.foreground = hwnd
        return True

    def client_to_screen(self, hwnd: int, x: int, y: int) -> Tuple[int, int]:
        if hwnd not in self.windows:
            raise BackendError("stale_window", "fixture HWND no longer exists")
        return 100 + x, 100 + y

    def virtual_screen_rect(self) -> Rect:
        return Rect(0, 0, 1920, 1080)

    def send_mouse_move(self, x: int, y: int) -> bool:
        self.events.append(("move", x, y))
        return True

    def send_mouse_button(self, button: str, down: bool) -> bool:
        self.events.append(("button", button, down))
        return True

    def send_mouse_wheel(self, delta_x: int, delta_y: int) -> bool:
        self.events.append(("wheel", delta_x, delta_y))
        return True

    def send_key(self, key: str, down: bool) -> bool:
        self.events.append(("key", key, down))
        return True

    def send_unicode_text(self, text: str) -> bool:
        self.events.append(("text", text))
        return True

    def capture_window(self, hwnd: int, max_width: int, max_height: int) -> Screenshot:
        _ = (hwnd, max_width, max_height)
        return self.capture

    def input_field_at(self, hwnd: int, x: int, y: int) -> Optional[InputFieldInfo]:
        return self.fields.get((hwnd, x, y))


def fixture_window(name: str = "safe-window") -> WindowIdentity:
    value = json.loads((FIXTURES / f"{name}.json").read_text(encoding="utf-8"))
    return WindowIdentity.from_mapping(value)


def target_for(window: WindowIdentity) -> TargetSpec:
    return TargetSpec(aumid=window.app.aumid)


class WindowsBackendContractTests(unittest.TestCase):
    def test_fixture_identity_attaches_and_observes_bounded_client_area(self) -> None:
        window = fixture_window()
        system = FixtureSystem([window])
        backend = WindowsComputerUseBackend(target_for(window), api=system)

        attached = backend.attach()
        self.assertEqual(attached.hwnd, window.hwnd)
        observation = backend.observe()
        self.assertEqual(observation.identity.hwnd, window.hwnd)
        self.assertEqual(observation.width, 100)
        self.assertEqual(observation.height, 80)
        self.assertLessEqual(len(observation.data), 64 * 1024 * 1024)

    def test_same_hwnd_with_new_process_creation_time_is_stale(self) -> None:
        window = fixture_window()
        system = FixtureSystem([window])
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        backend.attach()
        system.windows[window.hwnd] = replace(
            window,
            process_id=window.process_id + 1,
            process_start_time=window.process_start_time + 1,
        )

        with self.assertRaisesRegex(BackendError, "attached HWND or process identity changed") as raised:
            backend.revalidate()
        self.assertEqual(raised.exception.code, "stale_window")
        self.assertEqual(system.events, [])

    def test_identity_fallback_requires_verified_publisher_and_product(self) -> None:
        window = fixture_window()
        target = TargetSpec(
            publisher=window.app.publisher,
            product=window.app.product,
            binary_path=window.app.binary_path,
            binary_sha256=window.app.binary_sha256,
        )
        system = FixtureSystem([window])
        backend = WindowsComputerUseBackend(target, api=system)
        self.assertEqual(backend.attach().hwnd, window.hwnd)

        system.windows[window.hwnd] = replace(
            window,
            app=replace(window.app, signature_valid=False),
        )
        with self.assertRaises(BackendError) as raised:
            backend.revalidate()
        self.assertEqual(raised.exception.code, "stale_window")

    def test_secure_desktop_and_higher_integrity_fail_closed(self) -> None:
        window = fixture_window()
        secure = DesktopIdentity("Winlogon", session_id=window.desktop.session_id)
        system = FixtureSystem([replace(window, desktop=secure)], desktop=secure)
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        with self.assertRaises(BackendError) as secure_error:
            backend.attach()
        self.assertEqual(secure_error.exception.code, "security_ui")

        normal = fixture_window()
        elevated_system = FixtureSystem([replace(normal, integrity_level=INTEGRITY_HIGH)])
        elevated_backend = WindowsComputerUseBackend(target_for(normal), api=elevated_system)
        # Discovery withholds unsafe candidates instead of exposing their
        # security properties as an automatic-selection error.
        self.assertEqual(elevated_backend.discover(), ())
        with self.assertRaises(BackendError) as missing_error:
            elevated_backend.attach()
        self.assertEqual(missing_error.exception.code, "target_not_found")
        # An explicit matching HWND still reaches the integrity boundary.
        with self.assertRaises(BackendError) as integrity_error:
            elevated_backend.attach(normal.hwnd)
        self.assertEqual(integrity_error.exception.code, "integrity_boundary")
        self.assertEqual(elevated_system.events, [])

    def test_unsafe_candidate_does_not_hide_a_safe_exact_match(self) -> None:
        normal = fixture_window()
        elevated = replace(normal, hwnd=normal.hwnd + 1, integrity_level=INTEGRITY_HIGH)
        system = FixtureSystem([elevated, normal])
        backend = WindowsComputerUseBackend(target_for(normal), api=system)
        self.assertEqual(backend.discover(), (normal,))
        self.assertEqual(backend.attach(), normal)
        self.assertEqual(system.events, [])

    def test_attached_integrity_change_invalidates_identity_before_input(self) -> None:
        normal = fixture_window()
        system = FixtureSystem([normal])
        backend = WindowsComputerUseBackend(target_for(normal), api=system)
        backend.attach()
        system.windows[normal.hwnd] = replace(normal, integrity_level=INTEGRITY_HIGH)
        with self.assertRaises(BackendError) as changed_error:
            backend.click(10, 10)
        # Integrity is part of the retained HWND/process-instance identity.
        self.assertEqual(changed_error.exception.code, "stale_window")
        self.assertEqual(system.events, [])

    def test_coordinates_cannot_escape_client_or_virtual_desktop(self) -> None:
        window = fixture_window()
        system = FixtureSystem([window])
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        backend.attach()
        with self.assertRaises(BackendError) as raised:
            backend.click(100, 0)
        self.assertEqual(raised.exception.code, "coordinate_out_of_bounds")
        self.assertEqual(system.events, [])

    def test_focus_is_revalidated_before_input(self) -> None:
        window = fixture_window()
        other = replace(window, hwnd=window.hwnd + 1, title="other")
        system = FixtureSystem([window, other])
        system.foreground = other.hwnd
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        backend.attach(window.hwnd)
        result = backend.click(10, 10)
        self.assertEqual(result.completed, 1)
        self.assertEqual(system.events[0], ("activate", window.hwnd))
        self.assertIn(("button", "left", True), system.events)
        self.assertIn(("button", "left", False), system.events)

    def test_cancellation_releases_drag_button(self) -> None:
        window = fixture_window()
        system = FixtureSystem([window])
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        backend.attach()
        checks = [0]

        def cancelled() -> bool:
            checks[0] += 1
            return checks[0] > 5

        with self.assertRaises(BackendError) as raised:
            backend.drag(10, 10, 80, 60, cancellation=cancelled)
        self.assertEqual(raised.exception.code, "cancelled")
        self.assertIn(("button", "left", True), system.events)
        self.assertEqual(system.events[-1], ("button", "left", False))

    def test_text_requires_accessibility_classification_and_rejects_sensitive_role(self) -> None:
        window = fixture_window()
        system = FixtureSystem([window])
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        backend.attach()
        screen_point = (window.hwnd, 110, 120)
        system.fields[screen_point] = InputFieldInfo(window.hwnd, "password", sensitive=True)
        with self.assertRaises(BackendError) as sensitive_error:
            backend.type_text(10, 20, "not-a-secret", field_role="password")
        self.assertEqual(sensitive_error.exception.code, "manual_required")
        self.assertNotIn(("text", "not-a-secret"), system.events)

        system.fields[screen_point] = InputFieldInfo(window.hwnd, "search")
        result = backend.type_text(10, 20, "query", field_role="search")
        self.assertEqual(result.completed, 1)
        self.assertIn(("text", "query"), system.events)

    def test_consequential_key_requires_confirmation_and_allowlist_rejects_modifiers(self) -> None:
        window = fixture_window()
        system = FixtureSystem([window])
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        backend.attach()
        with self.assertRaises(BackendError) as confirmation_error:
            backend.press_key("ENTER")
        self.assertEqual(confirmation_error.exception.code, "confirmation_required")
        self.assertEqual(system.events, [])

        self.assertEqual(backend.press_key("ENTER", confirmed=True).completed, 1)
        with self.assertRaises(BackendError) as key_error:
            backend.press_key("CTRL")
        self.assertEqual(key_error.exception.code, "input_rejected")

    def test_target_validation_rejects_ambiguous_and_unidentified_targets(self) -> None:
        window = fixture_window()
        second = replace(window, hwnd=window.hwnd + 1)
        system = FixtureSystem([window, second])
        backend = WindowsComputerUseBackend(target_for(window), api=system)
        with self.assertRaises(BackendError) as ambiguous:
            backend.attach()
        self.assertEqual(ambiguous.exception.code, "target_ambiguous")

        with self.assertRaises(BackendError) as invalid:
            WindowsComputerUseBackend({"window_title": "only-a-label"}, api=system)
        self.assertEqual(invalid.exception.code, "invalid_target")


if __name__ == "__main__":
    unittest.main()
