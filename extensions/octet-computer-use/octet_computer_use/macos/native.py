"""Dependency-free macOS bindings.

This module is intentionally lazy: importing the extension on Linux, Windows,
or a macOS host without the frameworks must not attempt to request permission
or load an automation agent. The real adapter uses CoreGraphics,
ApplicationServices/AXUIElement, CoreFoundation, ImageIO, and objc_msgSend via
ctypes only.
"""

from __future__ import annotations

import ctypes
import math
import sys
import time
from typing import Any, Callable, Dict, Iterable, Optional, Sequence, Tuple

from .model import (
    AccessibilityNode,
    AccessibilityTree,
    MAX_DEPTH,
    MAX_NODES,
    MAX_SCREENSHOT_BYTES,
    MAX_TEXT_BYTES,
    MacOSBackendError,
    PermissionReport,
    Point,
    TargetIdentity,
    WindowGeometry,
    WindowSnapshot,
)
from .policy import is_sensitive_node, validate_drag_duration, validate_scroll, validate_text


# CoreGraphics constants. They are stable C API values, not browser or shell
# interfaces.
_CG_WINDOW_LIST_ON_SCREEN_ONLY = 1
_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS = 1 << 4
_CG_WINDOW_LIST_INCLUDING_WINDOW = 1 << 3
_CG_WINDOW_IMAGE_DEFAULT = 0
_CG_HID_EVENT_TAP = 0
_CG_EVENT_LEFT_MOUSE_DOWN = 1
_CG_EVENT_LEFT_MOUSE_UP = 2
_CG_EVENT_MOUSE_MOVED = 5
_CG_EVENT_LEFT_MOUSE_DRAGGED = 6
_CG_MOUSE_BUTTON_LEFT = 0
_CG_SCROLL_EVENT_UNIT_LINE = 1
_CF_STRING_ENCODING_UTF8 = 0x08000100
_CF_NUMBER_DOUBLE_TYPE = 13
_AX_VALUE_CG_POINT = 1
_AX_VALUE_CG_SIZE = 2
_AX_VALUE_CG_RECT = 3
_AX_SUCCESS = 0


class _CGPoint(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


class _CGSize(ctypes.Structure):
    _fields_ = [("width", ctypes.c_double), ("height", ctypes.c_double)]


class _CGRect(ctypes.Structure):
    _fields_ = [("origin", _CGPoint), ("size", _CGSize)]


class _CFRange(ctypes.Structure):
    _fields_ = [("location", ctypes.c_long), ("length", ctypes.c_long)]


class MacOSNative:
    """Concrete native adapter; it has no third-party Python dependency.

    The class is safe to construct on non-macOS hosts. All framework symbols
    are loaded only on Darwin and all permission calls are preflight calls that
    do not display a system prompt.
    """

    def __init__(self) -> None:
        self._supported = sys.platform == "darwin"
        self._loaded = False
        self._load_error = "unsupported_platform" if not self._supported else ""
        self._cf_keys: Dict[str, int] = {}
        self._pressed_buttons: set[int] = set()
        self._pressed_keys: set[int] = set()
        self._last_point = Point(0.0, 0.0)
        if self._supported:
            try:
                self._load_frameworks()
                self._loaded = True
            except Exception as error:
                # A missing framework symbol or malformed native binding is a
                # bounded capability failure, not a reason to fall back to a
                # less constrained automation mechanism.
                self._load_error = type(error).__name__

    @staticmethod
    def _pointer_value(value: Any) -> Optional[int]:
        if isinstance(value, ctypes.c_void_p):
            value = value.value
        if value is None:
            return None
        if type(value) is not int:
            raise MacOSBackendError("native_malformed", "The native pointer value was malformed.")
        maximum = (1 << (ctypes.sizeof(ctypes.c_void_p) * 8)) - 1
        if not 0 < value <= maximum:
            raise MacOSBackendError("native_malformed", "The native pointer value was outside its bounded range.")
        return value

    @property
    def available(self) -> bool:
        return self._loaded

    @property
    def load_error(self) -> str:
        return self._load_error

    def _load_frameworks(self) -> None:
        self._cf = ctypes.CDLL(
            "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
        )
        self._cg = ctypes.CDLL(
            "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics"
        )
        self._ax = ctypes.CDLL(
            "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices"
        )
        self._image_io = ctypes.CDLL(
            "/System/Library/Frameworks/ImageIO.framework/ImageIO"
        )
        self._foundation = ctypes.CDLL(
            "/System/Library/Frameworks/Foundation.framework/Foundation"
        )
        self._objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
        self._configure_corefoundation()
        self._configure_coregraphics()
        self._configure_accessibility()
        self._configure_image_io()
        self._configure_objc()

        # Creating these keys is side-effect free. They are retained for the
        # lifetime of the adapter so borrowed CF dictionary values remain safe
        # during every call.
        for name in (
            "kCGWindowNumber",
            "kCGWindowOwnerPID",
            "kCGWindowOwnerName",
            "kCGWindowName",
            "kCGWindowBounds",
            "kCGWindowLayer",
            "kCGWindowAlpha",
            "X",
            "Y",
            "Width",
            "Height",
            "AXWindows",
            "AXRole",
            "AXSubrole",
            "AXTitle",
            "AXDescription",
            "AXHelp",
            "AXValue",
            "AXEnabled",
            "AXFocused",
            "AXSelected",
            "AXPosition",
            "AXSize",
            "AXChildren",
            "AXActions",
            "AXPressAction",
            "AXSetValueAction",
            "public.png",
        ):
            self._cf_keys[name] = self._new_cf_string(name)

    def _configure_corefoundation(self) -> None:
        cf = self._cf
        cf.CFRelease.argtypes = [ctypes.c_void_p]
        cf.CFRelease.restype = None
        cf.CFRetain.argtypes = [ctypes.c_void_p]
        cf.CFRetain.restype = ctypes.c_void_p
        cf.CFGetTypeID.argtypes = [ctypes.c_void_p]
        cf.CFGetTypeID.restype = ctypes.c_ulong
        cf.CFStringGetTypeID.restype = ctypes.c_ulong
        cf.CFStringCreateWithCString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_char_p,
            ctypes.c_uint32,
        ]
        cf.CFStringCreateWithCString.restype = ctypes.c_void_p
        cf.CFStringGetLength.argtypes = [ctypes.c_void_p]
        cf.CFStringGetLength.restype = ctypes.c_long
        cf.CFStringGetCString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_char_p,
            ctypes.c_long,
            ctypes.c_uint32,
        ]
        cf.CFStringGetCString.restype = ctypes.c_bool
        cf.CFNumberGetTypeID.restype = ctypes.c_ulong
        cf.CFNumberGetValue.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_void_p,
        ]
        cf.CFNumberGetValue.restype = ctypes.c_bool
        cf.CFBooleanGetTypeID.restype = ctypes.c_ulong
        cf.CFBooleanGetValue.argtypes = [ctypes.c_void_p]
        cf.CFBooleanGetValue.restype = ctypes.c_bool
        cf.CFArrayGetTypeID.restype = ctypes.c_ulong
        cf.CFArrayGetCount.argtypes = [ctypes.c_void_p]
        cf.CFArrayGetCount.restype = ctypes.c_long
        cf.CFArrayGetValueAtIndex.argtypes = [ctypes.c_void_p, ctypes.c_long]
        cf.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
        cf.CFDictionaryGetTypeID.restype = ctypes.c_ulong
        cf.CFDictionaryGetCount.argtypes = [ctypes.c_void_p]
        cf.CFDictionaryGetCount.restype = ctypes.c_long
        cf.CFDictionaryGetKeysAndValues.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_void_p),
        ]
        cf.CFDictionaryGetKeysAndValues.restype = None
        cf.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        cf.CFDictionaryGetValue.restype = ctypes.c_void_p
        cf.CFDataGetTypeID.restype = ctypes.c_ulong
        cf.CFDataGetLength.argtypes = [ctypes.c_void_p]
        cf.CFDataGetLength.restype = ctypes.c_long
        cf.CFDataGetBytePtr.argtypes = [ctypes.c_void_p]
        cf.CFDataGetBytePtr.restype = ctypes.c_void_p
        cf.CFDataCreateMutable.argtypes = [ctypes.c_void_p, ctypes.c_long]
        cf.CFDataCreateMutable.restype = ctypes.c_void_p
        cf.CFDataGetLength.argtypes = [ctypes.c_void_p]
        cf.CFDataGetBytePtr.argtypes = [ctypes.c_void_p]
        cf.CFDataAppendBytes.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_ubyte),
            ctypes.c_long,
        ]
        cf.CFDataAppendBytes.restype = None

    def _configure_coregraphics(self) -> None:
        cg = self._cg
        cg.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
        cg.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
        cg.CGWindowListCreateImage.argtypes = [
            _CGRect,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_uint32,
        ]
        cg.CGWindowListCreateImage.restype = ctypes.c_void_p
        cg.CGImageGetWidth.argtypes = [ctypes.c_void_p]
        cg.CGImageGetWidth.restype = ctypes.c_size_t
        cg.CGImageGetHeight.argtypes = [ctypes.c_void_p]
        cg.CGImageGetHeight.restype = ctypes.c_size_t
        cg.CGImageRelease.argtypes = [ctypes.c_void_p]
        cg.CGImageRelease.restype = None
        cg.CGEventCreateMouseEvent.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint32,
            _CGPoint,
            ctypes.c_uint32,
        ]
        cg.CGEventCreateMouseEvent.restype = ctypes.c_void_p
        cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
        cg.CGEventPost.restype = None
        cg.CGEventCreateKeyboardEvent.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint16,
            ctypes.c_bool,
        ]
        cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
        cg.CGEventKeyboardSetUnicodeString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_ulong,
            ctypes.POINTER(ctypes.c_uint16),
        ]
        cg.CGEventKeyboardSetUnicodeString.restype = None
        cg.CGEventCreateScrollWheelEvent.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_int32,
            ctypes.c_int32,
        ]
        cg.CGEventCreateScrollWheelEvent.restype = ctypes.c_void_p
        cg.CGEventSetLocation.argtypes = [ctypes.c_void_p, _CGPoint]
        cg.CGEventSetLocation.restype = None
        cg.CGEventGetLocation.argtypes = [ctypes.c_void_p]
        cg.CGEventGetLocation.restype = _CGPoint
        cg.CGPreflightScreenCaptureAccess = getattr(cg, "CGPreflightScreenCaptureAccess", None)
        if cg.CGPreflightScreenCaptureAccess is not None:
            cg.CGPreflightScreenCaptureAccess.argtypes = []
            cg.CGPreflightScreenCaptureAccess.restype = ctypes.c_bool

        # A nullable symbol is available on supported macOS versions. The
        # backend reports screenshots unavailable rather than using another API
        # when it is absent.
        cg.CGEventSourceButtonState = getattr(cg, "CGEventSourceButtonState", None)

    def _configure_accessibility(self) -> None:
        ax = self._ax
        ax.AXIsProcessTrusted.argtypes = []
        ax.AXIsProcessTrusted.restype = ctypes.c_bool
        ax.AXUIElementCreateApplication.argtypes = [ctypes.c_int32]
        ax.AXUIElementCreateApplication.restype = ctypes.c_void_p
        ax.AXUIElementCopyAttributeValue.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        ax.AXUIElementCopyAttributeValue.restype = ctypes.c_int32
        ax.AXUIElementIsAttributeSettable.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_bool),
        ]
        ax.AXUIElementIsAttributeSettable.restype = ctypes.c_int32
        ax.AXUIElementPerformAction.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        ax.AXUIElementPerformAction.restype = ctypes.c_int32
        ax.AXUIElementSetAttributeValue.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        ax.AXUIElementSetAttributeValue.restype = ctypes.c_int32
        ax.AXValueGetType.argtypes = [ctypes.c_void_p]
        ax.AXValueGetType.restype = ctypes.c_int32
        ax.AXValueGetValue.argtypes = [ctypes.c_void_p, ctypes.c_int32, ctypes.c_void_p]
        ax.AXValueGetValue.restype = ctypes.c_bool

    def _configure_image_io(self) -> None:
        image_io = self._image_io
        image_io.CGImageDestinationCreateWithData.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_size_t,
            ctypes.c_void_p,
        ]
        image_io.CGImageDestinationCreateWithData.restype = ctypes.c_void_p
        image_io.CGImageDestinationAddImage.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        image_io.CGImageDestinationAddImage.restype = None
        image_io.CGImageDestinationFinalize.argtypes = [ctypes.c_void_p]
        image_io.CGImageDestinationFinalize.restype = ctypes.c_bool

    def _configure_objc(self) -> None:
        objc = self._objc
        objc.objc_getClass.argtypes = [ctypes.c_char_p]
        objc.objc_getClass.restype = ctypes.c_void_p
        objc.sel_registerName.argtypes = [ctypes.c_char_p]
        objc.sel_registerName.restype = ctypes.c_void_p
        self._objc_get_class = objc.objc_getClass
        self._sel_register = objc.sel_registerName
        self._objc_msg0 = ctypes.CFUNCTYPE(
            ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p
        )(("objc_msgSend", objc))
        self._objc_msg_i32 = ctypes.CFUNCTYPE(
            ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int32
        )(("objc_msgSend", objc))
        self._objc_msg_bool = ctypes.CFUNCTYPE(
            ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p
        )(("objc_msgSend", objc))
        self._objc_msg_double = ctypes.CFUNCTYPE(
            ctypes.c_double, ctypes.c_void_p, ctypes.c_void_p
        )(("objc_msgSend", objc))

    def _new_cf_string(self, value: str) -> int:
        if not isinstance(value, str):
            raise MacOSBackendError("native_malformed", "The native string value was malformed.")
        try:
            encoded = value.encode("utf-8")
        except UnicodeError as error:
            raise MacOSBackendError("native_malformed", "The native string value was not valid UTF-8.") from error
        if len(encoded) > MAX_TEXT_BYTES:
            raise MacOSBackendError("native_malformed", "The native string value exceeded the bounded size.")
        ref = self._cf.CFStringCreateWithCString(None, encoded, _CF_STRING_ENCODING_UTF8)
        pointer = self._pointer_value(ref)
        if pointer is None:
            raise MemoryError("could not create a CoreFoundation string")
        return pointer

    def _release(self, ref: Any) -> None:
        pointer = self._pointer_value(ref)
        if pointer is not None:
            self._cf.CFRelease(ctypes.c_void_p(pointer))

    def _safe_release(self, ref: Any) -> None:
        try:
            self._release(ref)
        except Exception:
            # Cleanup must not mask the bounded failure that triggered it.
            pass

    def _release_image(self, ref: Any) -> None:
        try:
            pointer = self._pointer_value(ref)
            if pointer is not None:
                self._cg.CGImageRelease(ctypes.c_void_p(pointer))
        except Exception:
            pass

    def _retain(self, ref: Any) -> Optional[int]:
        pointer = self._pointer_value(ref)
        if pointer is None:
            return None
        retained = self._cf.CFRetain(ctypes.c_void_p(pointer))
        retained_pointer = self._pointer_value(retained)
        if retained_pointer is None:
            raise MacOSBackendError("native_ownership", "CoreFoundation could not retain a native object safely.")
        return retained_pointer

    def _key(self, name: str) -> int:
        return self._cf_keys[name]

    def _require_loaded(self) -> None:
        if not self._loaded:
            raise MacOSBackendError(
                "native_unavailable",
                "The dependency-free macOS native interfaces are unavailable on this host.",
            )

    def permission_status(self) -> PermissionReport:
        if not self._loaded:
            return PermissionReport(
                supported=False,
                accessibility="unsupported",
                screen_recording="unsupported",
                synthetic_input="unsupported",
                detail="CoreGraphics and Accessibility are available only on macOS.",
            )
        try:
            trusted = self._ax.AXIsProcessTrusted()
            accessibility = "granted" if type(trusted) is bool and trusted else "denied" if type(trusted) is bool else "unknown"
        except Exception:
            accessibility = "unknown"
        preflight = getattr(self._cg, "CGPreflightScreenCaptureAccess", None)
        if preflight is None:
            screen_recording = "unknown"
        else:
            try:
                result = preflight()
                screen_recording = "granted" if type(result) is bool and result else "denied" if type(result) is bool else "unknown"
            except Exception:
                screen_recording = "unknown"
        synthetic_input = accessibility
        missing = [
            label
            for label, state in (
                ("Accessibility", accessibility),
                ("Screen Recording", screen_recording),
                ("Accessibility for synthetic input", synthetic_input),
            )
            if state != "granted"
        ]
        detail = "" if not missing else "Grant " + ", ".join(missing) + " in System Settings; no prompt was requested."
        return PermissionReport(
            supported=True,
            accessibility=accessibility,
            screen_recording=screen_recording,
            synthetic_input=synthetic_input,
            detail=detail,
        )

    def list_windows(self) -> Sequence[WindowSnapshot]:
        self._require_loaded()
        report = self.permission_status()
        if report.screen_recording != "granted":
            raise MacOSBackendError(
                "permission_denied",
                "Screen Recording permission is denied or unavailable; native observation is fail-closed.",
            )
        info = self._cg.CGWindowListCopyWindowInfo(
            _CG_WINDOW_LIST_ON_SCREEN_ONLY | _CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
            0,
        )
        info_ref = self._pointer_value(info)
        if info_ref is None:
            return ()
        try:
            rows = self._cf_value(info_ref, max_depth=4)
            if not isinstance(rows, list):
                return ()
            result: list[WindowSnapshot] = []
            first_window_for_pid: set[int] = set()
            for row in rows[:512]:
                if not isinstance(row, dict):
                    continue
                try:
                    window_id = self._native_integer(
                        row.get("kCGWindowNumber"), minimum=1, maximum=2**32 - 1
                    )
                    pid = self._native_integer(
                        row.get("kCGWindowOwnerPID"), minimum=1, maximum=2**31 - 1
                    )
                    # CoreGraphics orders on-screen windows front to back. Mark
                    # the first row for a process before filtering its geometry
                    # or layer so a malformed/overlay row cannot make a later
                    # window appear frontmost by accident.
                    first_for_pid = pid not in first_window_for_pid
                    first_window_for_pid.add(pid)
                    layer = self._native_integer(
                        row.get("kCGWindowLayer"), minimum=-(2**31), maximum=2**31 - 1
                    )
                    bounds = row.get("kCGWindowBounds")
                    if layer != 0 or not isinstance(bounds, dict):
                        continue
                    geometry = WindowGeometry(
                        bounds.get("X"),
                        bounds.get("Y"),
                        bounds.get("Width"),
                        bounds.get("Height"),
                    )
                except (TypeError, ValueError, OverflowError):
                    continue
                title = row.get("kCGWindowName")
                owner_name = row.get("kCGWindowOwnerName")
                if (title is not None and not isinstance(title, str)) or (
                    owner_name is not None and not isinstance(owner_name, str)
                ):
                    continue
                try:
                    bundle_id, process_token, active = self._process_metadata(pid)
                except (AttributeError, OSError, TypeError, ValueError, OverflowError):
                    continue
                if not bundle_id:
                    # Exact bundle identity is a safety requirement, not an
                    # optional display field.
                    continue
                frontmost = active and first_for_pid
                try:
                    identity = TargetIdentity(
                        bundle_id=bundle_id,
                        pid=pid,
                        window_id=window_id,
                        process_start_token=process_token,
                    )
                    result.append(
                        WindowSnapshot(
                            identity=identity,
                            title=title or "",
                            geometry=geometry,
                            owner_name=owner_name or "",
                            frontmost=frontmost,
                        )
                    )
                except (TypeError, ValueError):
                    continue
            return tuple(result)
        finally:
            self._safe_release(info_ref)

    def inspect_target(self, target: TargetIdentity) -> WindowSnapshot:
        self._require_loaded()
        target = TargetIdentity.from_value(target)
        for window in self.list_windows():
            if window.identity.pid != target.pid or window.identity.window_id != target.window_id:
                continue
            if window.identity.bundle_id != target.bundle_id:
                continue
            if target.process_start_token is not None and window.identity.process_start_token != target.process_start_token:
                continue
            return window
        raise MacOSBackendError(
            "target_not_found",
            "The exact application process and window are not currently present.",
        )

    def accessibility_tree(
        self,
        target: TargetIdentity,
        *,
        max_nodes: int = MAX_NODES,
        max_depth: int = MAX_DEPTH,
    ) -> AccessibilityTree:
        self._require_loaded()
        if type(max_nodes) is not int or not 1 <= max_nodes <= MAX_NODES:
            raise MacOSBackendError(
                "invalid_limit", "The Accessibility node limit is outside its bounded range."
            )
        if type(max_depth) is not int or not 1 <= max_depth <= MAX_DEPTH:
            raise MacOSBackendError(
                "invalid_limit", "The Accessibility depth limit is outside its bounded range."
            )
        target = TargetIdentity.from_value(target)
        window = self.inspect_target(target)
        app, ax_windows, ax_window = self._locate_ax_window(window)
        try:
            return self._walk_tree(ax_window, max_nodes=max_nodes, max_depth=max_depth)
        finally:
            self._safe_release(ax_windows)
            self._safe_release(app)

    def capture_window(self, target: TargetIdentity, *, max_bytes: int = MAX_SCREENSHOT_BYTES) -> bytes:
        self._require_loaded()
        if type(max_bytes) is not int or not 1 <= max_bytes <= MAX_SCREENSHOT_BYTES:
            raise MacOSBackendError(
                "invalid_limit", "The screenshot byte limit is outside its bounded range."
            )
        target_window = self.inspect_target(TargetIdentity.from_value(target))
        report = self.permission_status()
        if report.screen_recording != "granted":
            raise MacOSBackendError(
                "permission_denied",
                "Screen Recording permission is denied or unavailable; screenshots are fail-closed.",
            )
        if target_window.geometry.width * target_window.geometry.height > 16_777_216:
            raise MacOSBackendError(
                "capture_too_large",
                "The selected window exceeds the bounded screenshot pixel limit.",
            )
        rect = _CGRect(
            _CGPoint(target_window.geometry.x, target_window.geometry.y),
            _CGSize(target_window.geometry.width, target_window.geometry.height),
        )
        image = self._cg.CGWindowListCreateImage(
            rect,
            _CG_WINDOW_LIST_INCLUDING_WINDOW,
            target_window.identity.window_id,
            _CG_WINDOW_IMAGE_DEFAULT,
        )
        image_ref = self._pointer_value(image)
        if image_ref is None:
            raise MacOSBackendError("capture_unavailable", "CoreGraphics could not capture the selected window.")
        try:
            width = int(self._cg.CGImageGetWidth(ctypes.c_void_p(image_ref)))
            height = int(self._cg.CGImageGetHeight(ctypes.c_void_p(image_ref)))
            if width < 1 or height < 1 or width * height > 16_777_216:
                raise MacOSBackendError("capture_too_large", "The selected window exceeds the bounded screenshot pixel limit.")
            data = self._cf.CFDataCreateMutable(None, 0)
            data_ref = self._pointer_value(data)
            if data_ref is None:
                raise MacOSBackendError("capture_unavailable", "CoreFoundation could not allocate screenshot data.")
            try:
                destination = self._image_io.CGImageDestinationCreateWithData(
                    ctypes.c_void_p(data_ref), self._key("public.png"), 1, None
                )
                destination_ref = self._pointer_value(destination)
                if destination_ref is None:
                    raise MacOSBackendError("capture_unavailable", "ImageIO could not create a PNG destination.")
                try:
                    self._image_io.CGImageDestinationAddImage(
                        ctypes.c_void_p(destination_ref), ctypes.c_void_p(image_ref), None
                    )
                    if not self._image_io.CGImageDestinationFinalize(ctypes.c_void_p(destination_ref)):
                        raise MacOSBackendError("capture_unavailable", "ImageIO could not finalize the PNG capture.")
                finally:
                    self._safe_release(destination_ref)
                length = int(self._cf.CFDataGetLength(ctypes.c_void_p(data_ref)))
                if length < 1 or length > max_bytes:
                    raise MacOSBackendError("capture_too_large", "The PNG capture exceeds the bounded byte limit.")
                pointer = self._pointer_value(self._cf.CFDataGetBytePtr(ctypes.c_void_p(data_ref)))
                if pointer is None:
                    raise MacOSBackendError("capture_unavailable", "The PNG capture has no readable data.")
                return ctypes.string_at(pointer, length)
            finally:
                self._safe_release(data_ref)
        finally:
            self._release_image(image_ref)

    @staticmethod
    def _validate_action_point(
        point: Point,
        window: WindowSnapshot,
        expected_bounds: Optional[WindowGeometry],
    ) -> Tuple[Point, Optional[WindowGeometry]]:
        bounded_point = Point.from_value(point, label="action point")
        bounded_bounds = (
            None
            if expected_bounds is None
            else WindowGeometry.from_value(expected_bounds, label="observed node bounds")
        )
        if not window.geometry.contains(bounded_point, margin=0.5):
            raise MacOSBackendError(
                "invalid_geometry", "The action point is outside the freshly observed window."
            )
        if bounded_bounds is not None:
            center = bounded_bounds.center()
            if not bounded_bounds.contains(bounded_point, margin=0.5) or (
                abs(bounded_point.x - center.x) > 0.5 or abs(bounded_point.y - center.y) > 0.5
            ):
                raise MacOSBackendError(
                    "stale_node", "The action point is not the observed accessibility node center."
                )
        return bounded_point, bounded_bounds

    def click(
        self,
        target: TargetIdentity,
        path: Tuple[int, ...],
        point: Point,
        *,
        expected_role: Optional[str] = None,
        expected_title: Optional[str] = None,
        expected_bounds: Optional[WindowGeometry] = None,
        cancellation: Any = None,
    ) -> None:
        self._require_loaded()
        self._check_cancel(cancellation)
        window = self.inspect_target(TargetIdentity.from_value(target))
        point, expected_bounds = self._validate_action_point(point, window, expected_bounds)
        app, ax_windows, ax_window = self._locate_ax_window(window)
        try:
            element = self._resolve_element(ax_window, path)
            try:
                if element is None:
                    raise MacOSBackendError("stale_node", "The observed accessibility node is no longer present.")
                self._verify_element(element, expected_role, expected_title, expected_bounds)
                actions = self._element_actions(element)
                if self._key("AXPressAction") in actions:
                    self._check_cancel(cancellation)
                    status = self._ax.AXUIElementPerformAction(
                        ctypes.c_void_p(element), ctypes.c_void_p(self._key("AXPressAction"))
                    )
                    if status == _AX_SUCCESS:
                        return
                # AXPress is preferred. Physical input is the bounded fallback for
                # controls that expose a geometry but no AXPress action.
                self._post_click(point, cancellation)
            finally:
                self._safe_release(element)
        finally:
            self._safe_release(ax_windows)
            self._safe_release(app)

    def set_text(
        self,
        target: TargetIdentity,
        path: Tuple[int, ...],
        text: str,
        point: Point,
        *,
        expected_role: Optional[str] = None,
        expected_title: Optional[str] = None,
        expected_bounds: Optional[WindowGeometry] = None,
        cancellation: Any = None,
    ) -> None:
        self._require_loaded()
        bounded_text = validate_text(text)
        self._check_cancel(cancellation)
        window = self.inspect_target(TargetIdentity.from_value(target))
        point, expected_bounds = self._validate_action_point(point, window, expected_bounds)
        app, ax_windows, ax_window = self._locate_ax_window(window)
        try:
            element = self._resolve_element(ax_window, path)
            try:
                if element is None:
                    raise MacOSBackendError("stale_node", "The observed accessibility node is no longer present.")
                self._verify_element(element, expected_role, expected_title, expected_bounds)
                settable = ctypes.c_bool(False)
                status = self._ax.AXUIElementIsAttributeSettable(
                    ctypes.c_void_p(element), ctypes.c_void_p(self._key("AXValue")), ctypes.byref(settable)
                )
                if status == _AX_SUCCESS and settable.value:
                    value = self._new_cf_string(bounded_text)
                    try:
                        self._check_cancel(cancellation)
                        status = self._ax.AXUIElementSetAttributeValue(
                            ctypes.c_void_p(element), ctypes.c_void_p(self._key("AXValue")), ctypes.c_void_p(value)
                        )
                    finally:
                        self._safe_release(value)
                    if status == _AX_SUCCESS:
                        return
                # Some native text controls expose no settable AXValue. The
                # fallback clicks only the observed control center, then emits
                # Unicode events one bounded character at a time.
                self._post_click(point, cancellation)
                self._post_unicode(bounded_text, cancellation)
            finally:
                self._safe_release(element)
        finally:
            self._safe_release(ax_windows)
            self._safe_release(app)

    def press_key(self, key: str, *, cancellation: Any = None) -> None:
        self._require_loaded()
        self._check_cancel(cancellation)
        keycodes = {
            "Return": 36,
            "Enter": 36,
            "Escape": 53,
            "Tab": 48,
            "Space": 49,
            "Backspace": 51,
            "Delete": 117,
            "Left": 123,
            "Right": 124,
            "Up": 126,
            "Down": 125,
            "Home": 115,
            "End": 119,
            "PageUp": 116,
            "PageDown": 121,
        }
        if not isinstance(key, str) or len(key.encode("utf-8")) > 32:
            raise MacOSBackendError("key_not_allowed", "The native key is not in the bounded allowlist.")
        keycode = keycodes.get(key)
        if keycode is None:
            raise MacOSBackendError("key_not_allowed", "The native key is not in the bounded allowlist.")
        down = self._cg.CGEventCreateKeyboardEvent(None, keycode, True)
        if not down:
            raise MacOSBackendError("input_unavailable", "CoreGraphics could not create the key event.")
        self._pressed_keys.add(keycode)
        up_posted = False
        try:
            self._cg.CGEventPost(_CG_HID_EVENT_TAP, down)
            self._check_cancel(cancellation)
            up = self._cg.CGEventCreateKeyboardEvent(None, keycode, False)
            if not up:
                raise MacOSBackendError("input_unavailable", "CoreGraphics could not create the key release event.")
            try:
                self._cg.CGEventPost(_CG_HID_EVENT_TAP, up)
                up_posted = True
            finally:
                self._safe_release(up)
        finally:
            self._safe_release(down)
            try:
                if not up_posted and keycode in self._pressed_keys:
                    emergency_up = self._cg.CGEventCreateKeyboardEvent(None, keycode, False)
                    try:
                        if emergency_up:
                            self._cg.CGEventPost(_CG_HID_EVENT_TAP, emergency_up)
                    except Exception:
                        pass
                    finally:
                        self._safe_release(emergency_up)
            finally:
                self._pressed_keys.discard(keycode)

    def scroll(
        self,
        target: TargetIdentity,
        delta_x: int,
        delta_y: int,
        *,
        cancellation: Any = None,
    ) -> None:
        self._require_loaded()
        bounded_x, bounded_y = validate_scroll(delta_x, delta_y, 2_000)
        self._check_cancel(cancellation)
        window = self.inspect_target(TargetIdentity.from_value(target))
        event = self._cg.CGEventCreateScrollWheelEvent(
            None,
            _CG_SCROLL_EVENT_UNIT_LINE,
            2,
            bounded_y,
            bounded_x,
        )
        if not event:
            raise MacOSBackendError("input_unavailable", "CoreGraphics could not create the scroll event.")
        try:
            center = window.geometry.center()
            self._cg.CGEventSetLocation(event, _CGPoint(center.x, center.y))
            self._check_cancel(cancellation)
            self._cg.CGEventPost(_CG_HID_EVENT_TAP, event)
        finally:
            self._safe_release(event)

    def drag(
        self,
        target: TargetIdentity,
        start: Point,
        end: Point,
        duration: float,
        *,
        cancellation: Any = None,
    ) -> None:
        self._require_loaded()
        bounded_start = Point.from_value(start, label="drag start")
        bounded_end = Point.from_value(end, label="drag end")
        bounded_duration = validate_drag_duration(duration, 5.0)
        window = self.inspect_target(TargetIdentity.from_value(target))
        if not window.geometry.contains(bounded_start) or not window.geometry.contains(bounded_end):
            raise MacOSBackendError("invalid_geometry", "Drag coordinates must stay inside the selected window.")
        self._check_cancel(cancellation)
        down = self._mouse_event(_CG_EVENT_LEFT_MOUSE_DOWN, bounded_start)
        if not down:
            raise MacOSBackendError("input_unavailable", "CoreGraphics could not create the drag start event.")
        down_posted = False
        self._pressed_buttons.add(_CG_MOUSE_BUTTON_LEFT)
        try:
            self._check_cancel(cancellation)
            down_posted = True
            self._cg.CGEventPost(_CG_HID_EVENT_TAP, down)
            steps = max(1, min(300, int(math.ceil(bounded_duration * 60.0))))
            for index in range(1, steps + 1):
                self._check_cancel(cancellation)
                fraction = index / steps
                point = Point(
                    bounded_start.x + (bounded_end.x - bounded_start.x) * fraction,
                    bounded_start.y + (bounded_end.y - bounded_start.y) * fraction,
                )
                moved = self._mouse_event(_CG_EVENT_LEFT_MOUSE_DRAGGED, point)
                if not moved:
                    raise MacOSBackendError("input_unavailable", "CoreGraphics could not create a drag event.")
                try:
                    self._check_cancel(cancellation)
                    self._cg.CGEventPost(_CG_HID_EVENT_TAP, moved)
                finally:
                    self._safe_release(moved)
                if bounded_duration:
                    time.sleep(bounded_duration / steps)
        finally:
            self._safe_release(down)
            if down_posted:
                up: Optional[int] = None
                try:
                    up = self._mouse_event(_CG_EVENT_LEFT_MOUSE_UP, bounded_end)
                    if up:
                        self._cg.CGEventPost(_CG_HID_EVENT_TAP, up)
                except Exception:
                    pass
                finally:
                    self._safe_release(up)
            self._pressed_buttons.discard(_CG_MOUSE_BUTTON_LEFT)

    def release_all(self) -> None:
        if not self._loaded:
            self._pressed_buttons.clear()
            self._pressed_keys.clear()
            return
        for button in tuple(self._pressed_buttons):
            event: Optional[int] = None
            try:
                event = self._mouse_event(_CG_EVENT_LEFT_MOUSE_UP, self._last_point)
                if event:
                    self._cg.CGEventPost(_CG_HID_EVENT_TAP, event)
            except Exception:
                pass
            finally:
                self._safe_release(event)
                self._pressed_buttons.discard(button)
        self._release_all_keys()

    close = release_all

    def _release_all_keys(self) -> None:
        for keycode in tuple(self._pressed_keys):
            event: Optional[int] = None
            try:
                event = self._cg.CGEventCreateKeyboardEvent(None, keycode, False)
                if event:
                    self._cg.CGEventPost(_CG_HID_EVENT_TAP, event)
            except Exception:
                pass
            finally:
                self._safe_release(event)
                self._pressed_keys.discard(keycode)

    def _mouse_event(self, kind: int, point: Point) -> Optional[int]:
        self._last_point = point
        event = self._cg.CGEventCreateMouseEvent(
            None, kind, _CGPoint(float(point.x), float(point.y)), _CG_MOUSE_BUTTON_LEFT
        )
        return self._pointer_value(event)

    def _post_click(self, point: Point, cancellation: Any) -> None:
        self._check_cancel(cancellation)
        down = self._mouse_event(_CG_EVENT_LEFT_MOUSE_DOWN, point)
        up = self._mouse_event(_CG_EVENT_LEFT_MOUSE_UP, point)
        if not down or not up:
            self._safe_release(down)
            self._safe_release(up)
            raise MacOSBackendError("input_unavailable", "CoreGraphics could not create the click event.")
        down_posted = False
        up_posted = False
        self._pressed_buttons.add(_CG_MOUSE_BUTTON_LEFT)
        try:
            down_posted = True
            self._cg.CGEventPost(_CG_HID_EVENT_TAP, down)
            self._check_cancel(cancellation)
            self._cg.CGEventPost(_CG_HID_EVENT_TAP, up)
            up_posted = True
        finally:
            try:
                if down_posted and not up_posted:
                    emergency_up = self._mouse_event(_CG_EVENT_LEFT_MOUSE_UP, point)
                    try:
                        if emergency_up:
                            self._cg.CGEventPost(_CG_HID_EVENT_TAP, emergency_up)
                    except Exception:
                        pass
                    finally:
                        self._safe_release(emergency_up)
            finally:
                self._safe_release(down)
                self._safe_release(up)
                self._pressed_buttons.discard(_CG_MOUSE_BUTTON_LEFT)

    def _post_unicode(self, text: str, cancellation: Any) -> None:
        for character in text:
            self._check_cancel(cancellation)
            encoded = character.encode("utf-16-le")
            units = len(encoded) // 2
            buffer = (ctypes.c_uint16 * units).from_buffer_copy(encoded)
            down: Optional[int] = None
            up: Optional[int] = None
            down_posted = False
            up_posted = False
            try:
                down = self._pointer_value(self._cg.CGEventCreateKeyboardEvent(None, 0, True))
                up = self._pointer_value(self._cg.CGEventCreateKeyboardEvent(None, 0, False))
                if down is None or up is None:
                    raise MacOSBackendError(
                        "input_unavailable", "CoreGraphics could not create Unicode input events."
                    )
                self._cg.CGEventKeyboardSetUnicodeString(
                    ctypes.c_void_p(down), units, buffer
                )
                self._cg.CGEventKeyboardSetUnicodeString(
                    ctypes.c_void_p(up), units, buffer
                )
                down_posted = True
                self._cg.CGEventPost(_CG_HID_EVENT_TAP, ctypes.c_void_p(down))
                self._check_cancel(cancellation)
                self._cg.CGEventPost(_CG_HID_EVENT_TAP, ctypes.c_void_p(up))
                up_posted = True
            finally:
                try:
                    if down_posted and not up_posted and down is not None:
                        emergency_up = self._pointer_value(
                            self._cg.CGEventCreateKeyboardEvent(None, 0, False)
                        )
                        try:
                            if emergency_up:
                                self._cg.CGEventKeyboardSetUnicodeString(
                                    ctypes.c_void_p(emergency_up), units, buffer
                                )
                                self._cg.CGEventPost(
                                    _CG_HID_EVENT_TAP, ctypes.c_void_p(emergency_up)
                                )
                        except Exception:
                            pass
                        finally:
                            self._safe_release(emergency_up)
                finally:
                    self._safe_release(down)
                    self._safe_release(up)

    @staticmethod
    def _native_integer(value: Any, *, minimum: int, maximum: int) -> int:
        if type(value) is not int and type(value) is not float:
            raise ValueError("native integer was not numeric")
        try:
            numeric = float(value)
        except (TypeError, ValueError, OverflowError) as error:
            raise ValueError("native integer was not finite and integral") from error
        if not math.isfinite(numeric) or not numeric.is_integer():
            raise ValueError("native integer was not finite and integral")
        result = int(numeric)
        if not minimum <= result <= maximum:
            raise ValueError("native integer was outside its bounded range")
        return result

    def _process_metadata(self, pid: int) -> Tuple[str, Optional[str], bool]:
        try:
            cls = self._objc_get_class(b"NSRunningApplication")
            selector = self._sel_register(b"runningApplicationWithProcessIdentifier:")
            app = self._objc_msg_i32(cls, selector, pid)
            app_ref = self._pointer_value(app)
            if app_ref is None:
                return "", None, False
            bundle_ref = self._objc_msg0(app_ref, self._sel_register(b"bundleIdentifier"))
            name_ref = self._objc_msg0(app_ref, self._sel_register(b"localizedName"))
            launch_date = self._objc_msg0(app_ref, self._sel_register(b"launchDate"))
            token: Optional[str] = None
            if launch_date:
                launch_seconds = self._objc_msg_double(
                    launch_date, self._sel_register(b"timeIntervalSince1970")
                )
                if math.isfinite(launch_seconds) and launch_seconds > 0:
                    token = "launch:" + format(launch_seconds, ".17g")
            active = bool(self._objc_msg_bool(app_ref, self._sel_register(b"isActive")))
            return self._cf_string_value(self._pointer_value(bundle_ref)), token, active
        except (AttributeError, OSError, TypeError, ValueError, MacOSBackendError):
            return "", None, False

    def _cf_string_value(self, ref: Optional[int]) -> str:
        ref = self._pointer_value(ref)
        if ref is None:
            return ""
        if self._cf.CFGetTypeID(ctypes.c_void_p(ref)) != self._cf.CFStringGetTypeID():
            return ""
        try:
            length = int(self._cf.CFStringGetLength(ctypes.c_void_p(ref)))
        except (TypeError, ValueError, OverflowError):
            return ""
        if length < 0:
            return ""
        # CFStringGetLength counts UTF-16 code units. Use a byte-sized output
        # buffer so a hostile or malformed native string cannot scale the
        # allocation by a multibyte encoding factor.
        buffer = ctypes.create_string_buffer(MAX_TEXT_BYTES + 1)
        if not self._cf.CFStringGetCString(
            ctypes.c_void_p(ref), buffer, len(buffer), _CF_STRING_ENCODING_UTF8
        ):
            return ""
        try:
            result = buffer.value.decode("utf-8", "strict")
        except UnicodeError:
            return ""
        return result[:MAX_TEXT_BYTES]

    def _cf_number_value(self, ref: int) -> Optional[float]:
        ref = self._pointer_value(ref)
        if ref is None:
            return None
        if self._cf.CFGetTypeID(ctypes.c_void_p(ref)) != self._cf.CFNumberGetTypeID():
            return None
        result = ctypes.c_double()
        if not self._cf.CFNumberGetValue(
            ctypes.c_void_p(ref), _CF_NUMBER_DOUBLE_TYPE, ctypes.byref(result)
        ):
            return None
        value = float(result.value)
        return value if math.isfinite(value) else None

    def _cf_value(self, ref: int, *, max_depth: int, depth: int = 0) -> Any:
        ref = self._pointer_value(ref)
        if ref is None or depth > max_depth:
            return None
        type_id = self._cf.CFGetTypeID(ctypes.c_void_p(ref))
        if type_id == self._cf.CFStringGetTypeID():
            return self._cf_string_value(ref)
        if type_id == self._cf.CFBooleanGetTypeID():
            return bool(self._cf.CFBooleanGetValue(ctypes.c_void_p(ref)))
        if type_id == self._cf.CFNumberGetTypeID():
            return self._cf_number_value(ref)
        if type_id == self._cf.CFArrayGetTypeID():
            count = min(512, max(0, int(self._cf.CFArrayGetCount(ctypes.c_void_p(ref)))))
            return [
                self._cf_value(pointer, max_depth=max_depth, depth=depth + 1)
                for item in (
                    self._cf.CFArrayGetValueAtIndex(ctypes.c_void_p(ref), index)
                    for index in range(count)
                )
                for pointer in (self._pointer_value(item),)
                if pointer is not None
            ]
        if type_id == self._cf.CFDictionaryGetTypeID():
            count = min(512, max(0, int(self._cf.CFDictionaryGetCount(ctypes.c_void_p(ref)))))
            if count == 0:
                return {}
            keys = (ctypes.c_void_p * count)()
            values = (ctypes.c_void_p * count)()
            self._cf.CFDictionaryGetKeysAndValues(ctypes.c_void_p(ref), keys, values)
            result: dict[str, Any] = {}
            for index in range(count):
                key_pointer = self._pointer_value(keys[index])
                key = self._cf_string_value(key_pointer)
                if key:
                    value_pointer = self._pointer_value(values[index])
                    result[key] = self._cf_value(
                        value_pointer, max_depth=max_depth, depth=depth + 1
                    ) if value_pointer is not None else None
            return result
        if type_id == self._cf.CFDataGetTypeID():
            length = min(4_096, max(0, int(self._cf.CFDataGetLength(ctypes.c_void_p(ref)))))
            pointer = self._cf.CFDataGetBytePtr(ctypes.c_void_p(ref))
            return ctypes.string_at(pointer, length) if pointer and length else b""
        return None

    def _copy_attribute(self, element: int, name: str) -> Tuple[int, Optional[int]]:
        result = ctypes.c_void_p()
        status = int(
            self._ax.AXUIElementCopyAttributeValue(
                ctypes.c_void_p(element), ctypes.c_void_p(self._key(name)), ctypes.byref(result)
            )
        )
        return status, self._pointer_value(result.value)

    def _attribute_string(self, element: int, name: str) -> str:
        status, value = self._copy_attribute(element, name)
        if status != _AX_SUCCESS or not value:
            self._safe_release(value)
            return ""
        try:
            return self._cf_string_value(value)
        finally:
            self._safe_release(value)

    def _attribute_bool(self, element: int, name: str, default: bool) -> bool:
        status, value = self._copy_attribute(element, name)
        if status != _AX_SUCCESS or not value:
            self._safe_release(value)
            return default
        try:
            type_id = self._cf.CFGetTypeID(ctypes.c_void_p(value))
            if type_id == self._cf.CFBooleanGetTypeID():
                return bool(self._cf.CFBooleanGetValue(ctypes.c_void_p(value)))
            number = self._cf_number_value(value)
            if number is None or not math.isfinite(number) or number not in (0.0, 1.0):
                return default
            return number == 1.0
        finally:
            self._safe_release(value)

    def _attribute_array(self, element: int, name: str) -> Optional[int]:
        status, value = self._copy_attribute(element, name)
        if status != _AX_SUCCESS or not value:
            self._safe_release(value)
            return None
        try:
            if self._cf.CFGetTypeID(ctypes.c_void_p(value)) != self._cf.CFArrayGetTypeID():
                self._safe_release(value)
                return None
            return value
        except Exception:
            self._safe_release(value)
            raise

    def _ax_value_point(self, value: int, value_type: int) -> Optional[Tuple[float, float]]:
        if int(self._ax.AXValueGetType(ctypes.c_void_p(value))) != value_type:
            return None
        if value_type == _AX_VALUE_CG_POINT:
            result = _CGPoint()
            expected = (result.x, result.y)
        else:
            result = _CGSize()
            expected = (result.width, result.height)
        if not self._ax.AXValueGetValue(
            ctypes.c_void_p(value), value_type, ctypes.byref(result)
        ):
            return None
        values = (result.x, result.y) if value_type == _AX_VALUE_CG_POINT else (result.width, result.height)
        if any(not math.isfinite(float(item)) for item in values):
            return None
        return float(values[0]), float(values[1])

    def _element_geometry(self, element: int) -> Optional[WindowGeometry]:
        position: Optional[int] = None
        size: Optional[int] = None
        try:
            position_status, position = self._copy_attribute(element, "AXPosition")
            size_status, size = self._copy_attribute(element, "AXSize")
            if position_status != _AX_SUCCESS or size_status != _AX_SUCCESS or not position or not size:
                return None
            point = self._ax_value_point(position, _AX_VALUE_CG_POINT)
            dimensions = self._ax_value_point(size, _AX_VALUE_CG_SIZE)
            if point is None or dimensions is None:
                return None
            return WindowGeometry(point[0], point[1], dimensions[0], dimensions[1])
        except (TypeError, ValueError, OverflowError):
            return None
        finally:
            self._safe_release(position)
            self._safe_release(size)

    def _locate_ax_window(self, window: WindowSnapshot) -> Tuple[int, int, int]:
        app = self._ax.AXUIElementCreateApplication(window.identity.pid)
        app_ref = self._pointer_value(app)
        if app_ref is None:
            raise MacOSBackendError(
                "accessibility_denied",
                "The selected application cannot be inspected through Accessibility.",
            )
        ax_windows: Optional[int] = None
        try:
            ax_windows = self._attribute_array(app_ref, "AXWindows")
            if not ax_windows:
                raise MacOSBackendError(
                    "accessibility_denied",
                    "The selected application exposed no accessible windows.",
                )
            count = min(128, max(0, int(self._cf.CFArrayGetCount(ctypes.c_void_p(ax_windows)))))
            candidates: list[Tuple[float, int]] = []
            for index in range(count):
                element = self._cf.CFArrayGetValueAtIndex(ctypes.c_void_p(ax_windows), index)
                element_ref = self._pointer_value(element)
                if element_ref is None:
                    continue
                geometry = self._element_geometry(element_ref)
                if geometry is None:
                    continue
                title = self._attribute_string(element_ref, "AXTitle")
                distance = sum(
                    abs(float(left) - float(right))
                    for left, right in zip(
                        (geometry.x, geometry.y, geometry.width, geometry.height),
                        (window.geometry.x, window.geometry.y, window.geometry.width, window.geometry.height),
                    )
                )
                title_penalty = 0.0 if not window.title or title == window.title else 100_000.0
                if geometry.approximately_equals(window.geometry, tolerance=2.0) or title == window.title:
                    candidates.append((title_penalty + distance, element_ref))
            if not candidates:
                raise MacOSBackendError(
                    "accessibility_denied",
                    "The exact selected window could not be matched to an Accessibility window.",
                )
            candidates.sort(key=lambda item: item[0])
            return app_ref, ax_windows, candidates[0][1]
        except Exception:
            self._safe_release(ax_windows)
            self._safe_release(app_ref)
            raise

    def _walk_tree(self, root: int, *, max_nodes: int, max_depth: int) -> AccessibilityTree:
        nodes: list[AccessibilityNode] = []
        seen: set[int] = set()
        truncated = False

        def visit(element: int, path: Tuple[int, ...], depth: int) -> None:
            nonlocal truncated
            if len(nodes) >= max_nodes or depth > max_depth:
                truncated = True
                return
            if element in seen:
                return
            seen.add(element)
            role = self._attribute_string(element, "AXRole") or "AXUnknown"
            subrole = self._attribute_string(element, "AXSubrole")
            title = self._attribute_string(element, "AXTitle")
            description = self._attribute_string(element, "AXDescription")
            geometry = self._element_geometry(element)
            actions: list[str] = []
            action_array = self._attribute_array(element, "AXActions")
            if action_array:
                try:
                    count = min(32, max(0, int(self._cf.CFArrayGetCount(ctypes.c_void_p(action_array)))))
                    for index in range(count):
                        action_ref = self._cf.CFArrayGetValueAtIndex(ctypes.c_void_p(action_array), index)
                        action_pointer = self._pointer_value(action_ref)
                        action = self._cf_string_value(action_pointer)
                        if action and len(action.encode("utf-8")) <= 96:
                            actions.append(action)
                finally:
                    self._safe_release(action_array)
            settable = ctypes.c_bool(False)
            settable_status = self._ax.AXUIElementIsAttributeSettable(
                ctypes.c_void_p(element), ctypes.c_void_p(self._key("AXValue")), ctypes.byref(settable)
            )
            editable = bool(settable_status == _AX_SUCCESS and settable.value)
            editable = editable or any(
                marker in role.lower() for marker in ("textfield", "textarea", "searchfield", "combobox")
            )
            children: list[str] = []
            child_array = self._attribute_array(element, "AXChildren")
            child_elements: list[Tuple[int, Tuple[int, ...]]] = []
            if child_array:
                try:
                    count = min(512, max(0, int(self._cf.CFArrayGetCount(ctypes.c_void_p(child_array)))))
                    for index in range(count):
                        child_ref = self._cf.CFArrayGetValueAtIndex(ctypes.c_void_p(child_array), index)
                        child_pointer = self._pointer_value(child_ref)
                        if child_pointer is not None:
                            child_path = path + (index,)
                            retained_child = self._retain(child_pointer)
                            if retained_child is None:
                                continue
                            child_elements.append((retained_child, child_path))
                            children.append(
                                "ax:"
                                + ".".join(str(part) for part in child_path)
                            )
                except Exception:
                    for child, _ in child_elements:
                        self._safe_release(child)
                    raise
                finally:
                    self._safe_release(child_array)
            processed_children = 0
            try:
                try:
                    node = AccessibilityNode(
                        path=path,
                        role=role if not subrole else role,
                        title=title,
                        description=description,
                        bounds=geometry,
                        enabled=self._attribute_bool(element, "AXEnabled", True),
                        focused=self._attribute_bool(element, "AXFocused", False),
                        selected=self._attribute_bool(element, "AXSelected", False),
                        editable=editable,
                        sensitive=is_sensitive_node(
                            type("NodeLabel", (), {
                                "sensitive": False,
                                "role": role + " " + subrole,
                                "title": title,
                                "description": description,
                            })()
                        ),
                        actions=tuple(actions),
                        children=tuple(children),
                    )
                except (TypeError, ValueError):
                    node = AccessibilityNode(path=path, role="AXUnknown", bounds=geometry)
                nodes.append(node)
                for child, child_path in child_elements:
                    try:
                        if len(nodes) >= max_nodes:
                            truncated = True
                        else:
                            visit(child, child_path, depth + 1)
                    finally:
                        self._safe_release(child)
                        processed_children += 1
            finally:
                for child, _ in child_elements[processed_children:]:
                    self._safe_release(child)

        visit(root, (), 0)
        return AccessibilityTree(tuple(nodes[:max_nodes]), truncated=truncated)

    def _resolve_element(self, root: int, path: Tuple[int, ...]) -> Optional[int]:
        if not isinstance(path, tuple) or len(path) > MAX_DEPTH:
            return None
        element = self._retain(root)
        if element is None:
            return None
        try:
            for index in path:
                if type(index) is not int or index < 0 or index > 65_535:
                    self._safe_release(element)
                    return None
                children = self._attribute_array(element, "AXChildren")
                if not children:
                    self._safe_release(element)
                    return None
                next_element: Optional[int] = None
                try:
                    raw_count = self._cf.CFArrayGetCount(ctypes.c_void_p(children))
                    if type(raw_count) is not int or raw_count < 0:
                        self._safe_release(element)
                        return None
                    count = min(512, raw_count)
                    if index >= count:
                        self._safe_release(element)
                        return None
                    child = self._cf.CFArrayGetValueAtIndex(ctypes.c_void_p(children), index)
                    child_pointer = self._pointer_value(child)
                    if child_pointer is None:
                        self._safe_release(element)
                        return None
                    next_element = self._retain(child_pointer)
                finally:
                    self._safe_release(children)
                self._safe_release(element)
                if next_element is None:
                    return None
                element = next_element
            return element
        except Exception:
            self._safe_release(element)
            raise

    def _element_actions(self, element: int) -> set[int]:
        actions: set[int] = set()
        action_array = self._attribute_array(element, "AXActions")
        if not action_array:
            return actions
        try:
            count = min(32, max(0, int(self._cf.CFArrayGetCount(ctypes.c_void_p(action_array)))))
            for index in range(count):
                ref = self._cf.CFArrayGetValueAtIndex(ctypes.c_void_p(action_array), index)
                ref_pointer = self._pointer_value(ref)
                if ref_pointer is not None and self._cf_string_value(ref_pointer) == "AXPress":
                    actions.add(self._key("AXPressAction"))
        finally:
            self._safe_release(action_array)
        return actions

    def _verify_element(
        self,
        element: int,
        expected_role: Optional[str],
        expected_title: Optional[str],
        expected_bounds: Optional[WindowGeometry],
    ) -> None:
        role = self._attribute_string(element, "AXRole")
        title = self._attribute_string(element, "AXTitle")
        if expected_role is not None and (not isinstance(expected_role, str) or role != expected_role):
            raise MacOSBackendError("stale_node", "The observed accessibility role changed; observe again.")
        if expected_title is not None and (not isinstance(expected_title, str) or title != expected_title):
            raise MacOSBackendError("stale_node", "The observed accessibility label changed; observe again.")
        if not self._attribute_bool(element, "AXEnabled", False):
            raise MacOSBackendError("node_disabled", "The observed accessibility node is disabled.")
        if expected_bounds is not None:
            current_bounds = self._element_geometry(element)
            if current_bounds is None or not current_bounds.approximately_equals(expected_bounds, tolerance=0.5):
                raise MacOSBackendError("stale_node", "The observed accessibility geometry changed; observe again.")
        if is_sensitive_node(
            type(
                "NodeLabel",
                (),
                {
                    "sensitive": False,
                    "role": role,
                    "title": title,
                    "description": self._attribute_string(element, "AXDescription"),
                },
            )()
        ):
            raise MacOSBackendError(
                "sensitive_input_refused",
                "Input actions on credential or protected controls are refused.",
            )

    @staticmethod
    def _check_cancel(cancellation: Any) -> None:
        if cancellation is None:
            return
        try:
            if callable(cancellation):
                result = cancellation()
            elif callable(getattr(cancellation, "is_cancelled", None)):
                result = cancellation.is_cancelled()
            else:
                result = getattr(cancellation, "cancelled", False)
            if type(result) is not bool:
                raise ValueError("cancellation state is not a boolean")
        except Exception as error:
            raise MacOSBackendError(
                "cancelled", "The cancellation state could not be trusted; input was stopped."
            ) from error
        if result:
            raise MacOSBackendError("cancelled", "The native operation was cancelled safely.")


# Names used by callers that prefer the backend terminology.
NativeMacOS = MacOSNative


__all__ = ["MacOSNative", "NativeMacOS"]
